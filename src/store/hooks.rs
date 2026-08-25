//! Validation and reactivity hooks for stores.
//!
//! This module provides [`HookStore`], a decorator that wraps any backend
//! implementing [`Store`] and adds two capabilities:
//!
//! - **Validation** — [`Validator`] implementations run before a value is
//!   written; returning an error from `validate` rejects the write.
//! - **Reactivity** — changes are broadcast after they become durable, either
//!   to registered [`Observer`] implementations or to channel receivers
//!   obtained via [`watch`](HookStore::watch).
//!
//! Both hooks support key scoping through [`Scope`], so a validator or
//! observer can be attached to a single key, all keys sharing a prefix, or
//! the entire store.
//!
//! # Delivery guarantees
//!
//! - Watch channels are bounded: a consumer that falls behind misses events
//!   (oldest dropped per delivery attempt) but never stalls writers.
//! - Observers run concurrently with each other, but `await` their completion
//!   before the write returns. For fire-and-forget reactivity prefer the
//!   channel-based [`watch`](HookStore::watch) API.
//! - Validators registered on a [`HookStore`] are snapshotted when a
//!   transaction begins; validators added afterwards do not affect open
//!   transactions.
//! - Cloning a [`HookStore`] shares subscribers between the clones (events
//!   from either clone reach every watcher) while each clone keeps its own
//!   validator list.
//! - Deletes read the current value before removing it so [`ChangeEvent`]s
//!   can carry `old_value`; if another writer bypasses this decorator and
//!   mutates the key between that read and the delete, the reported old
//!   value may be stale.
//! - Every hook receives a read-only [`StoreView`]: inside a transaction it
//!   reflects the transaction's own staged writes (read-your-writes), while
//!   observers are handed a view of committed state as of after the change.
//!   The view exposes no write methods, so hooks cannot mutate the store or
//!   recursively trigger further events.
//! - Transactional writes are re-validated at commit time so a decision made
//!   at staging time cannot be invalidated by later writes within the same
//!   transaction. During this pass a validator sees every key's final
//!   post-transaction value *except* the key currently being validated,
//!   which shows its pre-transaction value instead - absence-based rules
//!   must not observe the staged write itself. Backends used with
//!   [`HookStore`] must implement `Clone` (both shipped backends do).
//! - Cross-transaction ordering depends on the backend: redb serializes
//!   writers natively, so notifications always match durability order there;
//!   with multiple concurrent transactions on other backends, notification
//!   order follows commit-call order, not a global serialization.
//!
//! # Example
//!
//! ```rust
//! use oxkv::{BTreeStore, GetSet, HookStore, Scope, StoreView, Validator};
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
//!     let mut store = HookStore::new(BTreeStore::default())
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
//! Hooks may read freely through the [`StoreView`] they are given; they
//! must never call back into the owning [`HookStore`] directly - hook calls
//! happen while writes are in progress, and reentrant store calls can
//! deadlock or double-notify. Beyond reads through the view, treat hooks as
//! side-effect-free: validators run more than once per transactional write
//! (staging plus commit-time re-validation), so they must be idempotent.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::channel::mpsc;

use super::{Direction, GetSet, KeyValue, Result, Store, Transaction};

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
    /// A key was inserted or updated.
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
    /// The value after the change; `None` for deletes.
    pub new_value: Option<Vec<u8>>,
}

/// A read-only view of the store, handed to hooks so they can inspect state.
///
/// Inside a transaction the view reflects the transaction's own staged
/// writes (read-your-writes); outside a transaction it reflects committed
/// state. Hooks cannot mutate the store through this view, which keeps
/// validation side-effect-free and prevents observers from recursively
/// triggering further change events.
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
}

#[async_trait]
impl<S: GetSet + Send + Sync> StoreView for S {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        GetSet::get_bytes(self, key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        GetSet::has(self, key).await
    }
}

/// Checks a value before it is written to the underlying store.
///
/// Validators are registered on a [`HookStore`] with
/// [`with_validator`](HookStore::with_validator) and run before every write
/// (standalone or transactional) whose key matches the validator's
/// [`scope`](Validator::scope). Returning an error aborts the write without
/// touching the underlying store. Transactional writes are re-validated at
/// commit time against the transaction's final state, so a decision made at
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
    /// The [`StoreView`] may be used to compare the pending write against
    /// other keys; inside a transaction it observes the transaction's own
    /// staged writes.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) to reject the write; the error is propagated
    /// to the caller of the mutating method (or to `commit`, during the
    /// commit-time re-validation pass).
    async fn validate(&self, ctx: &dyn StoreView, key: &str, value: &[u8]) -> Result<()>;
}

