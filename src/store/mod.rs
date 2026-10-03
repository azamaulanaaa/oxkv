//! Core traits and types for a key-value store with transaction support.
//!
//! This module defines the foundational abstractions for a persistent key-value
//! store, with operations for CRUD (Create, Read, Update, Delete), batched
//! retrieval with bidirectional cursors, and atomic transactions.
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

use crate::query::{eval as eval_json_query, parse as parse_query};

#[cfg(feature = "btree")]
pub use btree::{BTreeStore, BTreeTx};
#[cfg(feature = "btree")]
mod btree;
pub use hooks::{
    ChangeEvent, ChangeKind, HookStore, HookTx, Observer, Scope, StoreView, Validator,
};
mod hooks;
#[cfg(all(feature = "otel", not(target_arch = "wasm32")))]
pub use otel::{OtelStore, OtelTx};
#[cfg(feature = "oxkv")]
mod cache;
#[cfg(feature = "oxkv")]
mod lsm;
#[cfg(all(feature = "otel", not(target_arch = "wasm32")))]
mod otel;
#[cfg(feature = "oxkv")]
mod storage;
#[cfg(feature = "oxkv")]
pub use cache::{Cache, CacheStats, LruCache};
#[cfg(all(feature = "oxkv", feature = "btree"))]
pub use lsm::{CachedOxKvStore, CachedTx, WarmMode};
#[cfg(feature = "oxkv")]
pub use lsm::{OxKvReader, OxKvRoTx, OxKvStore, OxKvStoreBuilder, OxKvTx, SstFile};
#[cfg(all(feature = "oxkv", target_arch = "wasm32"))]
pub use storage::OpfsStorage;
#[cfg(feature = "oxkv")]
pub use storage::{
    GetOptions, GetOutput, MemStorage, ObjectPath, ObjectVersion, PutMode, PutOutcome, Storage,
};

/// A specialized `Result` type for store operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Portable async sleep for retry backoff.
///
/// Uses `futures-timer` so it resolves on every target, including `wasm32`
/// where `tokio::time` is unavailable.
#[cfg(feature = "oxkv")]
pub(crate) async fn sleep(duration: std::time::Duration) {
    futures_timer::Delay::new(duration).await;
}

/// Portable millisecond clock for TTLs and ids.
///
/// `std::time::Instant` panics on `wasm32-unknown-unknown`, so TTLs use this
/// instead: epoch millis natively, `Date.now()` on wasm. Backwards jumps only
/// ever extend a cache TTL — never a correctness issue.
#[cfg(feature = "oxkv")]
pub(crate) fn now_millis() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
    #[cfg(target_arch = "wasm32")]
    {
        // `Date.now()` (~1.7e12, always finite and non-negative) fits in u64
        // with room to spare, so this saturating cast is exact in practice.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let millis = js_sys::Date::now() as u64;
        millis
    }
}

/// Errors that can occur during store operations.
#[derive(Debug, Clone, Error)]
pub enum StoreError {
    /// An error originating from the underlying storage engine.
    #[error("storage error: {0}")]
    Storage(String),

    /// A conditional-write precondition failed (`If-None-Match` on create,
    /// `If-Match` on update): someone else won the CAS race.
    ///
    /// Backends report this variant (never a stringly `Storage` message) so
    /// callers match on the type. Retryable unless fencing says otherwise.
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
    /// Shared ownership keeps the whole error tree `Clone` without
    /// duplicating payloads.
    #[error("JSON error: {0}")]
    Json(Arc<serde_json::Error>),

