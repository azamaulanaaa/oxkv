//! Core traits and types for a key-value store with transaction support.
//!
//! This module defines the core abstractions for a persistent key-value store:
//!
//! - CRUD (Create, Read, Update, Delete) operations.
//! - Batched retrieval with bidirectional cursors.
//! - Atomic transactions.
//!
//! The primary traits are:
//! - [`GetSet`]: Basic key-value operations.
//! - [`Transaction`]: Extends [`GetSet`] with commit/rollback.
//! - [`Store`]: Extends [`GetSet`] with the ability to start a transaction.
//!
//! All operations return a [`Result`] with a [`StoreError`] on failure.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use thiserror::Error;

mod batch;
pub use batch::{BatchExt, BatchOp};
#[cfg(feature = "btree")]
pub use btree::{BTreeStore, BTreeTx};
#[cfg(feature = "btree")]
mod btree;
mod cas;
pub use cas::{Cas, CasResult};
pub use hooks::{
    ChangeEvent, ChangeKind, HookState, Hooked, Hooks, Observer, Scope, StoreView, Validator,
};
mod hooks;
pub mod testkit;
pub use testkit::unique_prefix;
#[cfg(feature = "oxkv")]
mod cache;
#[cfg(feature = "oxkv")]
mod lsm;
mod scan;
pub use scan::{KeyPrefix, Page, ScanExt};
#[cfg(feature = "oxkv")]
mod storage;
#[cfg(feature = "oxkv")]
pub use cache::{Cache, CacheStats, LruCache};
#[cfg(feature = "cached")]
pub use lsm::{CachedOxKvStore, CachedTx, WarmMode};
#[cfg(feature = "oxkv")]
pub use lsm::{OxKvReader, OxKvRoTx, OxKvStore, OxKvStoreBuilder, OxKvTx, SstFile};
#[cfg(feature = "oxkv")]
pub use storage::{
    GetOptions, GetOutput, MemStorage, ObjectPath, ObjectVersion, PutMode, PutOutcome, Storage,
};

/// A specialized `Result` type for store operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// A coarse class for a [`StoreError`].
///
/// Use this type to choose a recovery step. The class does not name the
/// failing backend or the failing key. Match on [`StoreError`] when you need
/// those details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Another writer won a race. A retry of the same call can succeed.
    Conflict,
    /// The storage backend failed. A retry of the same call can succeed.
    ///
    /// A failed write may still have landed. Re-read the key to learn the
    /// outcome before you retry the write.
    Transient,
    /// Another owner holds the epoch. The store rejected the call.
    ///
    /// This state is terminal. A retry cannot succeed. Restart the process
    /// and acquire the epoch again.
    Fenced,
    /// A validator rejected the write.
    ///
    /// A retry sends the same invalid write. Change the write before you
    /// retry.
    Rejected,
    /// The input was malformed. Examples are a JSON error and a UTF-8 error.
    ///
    /// A retry sends the same bytes. Change the input before you retry.
    InvalidInput,
    /// The error has no finer class.
    Other,
}

/// Portable async sleep for retry backoff.
///
/// Uses `futures-timer`, so it resolves on every target. This includes
/// `wasm32`, where `tokio::time` is unavailable.
#[cfg(feature = "oxkv")]
pub(crate) async fn sleep(duration: std::time::Duration) {
    futures_timer::Delay::new(duration).await;
}

/// Portable millisecond clock for TTLs and identifiers.
///
/// `std::time::Instant` panics on `wasm32-unknown-unknown`. The `web-time`
/// crate provides the same clock on every target. This clock reads epoch
/// milliseconds on native targets. On wasm it reads `Date.now()`. A backwards
/// jump only ever extends a cache TTL. A backwards jump is never a correctness
/// issue.
#[cfg(feature = "oxkv")]
pub(crate) fn now_millis() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Errors that can occur during store operations.
///
/// Use [`StoreError::kind`] to get a coarse class. Use
/// [`StoreError::is_retryable`] to learn whether a retry of the same call can
/// succeed. Use [`StoreError::retry_after`] to get a backend delay hint.
#[derive(Debug, Clone, Error)]
pub enum StoreError {
    /// An error originating from the underlying storage engine.
    #[error("storage error: {0}")]
    Storage(String),

    /// A conditional-write precondition failed. A create uses `If-None-Match`.
    /// An update uses `If-Match`. Someone else won the CAS race.
    ///
    /// Backends report this variant instead of a `Storage` string message, so
    /// callers match on the type. Callers may retry unless fencing says
    /// otherwise.
    #[error("CAS conflict: {0}")]
    CasConflict(String),

