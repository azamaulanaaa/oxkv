//! Validation and reactivity hooks for stores.
//!
//! This module provides [`HookStore`]. This decorator wraps any backend that
//! implements [`Store`]. It adds two capabilities.
//!
//! - **Validation**. [`Validator`] implementations run before a value is
//!   written. A validator rejects the write when `validate` returns an error.
//! - **Reactivity**. Each change is broadcast after it becomes durable. The
//!   broadcast reaches registered [`Observer`] implementations or the channel
//!   receivers from [`watch`](HookStore::watch).
//!
//! Both hooks support key scoping through [`Scope`]. A validator or an
//! observer attaches to a single key, to all keys that share a prefix, or to
//! the entire store.
//!
//! # Delivery guarantees
//!
//! - Watch channels are bounded. A consumer that falls behind misses events.
//!   Each event carries a monotonic `seq` number. A gap in the `seq`
//!   numbers that a watcher receives shows the lag. Such a consumer never
//!   stalls writers.
//! - Observers run concurrently with each other. The write method awaits
//!   their completion before the write returns. Use the channel-based
//!   [`watch`](HookStore::watch) API for fire-and-forget reactivity.
//! - The [`HookStore`] snapshots the validators when a transaction begins.
//!   Validators that are added afterwards do not affect open transactions.
//! - Cloning a store shares the validators and the subscribers between the
//!   clones. An event from either clone reaches every watcher. A validator
//!   added to one clone applies to every clone.
//! - A delete reads the current value before it removes the value, so that
//!   [`ChangeEvent`]s can carry `old_value`. The reported old value may be
//!   stale if another writer bypasses this decorator and mutates the key
//!   between that read and the delete.
//! - Every hook receives a read-only [`StoreView`]. Inside a transaction, the
//!   view reflects the transaction's own staged writes. A hook therefore
//!   reads its own writes. Observers receive a view of the committed state as
//!   of after the change. The view exposes no write methods. Therefore a hook
//!   cannot mutate the store. Therefore a hook cannot trigger further events
//!   recursively.
//! - Transactional writes are re-validated at commit time. A decision made at
//!   staging time cannot be invalidated by later writes within the same
//!   transaction. During this pass, a validator sees the final
//!   post-transaction value of every key. The key that the validator
//!   currently validates is an exception. That key shows its pre-transaction
//!   value instead. This exception keeps absence-based rules from observing
//!   the staged write itself. Backends used with [`HookStore`] must implement
//!   `Clone`. Both shipped backends do.
//! - Cross-transaction ordering depends on the backend. `redb` serializes
//!   writers natively, so notifications always match the durability order
//!   there. With multiple concurrent transactions on other backends, the
//!   notification order follows the commit-call order. It is not a global
//!   serialization.
//!
//! # Example
//!
//! ```rust
//! use oxkv::{BTreeStore, GetSet, Hooks, Scope, StoreView, Validator};
//!
//! struct RequireJson(Scope);
//!
//! #[async_trait::async_trait]
//! impl Validator for RequireJson {
//!     fn scope(&self) -> Scope {
//!         self.0.clone()
//!     }
//!
//!     async fn validate(&self, _ctx: &dyn oxkv::StoreView, key: &str, value: &[u8])
//!         -> oxkv::Result<()>
//!     {
//!         serde_json::from_slice::<serde_json::Value>(value)
//!             .map(|_| ())
//!             .map_err(|e| format!("key `{key}` requires JSON: {e}").into())
//!     }
//! }
//!
//! #[tokio::main(flavor = "current_thread")]
//! async fn main() {
//!     let store = BTreeStore::default()
//!         .with_validator(RequireJson(Scope::Prefix("user:".into())));
//!
//!     let mut rx = store.watch("user:42");
//!
//!     // Rejected: not valid JSON
//!     assert!(store.set_bytes("user:42", b"nope").await.is_err());
//!
//!     // Accepted: subscribers observe the change
//!     store.set_bytes("user:42", br#"{"ok":true}"#).await.unwrap();
//!     let event = rx.try_recv().unwrap();
//!     assert_eq!(event.key, "user:42");
//!     assert_eq!(event.kind, oxkv::ChangeKind::Set);
//!     assert_eq!(event.old_value, None);
//! }
//! ```
//!
//! Hooks may read freely through the [`StoreView`] that they are given. They
//! must never call the owning [`HookStore`] directly. Hook calls happen while
//! writes are in progress. Reentrant store calls can deadlock or
//! double-notify. Treat hooks as side-effect-free beyond reads through the
//! view. Validators run more than once per transactional write, at staging
//! time and again at commit-time re-validation. Therefore validators must be
//! idempotent.