/// Receives notifications about durable changes to the store.
///
/// Observers are registered on a [`HookStore`] with
/// [`add_observer`](HookStore::add_observer) and invoked once per committed
/// change whose key matches their [`scope`](Observer::scope). For
/// channel-based consumption prefer [`watch`](HookStore::watch), which needs
/// no trait implementation.
#[async_trait]
pub trait Observer: Send + Sync {
    /// Which keys this observer applies to. Defaults to [`Scope::All`].
    fn scope(&self) -> Scope {
        Scope::All
    }

    /// Called once for every matching committed change.
    ///
    /// The [`StoreView`] reflects committed state as of after the change,
    /// so an observer may inspect related keys without racing the write.
    ///
    /// Observers must not call back into the same store (see the
    /// module documentation about reentrancy).
    async fn on_change(&self, ctx: &dyn StoreView, event: &ChangeEvent);
}

#[derive(Default)]
struct Subscribers {
    observers: Vec<(Scope, Arc<dyn Observer>)>,
    senders: Vec<(Scope, mpsc::Sender<ChangeEvent>)>,
}

/// How many events may queue for a watcher before it starts missing them.
const WATCH_CAPACITY: usize = 256;

impl Subscribers {
    fn push_sender(&mut self, scope: Scope) -> mpsc::Receiver<ChangeEvent> {
        let (tx, rx) = mpsc::channel(WATCH_CAPACITY);
        self.senders.push((scope, tx));
        rx
    }
}

/// Delivers a change event to every matching subscriber.
///
/// Closed channels are pruned while delivering; full channels skip the event
/// so a slow consumer can never stall a write. Observer calls happen after
/// the lock is released and run concurrently, so observers may take
/// arbitrarily long relative to each other.
async fn notify(subscribers: &Mutex<Subscribers>, view: &dyn StoreView, event: ChangeEvent) {
    let observers = {
        let mut subs = subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

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

/// A [`StoreView`] for the commit-time re-validation pass: shows the
/// transaction's final state for every key *except* the one currently being
/// validated, whose pre-transaction value is exposed instead. Without the
/// shadowing, absence-based rules (e.g. "must not overwrite") would see the
/// staged write itself and falsely reject legitimate inserts.
struct RevalidateView<'a> {
    inner: &'a dyn StoreView,
    key: &'a str,
    old_value: Option<&'a [u8]>,
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
}

fn validate_key<'a>(
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

/// A store decorator adding validation hooks and change notifications.
///
/// Wrapping any [`Store`] backend, it intercepts writes to run registered
/// [`Validator`]s first, then broadcasts [`ChangeEvent`]s after the change is
/// durable. Because it implements [`Store`] itself, it composes transparently
/// with the rest of the crate, including transactions and the extension
/// traits ([`GetSetExt`](super::GetSetExt), [`StoreExt`](super::StoreExt)).
#[derive(Clone)]
pub struct HookStore<S> {
    inner: S,
    validators: Vec<Arc<dyn Validator>>,
    subscribers: Arc<Mutex<Subscribers>>,
}

impl<S> HookStore<S> {
    /// Wraps `inner` with no validators and no subscribers.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            validators: Vec::new(),
            subscribers: Arc::new(Mutex::new(Subscribers::default())),
        }
    }

    /// Registers an additional validator, consuming and returning `self`.
    #[must_use]
    pub fn with_validator<V>(mut self, validator: V) -> Self
    where
        V: Validator + 'static,
    {
        self.add_validator(validator);
        self
    }

    /// Registers an additional validator in place.
    pub fn add_validator<V>(&mut self, validator: V)
    where
        V: Validator + 'static,
    {
        self.validators.push(Arc::new(validator));
    }

    /// Registers an observer that receives matching change events.
    ///
    /// Dropping the returned receiver unsubscribes a channel-based
    /// subscriber automatically; observers stay registered until the store
    /// is dropped.
    pub fn add_observer<O>(&mut self, observer: O)
    where
        O: Observer + 'static,
    {
        let scope = observer.scope();
        self.subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observers
            .push((scope, Arc::new(observer)));
    }

    /// Subscribes to changes of the exact key `key`.
    ///
    /// Returns the receiving half of an unbounded channel receiving one
    /// [`ChangeEvent`] per committed change to `key`. Unsubscribes when the
    /// receiver is dropped.
    pub fn watch(&self, key: &str) -> mpsc::Receiver<ChangeEvent> {
        self.watch_scope(Scope::Exact(key.to_string()))
    }

    /// Subscribes to changes of every key under `prefix`.
    ///
    /// Semantics match [`watch`](HookStore::watch); unsubscribes when the
    /// receiver is dropped.
    pub fn watch_prefix(&self, prefix: &str) -> mpsc::Receiver<ChangeEvent> {
        self.watch_scope(Scope::Prefix(prefix.to_string()))
    }

    /// Subscribes to changes of every key in the store.
    ///
    /// Semantics match [`watch`](HookStore::watch); unsubscribes when the
    /// receiver is dropped.
    pub fn watch_all(&self) -> mpsc::Receiver<ChangeEvent> {
        self.watch_scope(Scope::All)
    }

    fn watch_scope(&self, scope: Scope) -> mpsc::Receiver<ChangeEvent> {
        self.subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_sender(scope)
    }
}