    /// A serialization or deserialization error (e.g., JSON).
    #[error("serialization error: {0}")]
    Serialization(String),

    /// An error when converting between UTF-8 strings and bytes.
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    /// A JSON serialization or deserialization error.
    ///
    /// Shared ownership keeps the whole error tree `Clone`. It does not
    /// duplicate payloads.
    #[error("JSON error: {0}")]
    Json(Arc<serde_json::Error>),

    /// An error when decoding UTF-8 from a byte slice.
    #[error("UTF-8 error: {0}")]
    Utf8Slice(#[from] std::str::Utf8Error),

    /// A generic error with a message.
    #[error("{0}")]
    Other(String),

    /// A validator rejected a write.
    ///
    /// The `key` names the key that the validator rejected. The `reason`
    /// explains why. API layers map this variant to a client error (400).
    #[error("rejected `{key}`: {reason}")]
    Rejected {
        /// The key that the validator rejected.
        key: String,
        /// The reason the validator rejected the write.
        reason: String,
    },

    /// The store has been fenced. Another owner acquired the epoch.
    ///
    /// This state is terminal. The current process must stop writing. It must
    /// then restart via `ownership.json` CAS.
    #[error("fenced: {0}")]
    Fenced(String),

    /// Conditional read not modified. The `ETag` matches `If-None-Match`.
    #[error("not modified")]
    NotModified,
}

impl StoreError {
    /// Returns the coarse class of this error.
    ///
    /// This method is the single source of truth for
    /// [`StoreError::is_retryable`].
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            StoreError::CasConflict(_) => ErrorKind::Conflict,
            StoreError::Storage(_) => ErrorKind::Transient,
            StoreError::Fenced(_) => ErrorKind::Fenced,
            StoreError::Rejected { .. } => ErrorKind::Rejected,
            StoreError::Serialization(_)
            | StoreError::Utf8(_)
            | StoreError::Json(_)
            | StoreError::Utf8Slice(_) => ErrorKind::InvalidInput,
            StoreError::Other(_) | StoreError::NotModified => ErrorKind::Other,
        }
    }

    /// Returns `true` when a retry of the same call can succeed.
    ///
    /// A retry can succeed for [`ErrorKind::Conflict`] and
    /// [`ErrorKind::Transient`]. It cannot succeed for every other kind. This
    /// method reads [`StoreError::kind`], so the two methods cannot disagree.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self.kind(), ErrorKind::Conflict | ErrorKind::Transient)
    }

    /// Returns the delay that the backend asks the caller to wait before a
    /// retry.
    ///
    /// This method returns `None` for every variant today. No backend reports
    /// a server hint yet. Callers choose their own backoff. Do not read `None`
    /// as "retry now".
    // The signature takes `&self` to give callers one uniform error API.
    // No backend supplies a hint yet.
    #[allow(clippy::unused_self)]
    #[must_use]
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        None
    }
}

impl PartialEq for StoreError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (StoreError::Storage(a), StoreError::Storage(b))
            | (StoreError::Serialization(a), StoreError::Serialization(b))
            | (StoreError::Other(a), StoreError::Other(b))
            | (StoreError::CasConflict(a), StoreError::CasConflict(b))
            | (StoreError::Fenced(a), StoreError::Fenced(b)) => a == b,
            (StoreError::Utf8(a), StoreError::Utf8(b)) => a == b,
            (StoreError::Utf8Slice(a), StoreError::Utf8Slice(b)) => a == b,
            (StoreError::Json(a), StoreError::Json(b)) => a.to_string() == b.to_string(),
            (
                StoreError::Rejected {
                    key: ka,
                    reason: ra,
                },
                StoreError::Rejected {
                    key: kb,
                    reason: rb,
                },
            ) => ka == kb && ra == rb,
            (StoreError::NotModified, StoreError::NotModified) => true,
            _ => false,
        }
    }
}

impl Eq for StoreError {}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        StoreError::Json(Arc::new(error))
    }
}

impl From<&str> for StoreError {
    fn from(msg: &str) -> Self {
        StoreError::Other(msg.to_string())
    }
}