use std::sync::Arc;
use std::collections::VecDeque;

use async_trait::async_trait;
use futures::channel::mpsc;

use super::{Direction, GetSet, KeyValue, Result, lock_ignore_poison};

/// Selects which keys a validator or observer applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Applies to every key in the store.
    All,
    /// Applies only to the exact key held by this variant.
    Exact(String),
    /// Applies to every key starting with the prefix held by this variant.
    Prefix(String),
}

impl Scope {
    /// Returns `true` when this scope covers the given key.
    #[must_use]
    pub fn matches(&self, key: &str) -> bool {
        match self {
            Scope::All => true,
            Scope::Exact(exact) => exact == key,
            Scope::Prefix(prefix) => key.starts_with(prefix.as_str()),
        }
    }
}

/// The kind of change that produced a [`ChangeEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A key was inserted or a key was updated.
    Set,
    /// A key was removed.
    Delete,
}

/// A notification describing a single durable change to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEvent {
    /// The key that changed.
    pub key: String,
    /// Whether the key was set or deleted.
    pub kind: ChangeKind,
    /// The value before the change, when it could be observed.
    pub old_value: Option<Vec<u8>>,
    /// The value after the change. The value is `None` for a delete.
    pub new_value: Option<Vec<u8>>,
    /// Monotonic sequence number assigned when the event is published.
    ///
    /// A gap in the sequence numbers that a watcher receives shows that
    /// the watcher lagged and missed events.
    pub seq: u64,
}

/// A read-only view of the store, handed to hooks so they can inspect state.
///
/// Inside a transaction, the view reflects the transaction's own staged
/// writes, so a hook reads its own writes. Outside a transaction, the view
/// reflects the committed state. A hook cannot mutate the store through this
/// view. This restriction keeps validation side-effect-free. It also prevents
/// observers from triggering further change events recursively.
#[async_trait]
pub trait StoreView: Send + Sync {
    /// Retrieves the committed-or-staged value associated with the given key.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the underlying storage fails.
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Checks whether the given key is visible in this view.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the underlying storage fails.
    async fn has(&self, key: &str) -> Result<bool>;

    /// Retrieves the committed-or-staged value as raw bytes.
    ///
    /// This method mirrors [`GetSet::get_bytes`]. It exists so a view reads
    /// like a store.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the underlying storage fails.
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get(key).await
    }

    /// Retrieves multiple key-value pairs with cursor-based pagination.
    ///
    /// This method mirrors [`GetSet::gets_bytes`]. It lets a hook scan a
    /// prefix or a range without calling back into the owning store.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the underlying storage fails.
    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>>;

    /// Retrieves JSON documents with cursor-based pagination. A query string
    /// can filter the documents.
    ///
    /// This method mirrors [`GetSetExt::gets`]. It runs the query engine
    /// against the committed-or-staged state visible through this view.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the query is invalid or
    /// retrieval fails.
    async fn gets(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
        query: Option<&str>,
    ) -> Result<Vec<KeyValue>> {
        const BATCH_SIZE: u32 = 256;

        let Some(query) = query else {
            return self.gets_bytes(limit, direction, cursor).await;
        };

        let max_results = limit.map_or(usize::MAX, |l| usize::try_from(l).unwrap_or(usize::MAX));
        if max_results == 0 {
            return Ok(Vec::new());
        }
        let ast = crate::query::parse(query).map_err(super::StoreError::Other)?;
        let mut results = Vec::new();
        let mut page_cursor: Option<String> = cursor.0.clone();

        loop {
            let batch = self
                .gets_bytes(
                    Some(BATCH_SIZE),
                    direction,
                    (page_cursor.clone(), cursor.1.clone()),
                )
                .await?;

            if batch.is_empty() {
                break;
            }

            let last_key = batch.last().map(|kv| kv.key.clone());
            let is_last_batch = batch.len() < BATCH_SIZE as usize;

            for kv in batch {
                if page_cursor.as_deref() == Some(kv.key.as_str()) {
                    continue;
                }

                let matches = serde_json::from_slice::<serde_json::Value>(&kv.value)
                    .is_ok_and(|value| crate::query::eval(&ast, &value));
                if matches {
                    results.push(kv);
                    if results.len() >= max_results {
                        break;
                    }
                }
            }

            if is_last_batch || results.len() >= max_results {
                break;
            }
            page_cursor = last_key;
        }

        Ok(results)
    }

    /// Streams the store's serialized form as byte chunks.
    ///
    /// This method mirrors [`StoreExt::save_stream`]. It lets a hook export a
    /// prefix-less snapshot without calling back into the owning store.
    ///
    /// # Errors
    ///
    /// Storage failures surface while the stream is polled.
    fn save_stream(&self) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>>> + Send + '_>>;
}