#[async_trait]
impl<S: GetSet + Send + Sync> GetSet for HookStore<S> {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.inner.get_bytes(key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        self.inner.has(key).await
    }

    async fn delete(&mut self, key: &str) -> Result<bool> {
        let old_value = self.inner.get_bytes(key).await?;
        if self.inner.delete(key).await? {
            notify(
                &self.subscribers,
                &self.inner,
                ChangeEvent {
                    key: key.to_string(),
                    kind: ChangeKind::Delete,
                    old_value,
                    new_value: None,
                },
            )
            .await;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        validate_key(&self.validators, &self.inner, key, value).await?;
        let prev = self.inner.set_bytes(key, value).await?;
        notify(
            &self.subscribers,
            &self.inner,
            ChangeEvent {
                key: key.to_string(),
                kind: ChangeKind::Set,
                old_value: prev.clone(),
                new_value: Some(value.to_vec()),
            },
        )
        .await;
        Ok(prev)
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.inner.gets_bytes(limit, direction, cursor).await
    }
}

#[async_trait]
impl<S> Store for HookStore<S>
where
    S: Store + Clone + Send + Sync,
    S::Transaction: Sync,
{
    type Transaction = HookTx<S::Transaction, S>;

    fn begin_tx(&mut self) -> Result<Self::Transaction> {
        Ok(HookTx {
            inner: self.inner.begin_tx()?,
            validators: self.validators.clone(),
            staged: Vec::new(),
            subscribers: Arc::clone(&self.subscribers),
            post_commit_view: self.inner.clone(),
        })
    }
}

/// A [`HookStore`] transaction that validates staged writes and defers
/// change notifications until commit.
///
/// Writes are validated as they are staged, so a rejected operation fails
/// early. Staged events replace any earlier event for the same key, and
/// nothing is broadcast unless [`commit`](Transaction::commit) succeeds —
/// rolled-back transactions produce no notifications.
pub struct HookTx<T, V> {
    inner: T,
    validators: Vec<Arc<dyn Validator>>,
    staged: Vec<ChangeEvent>,
    subscribers: Arc<Mutex<Subscribers>>,
    /// Committed-state view handed to observers after a successful commit.
    post_commit_view: V,
}

impl<T, V> HookTx<T, V> {
    /// Records a change event, replacing any earlier event for the same key
    /// so that each key appears at most once per commit. The original
    /// pre-transaction value is preserved across replacements so observers
    /// always see the net effect.
    fn stage(&mut self, mut event: ChangeEvent) {
        match self.staged.iter_mut().find(|e| e.key == event.key) {
            Some(existing) => {
                event.old_value = existing.old_value.take();
                *existing = event;
            }
            None => self.staged.push(event),
        }
    }
}

#[async_trait]
impl<T: GetSet + Send + Sync, V: Send + Sync> GetSet for HookTx<T, V> {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.inner.get_bytes(key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        self.inner.has(key).await
    }