/// Locks a `std` mutex, continuing through poisoning.
///
/// Policy: a poisoned lock means a previous holder panicked during a mutation.
/// The mutexes below guard state that this crate can rebuild or that this crate
/// accepts as best effort (caches, staged overlays, subscriber lists).
/// Availability wins over fail-fast for that state: take the guard and continue
/// instead of failing every subsequent operation. Durable state never relies on
/// this. Durable state goes through CAS.
pub(crate) fn lock_ignore_poison<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `std::sync::RwLock` counterpart of [`lock_ignore_poison`].
#[cfg(feature = "btree")]
pub(crate) fn rwlock_ignore_poison<T>(
    lock: &std::sync::RwLock<T>,
) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl From<std::string::String> for StoreError {
    fn from(msg: String) -> Self {
        StoreError::Other(msg)
    }
}

/// A single key-value pair. The value is raw bytes.
///
/// Batched retrieval operations return this type.
#[derive(Debug, Clone)]
pub struct KeyValue {
    /// The string key.
    pub key: String,
    /// The value as a byte vector.
    pub value: Vec<u8>,
}

/// Direction for cursor-based pagination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Traverse keys in ascending order (from the start cursor or from the beginning).
    Next,
    /// Traverse keys in descending order (from the end cursor or from the end).
    Prev,
}

/// Basic operations for a key-value store.
///
/// This trait provides the basic operations for store access. Every operation
/// is atomic. Every operation is immediately durable, unless a transaction
/// wraps the call.
#[async_trait]
pub trait GetSet {
    /// Retrieves the value associated with the given key.
    ///
    /// Returns `Ok(Some(bytes))` if the key exists. It returns `Ok(None)`
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Checks if a key exists in the store.
    ///
    /// Returns `Ok(true)` if the key exists. It returns `Ok(false)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn has(&self, key: &str) -> Result<bool>;

    /// Deletes the key-value pair for the given key.
    ///
    /// Returns `Ok(true)` if the key existed and this call deleted it. It
    /// returns `Ok(false)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn delete(&self, key: &str) -> Result<bool>;

    /// Sets a key-value pair. It inserts the pair if the key is absent. It
    /// updates the pair if the key is present.
    ///
    /// Returns the previous value if the key already existed. The store treats
    /// that case as an update. Returns `None` if the key did not exist before.
    /// The store treats that case as a new insertion.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Sets a key-value pair without reading the previous value.
    ///
    /// This method has the same durability as [`GetSet::set_bytes`]. It skips
    /// the read-your-write lookup, so a blind insert avoids the full read path
    /// (manifest + SST scan). Prefer this method for ingest where the previous
    /// value is discarded.
    ///
    /// The default body delegates to `set_bytes` and discards the result, so
    /// existing implementors are unaffected. A backend that reads and writes
    /// internally overrides this method.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        self.set_bytes(key, value).await?;
        Ok(())
    }

    /// Retrieves multiple key-value pairs with cursor-based pagination.
    ///
    /// # Parameters
    ///
    /// - `limit`: Maximum number of items to return. If `None`, all matching items are returned.
    /// - `direction`: Whether to traverse in ascending (`Next`) or descending (`Prev`) order.
    /// - `cursor`: A tuple of optional start and end cursors (both inclusive).
    ///
    ///   **For `Direction::Next` (ascending):**
    ///   - `(Some(start), Some(end))`: Range from `start` to `end` (inclusive). Both bounds must satisfy `start <= end`.
    ///   - `(Some(start), None)`: From `start` (inclusive) to the end of the range.
    ///   - `(None, Some(end))`: From the beginning to `end` (inclusive).
    ///   - `(None, None)`: All items.
    ///
    ///   **For `Direction::Prev` (descending):**
    ///   - `(Some(start), Some(end))`: Range from `start` down to `end` (inclusive). This range requires `start >= end`. Otherwise the result is empty.
    ///   - `(Some(start), None)`: From `start` (inclusive) down to the beginning of the range.
    ///   - `(None, Some(end))`: **Empty**. There is no starting point to traverse backwards from.
    ///   - `(None, None)`: **Empty**. The same reason applies.
    ///
    /// # Returns
    ///
    /// A vector of [`KeyValue`] pairs that match the query. The vector is ordered
    /// according to `direction`. The order is ascending for `Next` and descending
    /// for `Prev`.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>>;

    /// Retrieves keys with cursor-based pagination. This method does not return
    /// values.
    ///
    /// The parameters have the same meaning as [`GetSet::gets_bytes`]. The
    /// returned keys are in the order that `direction` selects.
    ///
    /// The default body calls [`GetSet::gets_bytes`] and discards the values.
    /// A backend overrides this method when it can read keys without loading
    /// values.
    ///
    /// A limit of `Some(0)` returns an empty vector. A limit of `None` reads
    /// every matching key and can exhaust memory.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn gets_keys(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<String>> {
        Ok(self
            .gets_bytes(limit, direction, cursor)
            .await?
            .into_iter()
            .map(|item| item.key)
            .collect())
    }
}