#[async_trait]
impl<S: GetSet + Send + Sync> StoreView for S {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        GetSet::get_bytes(self, key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        GetSet::has(self, key).await
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        GetSet::gets_bytes(self, limit, direction, cursor).await
    }

    fn save_stream(&self) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>>> + Send + '_>> {
        Box::pin(crate::store::SaveStream::new(self))
    }
}

/// Checks a value before it is written to the underlying store.
///
/// A validator runs before every write whose key matches its
/// [`scope`](Validator::scope). The write is standalone or transactional.
/// Register the validator on a [`HookStore`] with
/// [`with_validator`](HookStore::with_validator). An error aborts the write without
/// touching the underlying store. Transactional writes are re-validated at
/// commit time against the transaction's final state. A decision made at
/// staging time cannot be invalidated by later writes within the same
/// transaction.
#[async_trait]
pub trait Validator: Send + Sync {
    /// Which keys this validator applies to. Defaults to [`Scope::All`].
    fn scope(&self) -> Scope {
        Scope::All
    }

    /// Validates the pending write of `value` under `key`.
    ///
    /// The implementation may use the [`StoreView`] to compare the pending
    /// write against other keys. Inside a transaction, the view observes the
    /// transaction's own staged writes.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) to reject the write. The error goes
    /// to the caller of the mutating method. During the commit-time re-validation pass,
    /// the error goes to `commit`.
    async fn validate(&self, ctx: &dyn StoreView, key: &str, value: &[u8]) -> Result<()>;
}

/// Receives notifications about durable changes to the store.
///
/// Register an observer on a [`HookStore`] with
/// [`add_observer`](HookStore::add_observer). The hook store invokes the
/// observer once per committed change whose key matches its
/// [`scope`](Observer::scope). Use [`watch`](HookStore::watch) for
/// channel-based consumption. That API needs no trait implementation.
#[async_trait]
pub trait Observer: Send + Sync {
    /// Which keys this observer applies to. Defaults to [`Scope::All`].
    fn scope(&self) -> Scope {
        Scope::All
    }

    /// Called once for every matching committed change.
    ///
    /// The [`StoreView`] reflects the committed state as of after the change.
    /// An observer may inspect related keys through this view without racing
    /// the write.
    ///
    /// Observers must not call back into the same store (see the
    /// module documentation about reentrancy).
    async fn on_change(&self, ctx: &dyn StoreView, event: &ChangeEvent);
}

#[derive(Default)]
pub(crate) struct Subscribers {
    observers: Vec<(Scope, Arc<dyn Observer>)>,
    senders: Vec<(Scope, mpsc::Sender<ChangeEvent>)>,
    seq: std::sync::atomic::AtomicU64,
    history: VecDeque<ChangeEvent>,
}

/// How many events may queue for a watcher before it starts missing them.
const WATCH_CAPACITY: usize = 256;