    /// An error when decoding UTF-8 from a byte slice.
    #[error("UTF-8 error: {0}")]
    Utf8Slice(#[from] std::str::Utf8Error),

    /// A generic error with a message.
    #[error("{0}")]
    Other(String),

    /// The store has been fenced — another owner acquired the epoch.
    ///
    /// Terminal: the current process must stop writing and restart via
    /// `ownership.json` CAS.
    #[error("fenced: {0}")]
    Fenced(String),

    /// Conditional read not modified — `ETag` matches `If-None-Match`.
    #[error("not modified")]
    NotModified,
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
/// Policy: a poisoned lock means a previous holder panicked mid-mutation.
/// The mutexes below guard reconstructible or best-effort state (caches,
/// staged overlays, subscriber lists), so availability wins over fail-fast:
/// take the guard and continue rather than failing every subsequent
/// operation. Durable state never relies on this — it goes through CAS.
pub(crate) fn lock_ignore_poison<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `std::sync::RwLock` counterpart of [`lock_ignore_poison`].
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

/// A single key-value pair, where the value is raw bytes.
///
/// This is used as the return type for batched retrieval operations.
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
/// This trait provides the fundamental operations for interacting with the store.
/// All operations are atomic and immediately durable (unless wrapped in a transaction).
#[async_trait]
pub trait GetSet {
    /// Retrieves the value associated with the given key.
    ///
    /// Returns `Ok(Some(bytes))` if the key exists, `Ok(None)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Checks if a key exists in the store.
    ///
    /// Returns `Ok(true)` if the key exists, `Ok(false)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn has(&self, key: &str) -> Result<bool>;

    /// Deletes the key-value pair for the given key.
    ///
    /// Returns `Ok(true)` if the key existed and was deleted, `Ok(false)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn delete(&self, key: &str) -> Result<bool>;

    /// Sets a key-value pair, inserting if absent or updating if present.
    ///
    /// Returns the previous value if the key already existed (an update),
    /// or `None` if the key did not exist before (a new insertion).
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Sets a key-value pair without reading the previous value.
    ///
    /// Same durability as [`GetSet::set_bytes`] but skips the read-your-write
    /// lookup, so blind inserts avoid a full read path (manifest + SST scan).
    /// Prefer this for ingest where the previous value is discarded.
    ///
    /// The default body delegates to `set_bytes` and discards the result, so
    /// existing implementors are unaffected; backends that read-then-write
    /// internally override it.
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
    ///   - `(Some(start), Some(end))`: Range from `start` to `end` (inclusive), both bounds must satisfy `start <= end`.
    ///   - `(Some(start), None)`: From `start` (inclusive) to the end of the range.
    ///   - `(None, Some(end))`: From the beginning to `end` (inclusive).
    ///   - `(None, None)`: All items.
    ///
    ///   **For `Direction::Prev` (descending):**
    ///   - `(Some(start), Some(end))`: Range from `start` down to `end` (inclusive); requires `start >= end`, otherwise the result is empty.
    ///   - `(Some(start), None)`: From `start` (inclusive) down to the beginning of the range.
    ///   - `(None, Some(end))`: **Empty** – because there is no starting point to traverse backwards from.
    ///   - `(None, None)`: **Empty** – same reason.
    ///
    /// # Returns
    ///
    /// A vector of [`KeyValue`] pairs matching the query, ordered according to `direction`
    /// (ascending for `Next`, descending for `Prev`).
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
}

/// A transaction that groups multiple operations atomically.
///
/// All operations performed on a transaction are not visible to other readers
/// until the transaction is committed. If the transaction is rolled back, all
/// changes are discarded.
///
/// Transactions are obtained from a [`Store`] via [`Store::begin_tx`].
#[async_trait]
pub trait Transaction: GetSet {
    /// Commits the transaction, making all changes durable and visible.
    ///
    /// After commit, the transaction handle should no longer be used.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the commit fails (e.g., conflict, I/O error).
    async fn commit(self) -> Result<()>;

    /// Aborts the transaction, discarding all changes made.
    ///
    /// After rollback, the transaction handle should no longer be used.
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
#[async_trait]
pub trait Store: GetSet {
    /// The transaction type produced by [`begin_tx`][Self::begin_tx].
    ///
    /// Each concrete backend declares its own `Transaction` type here via the
    /// associated-type pattern — e.g., `type Transaction = OxKvTx;`. This
    /// allows zero-cost monomorphization: no heap allocation, no vtable dispatch.
    type Transaction: Transaction + Send;

    /// Begins a new write transaction.
    ///
    /// The returned transaction object provides the same CRUD operations as the store,
    /// but they are staged until [`Transaction::commit`] is called.
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
        // Required to drive the SaveStream returned by save_stream.
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
    /// The returned [`futures::Stream`] paginates through the store lazily and
    /// yields chunks of at least 16 KiB (except for the final chunk), so memory
    /// use stays bounded regardless of store size. Chunks concatenate to exactly
    /// what [`save`](Self::save) returns; boundaries always fall between whole
    /// records, so each chunk can be decoded independently downstream.
    fn save_stream(&self) -> Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send + '_>>
    where
        // The returned stream must itself be Send, which requires the inner
        // batch-read future (&self) to be Send; Sized because the concrete
        // SaveStream is boxed here.
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
/// Bump on any incompatible change to the header or record layout; loaders
/// reject other versions with a descriptive error instead of mis-parsing.
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
/// This is the streaming counterpart of [`StoreExt::load`]. Chunks may split
/// anywhere — mid-header, mid-key, mid-value — and arrive in any size; decoding
/// is fully incremental, so memory stays bounded by the largest pending record
/// rather than the total payload. Typical sources: files, network bodies, or
/// JS `ReadableStream`s bridged via `wasm-streams` (see the WASM bindings).
///
/// A failure leaves `store` untouched: the transaction is dropped without
/// commit, discarding all staged writes.
///
/// Returns the number of records loaded.
///
/// # Errors
///
/// Returns a [`StoreError`] if any chunk errors (`E: Into<StoreError>`), the
/// payload is not an oxkv snapshot, its format version is unsupported, the
/// stream ends mid-header or mid-record, or writing fails.
///
/// Note that the returned future is only `Send` when the chunk stream is: this
/// is inferred per call site rather than imposed by a trait, so non-`Send`
/// sources (such as `wasm-streams` adapters on `wasm32`) are accepted there.
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
        // accepted; unknown producers are rejected up-front.
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
/// Created via [`StoreExt::save_stream`]. Records are emitted in ascending key
/// order; boundaries always fall between records, so each chunk decodes
/// independently. Reading is lazy: nothing is fetched until polled, and only
/// one page of 256 entries is held at a time.
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
        // The header leads every stream so even an empty store produces a
        // valid, version-identifiable artifact.
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
                            // Also stop when everything was skipped to avoid
                            // re-fetching the same inclusive-cursor page forever.
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

            // Fetch the next page. The future borrows the inner store, which is
            // why SaveStream carries a lifetime instead of owning its source.
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

/// Incremental decoder for the save/load record format, tolerant of arbitrary
/// chunk boundaries.
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
    /// arrived. Returns `Ok(false)` while more bytes are needed; an error
    /// means the payload can never be a snapshot this build accepts.
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

    /// Attempts to decode the next complete record; returns `Ok(None)` while
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
    /// The value is serialized using `serde_json` and stored directly as bytes.
    /// If the key already exists, it will be overwritten (treated as an update).
    ///
    /// Returns the previous value deserialized as `T` if the key already existed
    /// (an update), or `None` if the key did not exist before (a new insertion).
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
    /// Blind-write counterpart to [`GetSetExt::set`]: same durability, no
    /// read-your-write lookup. Backends that override [`GetSet::put_bytes`]
    /// skip the read path entirely.
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

    /// Retrieves JSON documents with cursor-based pagination, optionally
    /// filtered by a query string.
    ///
    /// This mirrors [`GetSet::gets_bytes`]: the `limit`, `direction`, and
    /// `cursor` parameters carry identical semantics. When `query` is `None`
    /// this is a direct pass-through to [`gets_bytes`][GetSet::gets_bytes].
    ///
    /// When a query is provided (Lucene-style syntax parsed by
    /// [`crate::parse`]), entries are scanned in the requested order and an
    /// entry matches when its stored bytes deserialize as a
    /// `serde_json::Value` that satisfies the query. Entries whose values are
    /// not valid JSON are skipped. Here `limit` caps the number of *matching*
    /// entries returned; scanning continues across batches until the limit is
    /// reached or the range is exhausted.
    ///
    /// Matching is evaluated with [`crate::eval`]; see the `query` module docs
    /// for the full matching semantics.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the query is invalid or retrieval fails.
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
        // Checked before the scan: the loop below tests the limit *after* each
        // push, so `Some(0)` would otherwise return one row.
        if max_results == 0 {
            return Ok(Vec::new());
        }
        let ast = parse_query(query).map_err(StoreError::Other)?;
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
                // Skip duplicate processing when the cursor bound is inclusive
                if page_cursor.as_deref() == Some(kv.key.as_str()) {
                    continue;
                }

                let matches = serde_json::from_slice::<serde_json::Value>(&kv.value)
                    .is_ok_and(|value| eval_json_query(&ast, &value));
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
}

impl<T: GetSet> GetSetExt for T {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {

    use super::{Direction, GetSet, GetSetExt};
    /// `limit = 0` means zero rows. Regression: the loop tested the limit
    /// *after* each push, so `Some(0)` returned one row — and since this is a
    /// `GetSet` default method, every backend was affected.
    async fn assert_limit_semantics<T: GetSet + Sync>(store: &T, name: &str) {
        let rows = |limit| async move {
            store
                .gets(Some(limit), Direction::Next, (None, None), Some("n:1"))
                .await
                .unwrap()
        };
        assert_eq!(
            rows(0).await.len(),
            0,
            "{name}: gets(limit=0) returned rows"
        );
        assert_eq!(rows(1).await.len(), 1, "{name}: gets(limit=1)");
        assert_eq!(rows(2).await.len(), 2, "{name}: gets(limit=2)");
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn query_gets_limit_zero_returns_nothing_on_btree() {
        let s = crate::store::BTreeStore::default();
        for k in ["a", "b", "c"] {
            s.set_bytes(k, br#"{"n":1}"#).await.unwrap();
        }
        assert_limit_semantics(&s, "btree").await;
        // The pass-through path (no query) must agree.
        assert!(
            s.gets(Some(0), Direction::Next, (None, None), None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn query_gets_limit_zero_returns_nothing_on_oxkv() {
        let s = crate::store::OxKvStore::builder()
            .with_store(crate::store::lsm::new_in_memory())
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        for k in ["a", "b", "c"] {
            s.set_bytes(k, br#"{"n":1}"#).await.unwrap();
        }
        assert_limit_semantics(&s, "oxkv").await;
        // The pass-through path (no query) must agree with `gets_bytes`.
        assert!(
            s.gets(Some(0), Direction::Next, (None, None), None)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            s.gets_bytes(Some(0), Direction::Next, (None, None))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