/// A transaction that groups multiple operations atomically.
///
/// Other readers do not see any operation on a transaction until the
/// transaction commits. A rollback discards all changes.
///
/// Transactions are obtained from a [`Store`] via [`Store::begin_tx`].
#[async_trait]
pub trait Transaction: GetSet {
    /// Commits the transaction. All changes become durable and visible.
    ///
    /// After the commit, the caller should no longer use the transaction handle.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the commit fails (e.g., conflict, I/O error).
    async fn commit(self) -> Result<()>;

    /// Aborts the transaction. This call discards every change.
    ///
    /// After the rollback, the caller should no longer use the transaction handle.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the rollback fails (rare).
    async fn rollback(self) -> Result<()>;
}

/// A store that supports atomic transactions.
///
/// This trait extends [`GetSet`] with the ability to create a new transaction.
/// All standalone (non-transactional) operations are immediately committed.
///
/// Hooks are separate from this trait. A backend may implement [`Hooks`]
/// directly, or a caller may wrap any [`GetSet`] in [`Hooked`].
#[async_trait]
pub trait Store: GetSet {
    /// The transaction type produced by [`begin_tx`][Self::begin_tx].
    ///
    /// Each concrete backend declares its own `Transaction` type here with the
    /// associated-type pattern. The declaration looks like
    /// `type Transaction = OxKvTx;`. This design allows zero-cost
    /// monomorphization. Zero-cost monomorphization requires no heap allocation
    /// and no vtable dispatch.
    type Transaction: Transaction + Send;

    /// Begins a new write transaction.
    ///
    /// The returned transaction object provides the same CRUD operations as the
    /// store. The transaction stages those operations until
    /// [`Transaction::commit`] is called.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the transaction cannot be started.
    fn begin_tx(&self) -> Result<Self::Transaction>;
}

/// Extension methods for binary serialization and bulk loading on [`Store`].
#[async_trait]
pub trait StoreExt: Store {
    /// Serializes all key-value pairs into a single contiguous `Vec<u8>`.
    ///
    /// Equivalent to concatenating every chunk yielded by
    /// [`save_stream`](Self::save_stream). For large stores prefer streaming
    /// directly to the destination instead.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if retrieval fails.
    async fn save(&self) -> Result<Vec<u8>>
    where
        // The SaveStream returned by save_stream requires this bound.
        Self: Sync + Sized,
    {
        let mut out = Vec::with_capacity(4096);
        let mut chunks = self.save_stream();
        while let Some(chunk) = chunks.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(out)
    }

    /// Streams the store's serialized form as byte chunks.
    ///
    /// The returned [`futures::Stream`] paginates through the store lazily.
    /// The stream yields chunks of at least 16 KiB, except for the final chunk.
    /// Memory use therefore stays bounded regardless of store size. Chunks
    /// concatenate to exactly what [`save`](Self::save) returns. Boundaries
    /// always fall between whole records, so each chunk can be decoded
    /// independently downstream.
    fn save_stream(&self) -> Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send + '_>>
    where
        // The returned stream must itself be Send. That requirement needs the
        // inner batch-read future (&self) to be Send. Sized is required because
        // the concrete SaveStream is boxed here.
        Self: Sync + Sized,
    {
        Box::pin(SaveStream::new(self))
    }

    /// Loads all key-value pairs directly from a `&[u8]` slice into the store.
    ///
    /// Equivalent to [`load_stream`](fn@load_stream) over a single chunk.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the payload is malformed or writing fails.
    async fn load(&self, data: &[u8]) -> Result<usize>
    where
        Self: Sized,
    {
        load_stream(
            self,
            stream::iter(std::iter::once(Ok::<_, StoreError>(data))),
        )
        .await
    }
}

/// Magic bytes prefixing every snapshot: ASCII `"OXKV"`.
const SNAPSHOT_MAGIC: [u8; 4] = *b"OXKV";

/// Wire-format version written by this build and accepted by the load paths.
///
/// Bump this value on any incompatible change to the header or the record
/// layout. Loaders reject other versions with a descriptive error instead of
/// mis-parsing them.
const SNAPSHOT_VERSION: u32 = 1;