/// How many recently published events the store keeps for lag recovery.
const EVENT_HISTORY_CAPACITY: usize = 1024;

impl Subscribers {
    fn push_sender(&mut self, scope: Scope) -> mpsc::Receiver<ChangeEvent> {
        let (tx, rx) = mpsc::channel(WATCH_CAPACITY);
        self.senders.push((scope, tx));
        rx
    }
}

/// Delivers a change event to every matching subscriber.
///
/// The function prunes closed channels while it delivers. It skips the event
/// on a full channel, so a slow consumer can never stall a write. The lock
/// releases before the observer calls start. The observer calls then run
/// concurrently, so observers may take arbitrarily long relative to each
/// other.
pub(crate) async fn notify(subscribers: &std::sync::Arc<std::sync::Mutex<Subscribers>>, view: &dyn StoreView, event: ChangeEvent) {
    let mut event = event;
    let seq = {
        let subs = lock_ignore_poison(subscribers.as_ref());
        subs.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    };
    event.seq = seq;

    {
        let mut subs = lock_ignore_poison(subscribers.as_ref());
        subs.history.push_back(event.clone());
        while subs.history.len() > EVENT_HISTORY_CAPACITY {
            subs.history.pop_front();
        }
    }

    let observers = {
        let mut subs = lock_ignore_poison(subscribers.as_ref());

        let mut i = 0;
        while i < subs.senders.len() {
            let (scope, tx) = &mut subs.senders[i];
            let send_failed_disconnected = scope.matches(&event.key)
                && tx
                    .try_send(event.clone())
                    .err()
                    .is_some_and(|err| err.is_disconnected());
            if send_failed_disconnected {
                subs.senders.swap_remove(i);
                continue;
            }
            i += 1;
        }

        subs.observers
            .iter()
            .filter(|(scope, _)| scope.matches(&event.key))
            .map(|(_, observer)| Arc::clone(observer))
            .collect::<Vec<_>>()
    };

    futures::future::join_all(
        observers
            .iter()
            .map(|observer| observer.on_change(view, &event)),
    )
    .await;
}

/// This view supports the commit-time re-validation pass. It shows the
/// transaction's final state for every key. The key that the pass currently
/// validates is an exception. That key shows its pre-transaction value
/// instead. Without this exception, an absence-based rule (e.g. "must not
/// overwrite") sees the staged write itself. Such a rule falsely rejects
/// legitimate inserts.
pub(crate) struct RevalidateView<'a> {
    pub(crate) inner: &'a dyn StoreView,
    pub(crate) key: &'a str,
    pub(crate) old_value: Option<&'a [u8]>,
}

#[async_trait]
impl StoreView for RevalidateView<'_> {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if key == self.key {
            Ok(self.old_value.map(<[u8]>::to_vec))
        } else {
            self.inner.get(key).await
        }
    }

    async fn has(&self, key: &str) -> Result<bool> {
        if key == self.key {
            Ok(self.old_value.is_some())
        } else {
            self.inner.has(key).await
        }
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.inner.gets_bytes(limit, direction, cursor).await
    }

    fn save_stream(&self) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>>> + Send + '_>> {
        self.inner.save_stream()
    }
}

pub(crate) fn validate_key<'a>(
    validators: &'a [Arc<dyn Validator>],
    view: &'a dyn StoreView,
    key: &'a str,
    value: &'a [u8],
) -> futures::future::BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        for validator in validators {
            if validator.scope().matches(key) {
                validator.validate(view, key, value).await?;
            }
        }
        Ok(())
    })
}

/// Shared hook state for a store.
///
/// The state holds the validators and the subscribers. Clones of a store
/// share this state.
#[derive(Clone, Default)]
pub struct HookState {
    validators: std::sync::Arc<std::sync::Mutex<Vec<std::sync::Arc<dyn Validator>>>>,
    subscribers: std::sync::Arc<std::sync::Mutex<Subscribers>>,
}