    async fn delete(&mut self, key: &str) -> Result<bool> {
        let old_value = self.inner.get_bytes(key).await?;
        if self.inner.delete(key).await? {
            self.stage(ChangeEvent {
                key: key.to_string(),
                kind: ChangeKind::Delete,
                old_value,
                new_value: None,
            });
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        validate_key(&self.validators, &self.inner, key, value).await?;
        let prev = self.inner.set_bytes(key, value).await?;
        self.stage(ChangeEvent {
            key: key.to_string(),
            kind: ChangeKind::Set,
            old_value: prev.clone(),
            new_value: Some(value.to_vec()),
        });
        Ok(prev)
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.inner.gets_bytes(limit, direction, cursor).await
    }
}

#[async_trait]
impl<T: Transaction + Send + Sync, V: StoreView> Transaction for HookTx<T, V> {
    async fn commit(mut self) -> Result<()> {
        // Authoritative pass: re-validate every staged write against the
        // transaction's final state while it is still the exclusive writer,
        // so later writes within the same transaction (or concurrent ones
        // racing for the same write path) cannot invalidate a decision made
        // at staging time.
        for event in &self.staged {
            if let (ChangeKind::Set, Some(value)) = (event.kind, event.new_value.as_deref()) {
                let view = RevalidateView {
                    inner: &self.inner,
                    key: &event.key,
                    old_value: event.old_value.as_deref(),
                };
                validate_key(&self.validators, &view, &event.key, value).await?;
            }
        }

        self.inner.commit().await?;

        for event in self.staged {
            notify(&self.subscribers, &self.post_commit_view, event).await;
        }
        Ok(())
    }

    async fn rollback(self) -> Result<()> {
        self.inner.rollback().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::store::{BTreeStore, StoreError};

    struct Rejecting(Scope);

    #[async_trait]
    impl Validator for Rejecting {
        fn scope(&self) -> Scope {
            self.0.clone()
        }

        async fn validate(&self, _ctx: &dyn StoreView, _key: &str, _value: &[u8]) -> Result<()> {
            Err(StoreError::Other("rejected".into()))
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
                .map_err(|_| StoreError::Other(format!("key `{key}` requires JSON")))
        }
    }

    struct Collector(Arc<Mutex<Vec<ChangeEvent>>>);

    #[async_trait]
    impl Observer for Collector {
        fn scope(&self) -> Scope {
            Scope::Prefix("user:".into())
        }

        async fn on_change(&self, _ctx: &dyn StoreView, event: &ChangeEvent) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event.clone());
        }
    }

    type StoreUnderTest = HookStore<BTreeStore>;

    fn store() -> StoreUnderTest {
        HookStore::new(BTreeStore::default())
    }