/// Length of the snapshot header: magic bytes + little-endian version.
const SNAPSHOT_HEADER_LEN: usize = SNAPSHOT_MAGIC.len() + 4;

/// Appends the snapshot header (magic + version) to `buffer`.
fn write_snapshot_header(buffer: &mut Vec<u8>) {
    buffer.extend_from_slice(&SNAPSHOT_MAGIC);
    buffer.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
}

/// Loads key-value pairs directly from an arbitrary source of byte chunks into
/// `store`, inside one transaction committed on success.
///
/// This function is the streaming counterpart of [`StoreExt::load`]. A chunk
/// may split anywhere. A chunk may split mid-header, mid-key, or mid-value. A
/// chunk may arrive in any size. Decoding is fully incremental, so memory stays
/// bounded by the largest pending record. Memory does not scale with the total
/// payload. Typical sources are files, network bodies, and JS `ReadableStream`s
/// bridged via `wasm-streams` (see the WASM bindings).
///
/// A failure leaves `store` untouched. This function drops the transaction
/// without a commit. The drop discards every staged write.
///
/// Returns the number of records loaded.
///
/// # Errors
///
/// Returns a [`StoreError`] in the following cases:
/// - Any chunk returns an error (`E: Into<StoreError>`).
/// - The payload is not an oxkv snapshot.
/// - The format version of the payload is not supported.
/// - The stream ends mid-header or mid-record.
/// - A write fails.
///
/// The returned future is `Send` only if the chunk stream is `Send`. The
/// compiler infers this at each call site instead of taking it from a trait.
/// Sources that are not `Send`, such as `wasm-streams` adapters on `wasm32`,
/// are accepted at such a call site.
pub async fn load_stream<T, C, E, S>(store: &T, chunks: S) -> Result<usize>
where
    T: Store + ?Sized,
    C: AsRef<[u8]>,
    E: Into<StoreError>,
    S: Stream<Item = std::result::Result<C, E>> + Unpin,
{
    let tx = store.begin_tx()?;
    let mut count = 0usize;
    let mut decoder = RecordDecoder::new();
    let mut chunks = chunks;

    while let Some(item) = chunks.next().await {
        let chunk = item.map_err(Into::into)?;
        decoder.push(chunk.as_ref());

        // Validate and consume the versioned header before any record is
        // accepted. The function rejects an unknown producer up front.
        if !decoder.header_validated && !decoder.validate_header()? {
            continue; // header still arriving
        }

        while let Some((key, value)) = decoder.next_record()? {
            tx.set_bytes(&key, &value).await?;
            count += 1;
        }
        decoder.compact();
    }

    if !decoder.header_validated {
        return Err(StoreError::Serialization(
            "Truncated snapshot header at end of input".into(),
        ));
    }
    if !decoder.is_empty() {
        return Err(StoreError::Serialization(
            "Truncated record at end of input".into(),
        ));
    }

    tx.commit().await?;
    Ok(count)
}

impl<T: Store> StoreExt for T {}

/// Target size above which [`SaveStream`] flushes its internal buffer.
const SAVE_CHUNK_TARGET: usize = 16 * 1024;

/// Batch size used for paginated reads while streaming a save.
const SAVE_BATCH_SIZE: u32 = 256;

/// In-flight batch read held by [`SaveStream`] between polls.
type PendingBatch<'a> = Pin<Box<dyn Future<Output = Result<Vec<KeyValue>>> + Send + 'a>>;

/// A [`futures::Stream`] yielding the store's serialization as byte chunks.
///
/// Create this stream with [`StoreExt::save_stream`]. The stream emits records
/// in ascending key order. Boundaries always fall between records, so each
/// chunk decodes independently. Reading is lazy. Nothing is fetched until the
/// stream is polled. Only one page of 256 entries is held at a time.
#[must_use = "streams do nothing unless polled"]
pub struct SaveStream<'a, S> {
    inner: &'a S,
    cursor: Option<String>,
    buffer: Vec<u8>,
    pending: Option<PendingBatch<'a>>,
    exhausted: bool,
}

impl<'a, S> SaveStream<'a, S> {
    fn new(inner: &'a S) -> Self {
        let mut buffer = Vec::with_capacity(SAVE_CHUNK_TARGET);
        // The header leads every stream. An empty store therefore produces a
        // valid artifact that identifies the version.
        write_snapshot_header(&mut buffer);
        Self {
            inner,
            cursor: None,
            buffer,
            pending: None,
            exhausted: false,
        }
    }
}