impl HookState {
    /// Creates empty hook state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a validator.
    pub fn add_validator<V>(&self, validator: V)
    where
        V: Validator + 'static,
    {
        lock_ignore_poison(&self.validators).push(std::sync::Arc::new(validator));
    }

    /// Returns a snapshot of the validators.
    #[must_use]
    pub fn validators(&self) -> Vec<std::sync::Arc<dyn Validator>> {
        lock_ignore_poison(&self.validators).clone()
    }

    /// Registers an observer.
    pub fn add_observer<O>(&self, observer: O)
    where
        O: Observer + 'static,
    {
        let scope = observer.scope();
        lock_ignore_poison(&self.subscribers)
            .observers
            .push((scope, std::sync::Arc::new(observer)));
    }

    /// Returns the subscribers.
    #[must_use]
    pub(crate) fn subscribers(&self) -> std::sync::Arc<std::sync::Mutex<Subscribers>> {
        std::sync::Arc::clone(&self.subscribers)
    }

    /// Subscribes with the given scope.
    pub fn watch_scope(&self, scope: Scope) -> mpsc::Receiver<ChangeEvent> {
        lock_ignore_poison(&self.subscribers).push_sender(scope)
    }

    /// Returns the events published after `seq`, when they are still retained.
    ///
    /// Returns `None` when the requested range fell out of the retained
    /// history. The caller must then re-read the store to recover.
    pub fn events_since(&self, seq: u64) -> Option<Vec<ChangeEvent>> {
        let subs = lock_ignore_poison(&self.subscribers);
        let current = subs.seq.load(std::sync::atomic::Ordering::SeqCst);
        if seq >= current {
            return Some(Vec::new());
        }
        let first_available = match subs.history.front() {
            Some(event) => event.seq,
            None => return None,
        };
        if seq + 1 < first_available {
            return None;
        }
        Some(
            subs.history
                .iter()
                .filter(|event| event.seq > seq)
                .cloned()
                .collect(),
        )
    }