    #[tokio::test]
    async fn test_validator_rejects_write_without_touching_store() {
        let mut s = store().with_validator(Rejecting(Scope::All));

        assert!(s.set_bytes("k", b"v").await.is_err());
        assert_eq!(s.get_bytes("k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_validator_scope_limits_application() {
        let mut s = store().with_validator(Rejecting(Scope::Exact(String::from("locked"))));

        assert!(s.set_bytes("locked", b"v").await.is_err());
        assert!(s.set_bytes("free", b"v").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_json_validator_accepts_only_json_values() {
        let mut s = store().with_validator(RequireJson(Scope::All));

        assert!(s.set_bytes("doc", br#"{"ok":true}"#).await.is_ok());
        assert!(s.set_bytes("raw", b"\x00\xff").await.is_err());
    }

    #[tokio::test]
    async fn test_watch_receives_set_and_delete_events() {
        let mut s = store();
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
    async fn test_events_carry_old_and_new_values() {
        let mut s = store();
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
    async fn test_transaction_event_reports_net_change() {
        let mut s = store();
        s.set_bytes("a", b"original").await.unwrap();
        let mut rx = s.watch("a");

        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes("a", b"first").await.unwrap();
        tx.set_bytes("a", b"second").await.unwrap();
        tx.commit().await.unwrap();

        let event = rx.try_recv().unwrap();
        assert_eq!(event.old_value, Some(b"original".to_vec()));
        assert_eq!(event.new_value, Some(b"second".to_vec()));
    }

    #[tokio::test]
    async fn test_slow_watcher_misses_events_but_does_not_block_writes() {
        let mut s = store();
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
    async fn test_delete_missing_key_notifies_nothing() {
        let mut s = store();
        let mut rx = s.watch_all();

        assert!(!s.delete("missing").await.unwrap());
        assert!(rx.try_recv().unwrap_err().is_empty());
    }

    #[tokio::test]
    async fn test_watch_prefix_ignores_unrelated_keys() {
        let mut s = store();
        let mut rx = s.watch_prefix("user:");

        s.set_bytes("other", b"v").await.unwrap();
        assert!(rx.try_recv().unwrap_err().is_empty());

        s.set_bytes("user:1", b"v").await.unwrap();
        assert_eq!(rx.try_recv().unwrap().key, "user:1");
    }

    #[tokio::test]
    async fn test_dropped_receiver_does_not_break_writes() {
        let mut s = store();
        drop(s.watch_all());

        assert!(s.set_bytes("k", b"v").await.is_ok());
    }

    #[tokio::test]
    async fn test_observer_receives_scoped_events() {
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
                },
                ChangeEvent {
                    key: "user:1".into(),
                    kind: ChangeKind::Delete,
                    old_value: Some(b"v".to_vec()),
                    new_value: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn test_transaction_commit_notifies_once_per_key() {
        let mut s = store();
        let mut rx = s.watch_all();

        let mut tx = s.begin_tx().unwrap();
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
    async fn test_transaction_rollback_notifies_nothing() {
        let mut s = store();
        let mut rx = s.watch_all();

        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes("a", b"1").await.unwrap();
        tx.rollback().await.unwrap();

        assert!(rx.try_recv().unwrap_err().is_empty());
        assert_eq!(s.get_bytes("a").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_transaction_validates_at_stage_time() {
        let mut s = store().with_validator(Rejecting(Scope::Exact(String::from("bad"))));
        let mut tx = s.begin_tx().unwrap();

        assert!(tx.set_bytes("bad", b"v").await.is_err());

        tx.set_bytes("good", b"v").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(s.get_bytes("good").await.unwrap(), Some(b"v".to_vec()));
    }

    struct NoOverwrite(Scope);

    #[async_trait]
    impl Validator for NoOverwrite {
        fn scope(&self) -> Scope {
            self.0.clone()
        }

        async fn validate(&self, ctx: &dyn StoreView, key: &str, _value: &[u8]) -> Result<()> {
            if ctx.has(key).await? {
                return Err(StoreError::Other(format!(
                    "key `{key}` must not be overwritten"
                )));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_stateful_validator_reads_transaction_state() {
        let mut s = store().with_validator(NoOverwrite(Scope::All));

        // Stage-time validation observes the transaction's own staged writes.
        let mut tx = s.begin_tx().unwrap();
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
                return Err(StoreError::Other(format!(
                    "`{key}` may only be written while `{}` is absent",
                    self.0
                )));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_commit_revalidates_against_final_transaction_state() {
        let mut s = store().with_validator(OnlyWhileAbsent(String::from("flag")));

        let mut tx = s.begin_tx().unwrap();
        // Passes at staging time: "flag" does not exist yet.
        tx.set_bytes("target", b"v").await.unwrap();
        // Creating "flag" inside the same tx invalidates "target" by commit.
        tx.set_bytes("flag", b"on").await.unwrap();

        assert!(tx.commit().await.is_err());
        // Re-validation failed before the inner transaction committed:
        // nothing landed in the store.
        assert_eq!(s.get_bytes("target").await.unwrap(), None);
        assert_eq!(s.get_bytes("flag").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_commit_revalidation_hides_pending_write_of_validated_key() {
        // Absence-based rules must not see the staged write being validated,
        // or every legitimate insert would be rejected at commit time.
        let mut s = store().with_validator(NoOverwrite(Scope::All));

        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes("fresh", b"v").await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(s.get_bytes("fresh").await.unwrap(), Some(b"v".to_vec()));
    }

    #[tokio::test]
    async fn test_transaction_delete_stages_single_event() {
        let mut s = store();
        s.set_bytes("a", b"1").await.unwrap();
        let mut rx = s.watch("a");

        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes("a", b"2").await.unwrap();
        tx.delete("a").await.unwrap();
        tx.commit().await.unwrap();

        let event = rx.try_recv().unwrap();
        assert_eq!(event.kind, ChangeKind::Delete);
        assert!(rx.try_recv().unwrap_err().is_empty());
    }

    #[tokio::test]
    async fn test_hook_store_composes_with_get_set_ext_and_query() {
        use crate::store::{Direction, GetSetExt};

        let mut s = store().with_validator(RequireJson(Scope::Prefix(String::from("doc:"))));

        s.set("doc:a", &serde_json::json!({ "lang": "rust", "stars": 10 }))
            .await
            .unwrap();
        assert!(s.set_bytes("doc:b", b"not json").await.is_err());

        let matches = s
            .gets(
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