impl<S> Stream for SaveStream<'_, S>
where
    S: GetSet + Sync,
{
    type Item = Result<Vec<u8>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            // Drain the in-flight batch read first.
            if let Some(fut) = this.pending.as_mut() {
                match fut.as_mut().poll(cx) {
                    Poll::Ready(batch) => {
                        this.pending = None;
                        let batch = match batch {
                            Ok(batch) => batch,
                            Err(e) => {
                                this.exhausted = true;
                                return Poll::Ready(Some(Err(e)));
                            }
                        };

                        if batch.is_empty() {
                            this.exhausted = true;
                        } else {
                            let mut last_key = None;
                            let mut processed = 0;
                            for kv in &batch {
                                // Skip the inclusive lower bound carried over from
                                // the previous page.
                                if this.cursor.as_deref() == Some(kv.key.as_str()) {
                                    continue;
                                }
                                match encode_record(&mut this.buffer, &kv.key, &kv.value) {
                                    Ok(()) => {
                                        processed += 1;
                                        last_key = Some(kv.key.clone());
                                    }
                                    Err(e) => {
                                        this.exhausted = true;
                                        return Poll::Ready(Some(Err(e)));
                                    }
                                }
                            }
                            this.cursor = last_key;

                            let is_last_batch = batch.len() < SAVE_BATCH_SIZE as usize;
                            // The stream also stops when every entry was
                            // skipped. That step avoids fetching the same
                            // inclusive-cursor page forever.
                            if is_last_batch || processed == 0 {
                                this.exhausted = true;
                            }
                        }
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }

            // Yield once enough data has accumulated.
            if this.buffer.len() >= SAVE_CHUNK_TARGET {
                return Poll::Ready(Some(Ok(std::mem::take(&mut this.buffer))));
            }

            if this.exhausted {
                return if this.buffer.is_empty() {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Ok(std::mem::take(&mut this.buffer))))
                };
            }

            // Fetch the next page. The future borrows the inner store, so
            // SaveStream carries a lifetime. SaveStream does not own its source.
            let inner = this.inner;
            let cursor = this.cursor.clone();
            this.pending = Some(Box::pin(async move {
                inner
                    .gets_bytes(Some(SAVE_BATCH_SIZE), Direction::Next, (cursor, None))
                    .await
            }));
        }
    }
}

/// Appends one length-prefixed record (`[u32 key len][key][u32 value len][value]`)
/// to `buffer`.
pub(crate) fn encode_record(buffer: &mut Vec<u8>, key: &str, value: &[u8]) -> Result<()> {
    let key_len = u32::try_from(key.len())
        .map_err(|e| StoreError::Serialization(format!("key too long: {e}")))?;
    let val_len = u32::try_from(value.len())
        .map_err(|e| StoreError::Serialization(format!("value too long: {e}")))?;

    buffer.reserve(8 + key.len() + value.len());
    buffer.extend_from_slice(&key_len.to_le_bytes());
    buffer.extend_from_slice(key.as_bytes());
    buffer.extend_from_slice(&val_len.to_le_bytes());
    buffer.extend_from_slice(value);
    Ok(())
}

/// Incremental decoder for the save/load record format. The decoder tolerates
/// arbitrary chunk boundaries.
struct RecordDecoder {
    buf: Vec<u8>,
    pos: usize,
    /// Set once the snapshot header (magic + version) has been validated.
    header_validated: bool,
}

impl RecordDecoder {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            pos: 0,
            header_validated: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// True when no undecoded bytes remain.
    fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Validates and consumes the snapshot header once enough bytes have
    /// arrived. Returns `Ok(false)` while more bytes are needed. An error
    /// means the payload can never be a snapshot that this build accepts.
    fn validate_header(&mut self) -> Result<bool> {
        let avail = &self.buf[self.pos..];
        if avail.len() >= SNAPSHOT_MAGIC.len() && avail[..SNAPSHOT_MAGIC.len()] != SNAPSHOT_MAGIC {
            return Err(StoreError::Serialization(
                "not an oxkv snapshot: missing OXKV magic".into(),
            ));
        }
        if avail.len() < SNAPSHOT_HEADER_LEN {
            return Ok(false); // header still arriving; magic already plausible
        }
        let version = u32::from_le_bytes([
            avail[SNAPSHOT_MAGIC.len()],
            avail[SNAPSHOT_MAGIC.len() + 1],
            avail[SNAPSHOT_MAGIC.len() + 2],
            avail[SNAPSHOT_MAGIC.len() + 3],
        ]);
        if version != SNAPSHOT_VERSION {
            return Err(StoreError::Serialization(format!(
                "unsupported oxkv snapshot version {version} (this build reads version {SNAPSHOT_VERSION})"
            )));
        }
        self.pos += SNAPSHOT_HEADER_LEN;
        self.header_validated = true;
        Ok(true)
    }