    /// Returns the sequence number of the most recently published event.
    ///
    /// A watcher that detects a gap in the `seq` numbers it receives can
    /// compare its position with this value. The comparison shows how far
    /// behind the watcher is. The watcher can then re-read the affected keys
    /// to recover.
    pub fn current_seq(&self) -> u64 {
        lock_ignore_poison(&self.subscribers).seq.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Hook capabilities for a store.
#[async_trait]
pub trait Hooks: Send + Sync {
    /// Registers a validator.
    fn add_validator<V>(&self, validator: V)
    where
        V: Validator + 'static;

    /// Registers an observer.
    fn add_observer<O>(&self, observer: O)
    where
        O: Observer + 'static;

    /// Subscribes to an exact key.
    fn watch(&self, key: &str) -> mpsc::Receiver<ChangeEvent> {
        self.watch_scope(Scope::Exact(key.to_string()))
    }

    /// Subscribes to a prefix.
    fn watch_prefix(&self, prefix: &str) -> mpsc::Receiver<ChangeEvent> {
        self.watch_scope(Scope::Prefix(prefix.to_string()))
    }

    /// Subscribes to all keys.
    fn watch_all(&self) -> mpsc::Receiver<ChangeEvent> {
        self.watch_scope(Scope::All)
    }

    /// Subscribes with a scope.
    fn watch_scope(&self, scope: Scope) -> mpsc::Receiver<ChangeEvent>;

    /// Returns the sequence number of the most recently published event.
    ///
    /// A watcher that detects a gap in the `seq` numbers it receives can use
    /// this value to measure how far behind it is. The watcher can then
    /// re-read the affected keys to recover.
    fn current_seq(&self) -> u64;

    /// Returns the events published after `seq`, when they are still retained.
    ///
    /// Returns `None` when the requested range fell out of the retained
    /// history. The caller must then re-read the store to recover.
    fn events_since(&self, seq: u64) -> Option<Vec<ChangeEvent>>;

    /// Registers a validator and returns self.
    fn with_validator<V>(self, validator: V) -> Self
    where
        Self: Sized,
        V: Validator + 'static,
    {
        self.add_validator(validator);
        self
    }
}


#[cfg(all(test, feature = "btree"))]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::store::BTreeStore;
    use crate::store::Store;
    use crate::store::Transaction;
    use crate::store::StoreError;

    struct Rejecting(Scope);

    #[async_trait]
    impl Validator for Rejecting {
        fn scope(&self) -> Scope {
            self.0.clone()
        }

        async fn validate(&self, _ctx: &dyn StoreView, key: &str, _value: &[u8]) -> Result<()> {
            Err(StoreError::Rejected {
                key: key.to_string(),
                reason: "rejected".to_string(),
            })
        }
    }

    struct RequireJson(Scope);

    #[async_trait]
    impl Validator for RequireJson {
        fn scope(&self) -> Scope {
            self.0.clone()
        }

        async fn validate(&self, _ctx: &dyn StoreView, key: &str, value: &[u8]) -> Result<()> {
            serde_json::from_slice::<serde_json::Value>(value)
                .map(|_| ())
                .map_err(|_| StoreError::Rejected {
                    key: key.to_string(),
                    reason: format!("key `{key}` requires JSON"),
                })
        }
    }

    struct Collector(Arc<Mutex<Vec<ChangeEvent>>>);

    #[async_trait]
    impl Observer for Collector {
        fn scope(&self) -> Scope {
            Scope::Prefix("user:".into())
        }

        async fn on_change(&self, _ctx: &dyn StoreView, event: &ChangeEvent) {
            lock_ignore_poison(&self.0).push(event.clone());
        }
    }

    type StoreUnderTest = BTreeStore;

    fn store() -> StoreUnderTest {
        BTreeStore::default()
    }

    #[tokio::test]
    async fn test_a_rejecting_validator_blocks_the_write() {
        let s = store().with_validator(Rejecting(Scope::All));

        assert!(s.set_bytes("k", b"v").await.is_err());
        assert_eq!(s.get("k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_a_validator_applies_only_inside_its_scope() {
        let s = store().with_validator(Rejecting(Scope::Exact(String::from("locked"))));

        assert!(s.set_bytes("locked", b"v").await.is_err());
        assert!(s.set_bytes("free", b"v").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_a_json_validator_accepts_a_json_document_and_rejects_invalid_bytes() {
        let s = store().with_validator(RequireJson(Scope::All));

        assert!(s.set_bytes("doc", br#"{"ok":true}"#).await.is_ok());
        assert!(s.set_bytes("raw", b"\x00\xff").await.is_err());
    }

    #[tokio::test]
    async fn test_a_watcher_receives_the_kind_of_each_change() {
        let s = store();
        let mut rx = s.watch("k");

        s.set_bytes("k", b"v").await.unwrap();
        let event = rx.try_recv().unwrap();
        assert_eq!(event.key, "k");
        assert_eq!(event.kind, ChangeKind::Set);

        s.delete("k").await.unwrap();
        let event = rx.try_recv().unwrap();
        assert_eq!(event.kind, ChangeKind::Delete);
    }

    #[tokio::test]
    async fn test_a_set_event_carries_the_previous_value_and_a_delete_event_has_no_new_value() {
        let s = store();
        let mut rx = s.watch_all();

        s.set_bytes("k", b"one").await.unwrap();
        let event = rx.try_recv().unwrap();
        assert_eq!(event.old_value, None);
        assert_eq!(event.new_value, Some(b"one".to_vec()));

        s.set_bytes("k", b"two").await.unwrap();
        let event = rx.try_recv().unwrap();
        assert_eq!(event.old_value, Some(b"one".to_vec()));
        assert_eq!(event.new_value, Some(b"two".to_vec()));

        s.delete("k").await.unwrap();
        let event = rx.try_recv().unwrap();
        assert_eq!(event.old_value, Some(b"two".to_vec()));
        assert_eq!(event.new_value, None);
    }

    #[tokio::test]
    async fn test_a_transaction_event_reports_only_the_last_staged_value() {
        let s = store();
        s.set_bytes("a", b"original").await.unwrap();
        let mut rx = s.watch("a");

        let tx = s.begin_tx().unwrap();
        tx.set_bytes("a", b"first").await.unwrap();
        tx.set_bytes("a", b"second").await.unwrap();
        tx.commit().await.unwrap();

        let event = rx.try_recv().unwrap();
        assert_eq!(event.old_value, Some(b"original".to_vec()));
        assert_eq!(event.new_value, Some(b"second".to_vec()));
    }

    #[tokio::test]
    async fn test_a_slow_watcher_does_not_block_writes() {
        let s = store();
        let mut rx = s.watch_all();

        for i in 0..(WATCH_CAPACITY * 2) {
            assert!(s.set_bytes("k", format!("{i}").as_bytes()).await.is_ok());
        }

        let mut received = 0;
        while rx.try_recv().is_ok() {
            received += 1;
        }
        assert!(received >= 1);
        assert!(received <= WATCH_CAPACITY + 1);
        assert!(received < WATCH_CAPACITY * 2);
    }

    #[tokio::test]
    async fn test_deleting_a_missing_key_returns_false_and_sends_no_event() {
        let s = store();
        let mut rx = s.watch_all();

        assert!(!s.delete("missing").await.unwrap());
        assert!(rx.try_recv().unwrap_err().is_empty());
    }

    #[tokio::test]
    async fn test_a_prefix_watcher_receives_events_only_for_keys_with_the_prefix() {
        let s = store();
        let mut rx = s.watch_prefix("user:");

        s.set_bytes("other", b"v").await.unwrap();
        assert!(rx.try_recv().unwrap_err().is_empty());

        s.set_bytes("user:1", b"v").await.unwrap();
        assert_eq!(rx.try_recv().unwrap().key, "user:1");
    }

    #[tokio::test]
    async fn test_a_write_succeeds_after_the_watcher_receiver_is_dropped() {
        let s = store();
        drop(s.watch_all());

        assert!(s.set_bytes("k", b"v").await.is_ok());
    }

    #[tokio::test]
    async fn test_an_observer_receives_only_events_inside_its_scope() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut s = store();
        s.add_observer(Collector(Arc::clone(&log)));

        s.set_bytes("user:1", b"v").await.unwrap();
        s.delete("user:1").await.unwrap();
        s.set_bytes("admin:1", b"v").await.unwrap();

        let events = log.lock().unwrap();
        assert_eq!(
            *events,
            vec![
                ChangeEvent {
                    key: "user:1".into(),
                    kind: ChangeKind::Set,
                    old_value: None,
                    new_value: Some(b"v".to_vec()),
                    seq: 0,
                },
                ChangeEvent {
                    key: "user:1".into(),
                    kind: ChangeKind::Delete,
                    old_value: Some(b"v".to_vec()),
                    new_value: None,
                    seq: 1,
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_a_commit_notifies_each_changed_key_exactly_once() {
        let s = store();
        let mut rx = s.watch_all();

        let tx = s.begin_tx().unwrap();
        tx.set_bytes("a", b"1").await.unwrap();
        tx.set_bytes("a", b"2").await.unwrap();
        tx.set_bytes("b", b"2").await.unwrap();
        tx.commit().await.unwrap();

        let mut keys: Vec<(String, ChangeKind)> = Vec::new();
        while let Ok(event) = rx.try_recv() {
            keys.push((event.key, event.kind));
        }
        assert_eq!(
            keys,
            vec![
                ("a".to_string(), ChangeKind::Set),
                ("b".to_string(), ChangeKind::Set),
            ]
        );
    }

    #[tokio::test]
    async fn test_a_rollback_sends_no_event_and_leaves_the_key_absent() {
        let s = store();
        let mut rx = s.watch_all();

        let tx = s.begin_tx().unwrap();
        tx.set_bytes("a", b"1").await.unwrap();
        tx.rollback().await.unwrap();

        assert!(rx.try_recv().unwrap_err().is_empty());
        assert_eq!(s.get("a").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_a_validator_rejects_a_transactional_write_at_staging_time() {
        let s = store().with_validator(Rejecting(Scope::Exact(String::from("bad"))));
        let tx = s.begin_tx().unwrap();

        assert!(tx.set_bytes("bad", b"v").await.is_err());

        tx.set_bytes("good", b"v").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(s.get("good").await.unwrap(), Some(b"v".to_vec()));
    }

    struct NoOverwrite(Scope);

    #[async_trait]
    impl Validator for NoOverwrite {
        fn scope(&self) -> Scope {
            self.0.clone()
        }

        async fn validate(&self, ctx: &dyn StoreView, key: &str, _value: &[u8]) -> Result<()> {
            if ctx.has(key).await? {
                return Err(StoreError::Rejected {
                    key: key.to_string(),
                    reason: format!("key `{key}` must not be overwritten"),
                });
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_stage_time_validation_sees_the_staged_writes() {
        let s = store().with_validator(NoOverwrite(Scope::All));

        // Stage-time validation observes the transaction's own staged writes.
        let tx = s.begin_tx().unwrap();
        tx.set_bytes("k", b"v1").await.unwrap();
        assert!(tx.set_bytes("k", b"v2").await.is_err());
        tx.rollback().await.unwrap();

        // Standalone validation sees committed state.
        s.set_bytes("other", b"v").await.unwrap();
        assert!(s.set_bytes("other", b"v2").await.is_err());
    }

    struct OnlyWhileAbsent(String);

    #[async_trait]
    impl Validator for OnlyWhileAbsent {
        fn scope(&self) -> Scope {
            Scope::All
        }

        async fn validate(&self, ctx: &dyn StoreView, key: &str, _value: &[u8]) -> Result<()> {
            if ctx.has(&self.0).await? {
                return Err(StoreError::Rejected {
                    key: key.to_string(),
                    reason: format!("`{key}` may only be written while `{}` is absent", self.0),
                });
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_commit_fails_when_the_final_state_breaks_the_rule() {
        let s = store().with_validator(OnlyWhileAbsent(String::from("flag")));

        let tx = s.begin_tx().unwrap();
        // Passes at staging time: "flag" does not exist yet.
        tx.set_bytes("target", b"v").await.unwrap();
        // Creating "flag" inside the same tx invalidates "target" by commit.
        tx.set_bytes("flag", b"on").await.unwrap();

        assert!(tx.commit().await.is_err());
        // Re-validation failed before the inner transaction committed.
        // Nothing landed in the store.
        assert_eq!(s.get("target").await.unwrap(), None);
        assert_eq!(s.get("flag").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_commit_revalidation_does_not_reject_a_validated_insert() {
        // An absence-based rule must not see the staged write that the
        // pass validates. Otherwise every legitimate insert would be
        // rejected at commit time.
        let s = store().with_validator(NoOverwrite(Scope::All));

        let tx = s.begin_tx().unwrap();
        tx.set_bytes("fresh", b"v").await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(s.get("fresh").await.unwrap(), Some(b"v".to_vec()));
    }

    #[tokio::test]
    async fn test_a_transaction_set_then_delete_emits_one_delete_event() {
        let s = store();
        s.set_bytes("a", b"1").await.unwrap();
        let mut rx = s.watch("a");

        let tx = s.begin_tx().unwrap();
        tx.set_bytes("a", b"2").await.unwrap();
        tx.delete("a").await.unwrap();
        tx.commit().await.unwrap();

        let event = rx.try_recv().unwrap();
        assert_eq!(event.kind, ChangeKind::Delete);
        assert!(rx.try_recv().unwrap_err().is_empty());
    }

    #[tokio::test]
    async fn test_hook_store_supports_the_get_set_extension() {
        use crate::store::{Direction, GetSetExt};

        let s = store().with_validator(RequireJson(Scope::Prefix(String::from("doc:"))));

        s.set("doc:a", &serde_json::json!({ "lang": "rust", "stars": 10 }))
            .await
            .unwrap();
        assert!(s.set_bytes("doc:b", b"not json").await.is_err());

        let matches = GetSetExt::gets(
            &s,
            None,
            Direction::Next,
            (None, None),
            Some("lang:rust AND stars:[5 TO 50]"),
        )
        .await
        .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].key, "doc:a");
    }
}