    /// Attempts to decode the next complete record. Returns `Ok(None)` while
    /// more bytes are needed.
    fn next_record(&mut self) -> Result<Option<(String, Vec<u8>)>> {
        let avail = &self.buf[self.pos..];
        if avail.len() < 4 {
            return Ok(None);
        }
        let key_len = u32::from_le_bytes([avail[0], avail[1], avail[2], avail[3]]) as usize;
        if avail.len() < 4 + key_len {
            return Ok(None);
        }
        let value_start = 4 + key_len;
        if avail.len() < value_start + 4 {
            return Ok(None);
        }
        let val_len = u32::from_le_bytes([
            avail[value_start],
            avail[value_start + 1],
            avail[value_start + 2],
            avail[value_start + 3],
        ]) as usize;
        if avail.len() < value_start + 4 + val_len {
            return Ok(None);
        }

        let key = std::str::from_utf8(&avail[4..value_start])?.to_string();
        let value = avail[value_start + 4..value_start + 4 + val_len].to_vec();
        self.pos += value_start + 4 + val_len;
        Ok(Some((key, value)))
    }

    /// Drops fully consumed prefix bytes. Called between chunks so the buffer
    /// never grows beyond the largest pending record plus one chunk.
    fn compact(&mut self) {
        if self.pos > 0 && self.pos * 2 >= self.buf.len() {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }
}

/// Extension methods for common serialization formats (BINCODE).
///
/// This trait is automatically implemented for all types that implement [`GetSet`].
#[async_trait]
pub trait GetSetExt: GetSet {
    /// Sets a value serialized with JSON, stored as raw bytes.
    ///
    /// The value is serialized with `serde_json`. The store writes the bytes
    /// directly. If the key already exists, this call overwrites it. The store
    /// treats that call as an update.
    ///
    /// Returns the previous value deserialized as `T` if the key already
    /// existed. The store treats that case as an update. Returns `None` if the
    /// key did not exist before. The store treats that case as a new insertion.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if serialization or storage fails.
    async fn set<T: serde::Serialize + Sync>(&self, key: &str, value: &T) -> Result<Option<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        let json = serde_json::to_vec(value)?;
        match self.set_bytes(key, &json).await? {
            Some(prev) => Ok(Some(serde_json::from_slice(&prev)?)),
            None => Ok(None),
        }
    }

    /// Sets a JSON-serialized value without reading the previous value.
    ///
    /// This method is the blind-write counterpart to [`GetSetExt::set`]. It
    /// has the same durability. It runs no read-your-write lookup. A backend
    /// that overrides [`GetSet::put_bytes`] skips the read path entirely.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if serialization or storage fails.
    async fn put<T: serde::Serialize + Sync>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_vec(value)?;
        self.put_bytes(key, &json).await
    }

    /// Retrieves a value and deserializes it using JSON.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if deserialization or retrieval fails.
    async fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.get_bytes(key).await? {
            Some(json) => Ok(Some(serde_json::from_slice(&json)?)),
            None => Ok(None),
        }
    }
}

impl<T: GetSet> GetSetExt for T {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{ErrorKind, StoreError};
    #[cfg(feature = "btree")]
    use super::{GetSet, SNAPSHOT_HEADER_LEN, StoreExt, load_stream};
    #[cfg(feature = "btree")]
    use futures::stream;

    /// One table covers every `StoreError` variant. Each row checks the kind,
    /// the retry answer, and the delay hint. A new variant must add a row.
    #[test]
    fn every_store_error_variant_has_a_stable_class() {
        let invalid_utf8 = vec![0xFF_u8];
        let cases = [
            (
                "Storage",
                StoreError::Storage("backend failed".into()),
                ErrorKind::Transient,
                true,
            ),
            (
                "CasConflict",
                StoreError::CasConflict("key changed".into()),
                ErrorKind::Conflict,
                true,
            ),
            (
                "Serialization",
                StoreError::Serialization("bad record".into()),
                ErrorKind::InvalidInput,
                false,
            ),
            (
                "Utf8",
                StoreError::Utf8(String::from_utf8(vec![0xFF]).expect_err("invalid utf8")),
                ErrorKind::InvalidInput,
                false,
            ),
            (
                "Json",
                StoreError::Json(std::sync::Arc::new(
                    serde_json::from_str::<i32>("not json").expect_err("invalid json"),
                )),
                ErrorKind::InvalidInput,
                false,
            ),
            (
                "Utf8Slice",
                StoreError::Utf8Slice(
                    std::str::from_utf8(&invalid_utf8).expect_err("invalid utf8"),
                ),
                ErrorKind::InvalidInput,
                false,
            ),
            (
                "Other",
                StoreError::Other("misc".into()),
                ErrorKind::Other,
                false,
            ),
            (
                "Rejected",
                StoreError::Rejected {
                    key: "k".into(),
                    reason: "r".into(),
                },
                ErrorKind::Rejected,
                false,
            ),
            (
                "Fenced",
                StoreError::Fenced("stale epoch".into()),
                ErrorKind::Fenced,
                false,
            ),
            (
                "NotModified",
                StoreError::NotModified,
                ErrorKind::Other,
                false,
            ),
        ];

        for (name, error, kind, retryable) in cases {
            assert_eq!(error.kind(), kind, "{name}: kind");
            assert_eq!(error.is_retryable(), retryable, "{name}: is_retryable");
            assert_eq!(error.retry_after(), None, "{name}: retry_after");
        }
    }

    /// `is_retryable` must follow `kind`, not a second mapping.
    #[test]
    fn only_conflict_and_transient_are_retryable() {
        let kinds = [
            (ErrorKind::Conflict, true),
            (ErrorKind::Transient, true),
            (ErrorKind::Fenced, false),
            (ErrorKind::Rejected, false),
            (ErrorKind::InvalidInput, false),
            (ErrorKind::Other, false),
        ];
        for (kind, retryable) in kinds {
            assert_eq!(
                matches!(kind, ErrorKind::Conflict | ErrorKind::Transient),
                retryable,
                "{kind:?}"
            );
        }
    }

    /// No test ever constructed the public UTF-8 error variants. Their
    /// `#[from]` conversions and `PartialEq` arms were therefore untested.
    #[test]
    fn utf8_error_variants_round_trip() {
        let invalid = vec![0xFF_u8, 0xFE];
        let invalid: &[u8] = &invalid;
        let owned = String::from_utf8(vec![0xFF, 0xFE]).expect_err("invalid utf8");
        let e: StoreError = owned.into();
        assert!(matches!(e, StoreError::Utf8(_)), "got {e:?}");
        assert!(e.to_string().starts_with("UTF-8 error:"));
        let same: StoreError = String::from_utf8(vec![0xFF, 0xFE]).unwrap_err().into();
        assert_eq!(e, same, "same cause must compare equal");
        let other: StoreError = String::from_utf8(vec![0xFF]).unwrap_err().into();
        assert_ne!(e, other, "different causes must not compare equal");

        let borrowed = std::str::from_utf8(invalid).expect_err("invalid utf8");
        let e: StoreError = borrowed.into();
        assert!(matches!(e, StoreError::Utf8Slice(_)), "got {e:?}");
        let same: StoreError = std::str::from_utf8(invalid).unwrap_err().into();
        assert_eq!(e, same);
        // The two UTF-8 variants are distinct and never compare equal.
        let owned: StoreError = String::from_utf8(vec![0xFF]).unwrap_err().into();
        assert_ne!(e, owned);
    }

    /// A chunk-level failure must surface *as that error*, not as some
    /// unrelated failure that happens to also be an `Err`.
    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn load_stream_propagates_the_chunk_error_identity() {
        let store = crate::store::BTreeStore::default();
        let injected = StoreError::Storage("injected chunk failure".to_string());
        let err = load_stream(&store, stream::iter([Err::<Vec<u8>, _>(injected.clone())]))
            .await
            .expect_err("chunk error must propagate");
        assert_eq!(err, injected);
        // An empty store saves exactly the snapshot header: nothing committed.
        let saved = store.save().await.unwrap();
        assert!(
            saved.len() <= SNAPSHOT_HEADER_LEN,
            "a failed load must not commit"
        );
    }

    /// A malformed payload is rejected outright, with nothing committed.
    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn load_stream_rejects_a_malformed_payload() {
        let store = crate::store::BTreeStore::default();
        let err = store
            .load(b"not a snapshot at all")
            .await
            .expect_err("garbage");
        assert!(
            !err.to_string().is_empty(),
            "malformed payload must report why"
        );
        assert!(!store.has("k").await.unwrap());
    }
}
