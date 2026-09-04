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
#[cfg(feature = "otel")]
pub use otel::{OtelStore, OtelTx};
#[cfg(feature = "otel")]
mod otel;
#[cfg(feature = "redb")]
pub use redb::{RedbStore, RedbTx};
#[cfg(feature = "redb")]
mod redb;
#[cfg(all(feature = "s3", not(target_arch = "wasm32")))]
mod s3;
#[cfg(all(feature = "s3", not(target_arch = "wasm32")))]
pub use s3::{S3Store, S3StoreBuilder};

/// A specialized `Result` type for store operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Errors that can occur during store operations.
#[derive(Debug, Error)]
pub enum StoreError {
    /// An error originating from the underlying storage engine (e.g., redb).
    #[error("storage error: {0}")]
    Storage(String),

    /// A serialization or deserialization error (e.g., JSON).
    #[error("serialization error: {0}")]
    Serialization(String),

    /// An error when converting between UTF-8 strings and bytes.
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    /// A JSON serialization or deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// An error when decoding UTF-8 from a byte slice.
    #[error("UTF-8 error: {0}")]
    Utf8Slice(#[from] std::str::Utf8Error),

    /// A generic error with a message.
    #[error("{0}")]
    Other(String),

    /// The store has been fenced — another owner acquired the epoch.
    ///
    /// Terminal: the current process must stop writing and restart via
    /// `ownership.json` CAS (see `docs/s3-lsm-design.md` §9 self-fencing).
    #[error("fenced: {0}")]
    Fenced(String),
}

impl PartialEq for StoreError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (StoreError::Storage(a), StoreError::Storage(b))
            | (StoreError::Serialization(a), StoreError::Serialization(b))
            | (StoreError::Other(a), StoreError::Other(b))
            | (StoreError::Fenced(a), StoreError::Fenced(b)) => a == b,
            (StoreError::Utf8(a), StoreError::Utf8(b)) => a == b,
            (StoreError::Utf8Slice(a), StoreError::Utf8Slice(b)) => a == b,
            (StoreError::Json(a), StoreError::Json(b)) => a.to_string() == b.to_string(),
            _ => false,
        }
    }
}

impl Eq for StoreError {}

impl From<&str> for StoreError {
    fn from(msg: &str) -> Self {
        StoreError::Other(msg.to_string())
    }
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
    async fn delete(&mut self, key: &str) -> Result<bool>;

    /// Sets a key-value pair, inserting if absent or updating if present.
    ///
    /// Returns the previous value if the key already existed (an update),
    /// or `None` if the key did not exist before (a new insertion).
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>>;

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
    /// associated-type pattern — e.g., `type Transaction = RedbTx;`. This
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
    fn begin_tx(&mut self) -> Result<Self::Transaction>;
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
    async fn load(&mut self, data: &[u8]) -> Result<usize>
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
pub async fn load_stream<T, C, E, S>(store: &mut T, chunks: S) -> Result<usize>
where
    T: Store + ?Sized,
    C: AsRef<[u8]>,
    E: Into<StoreError>,
    S: Stream<Item = std::result::Result<C, E>> + Unpin,
{
    let mut tx = store.begin_tx()?;
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
    async fn set<T: serde::Serialize + Sync>(&mut self, key: &str, value: &T) -> Result<Option<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        let json = serde_json::to_vec(value)?;
        match self.set_bytes(key, &json).await? {
            Some(prev) => Ok(Some(serde_json::from_slice(&prev)?)),
            None => Ok(None),
        }
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
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    mockall::mock! {
        pub Transaction {}

        #[async_trait]
        impl GetSet for Transaction {
            async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>>;
            async fn has(&self, key: &str) -> Result<bool>;
            async fn delete(&mut self, key: &str) -> Result<bool>;
            async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>>;
            async fn gets_bytes(
                &self,
                limit: Option<u32>,
                direction: Direction,
                cursor: (Option<String>, Option<String>),
            ) -> Result<Vec<KeyValue>>;
        }

        #[async_trait]
        impl Transaction for Transaction {
            async fn commit(self) -> Result<()>;
            async fn rollback(self) -> Result<()>;
        }
    }

    mockall::mock! {
        pub Store {}

        #[async_trait]
        impl GetSet for Store {
            async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>>;
            async fn has(&self, key: &str) -> Result<bool>;
            async fn delete(&mut self, key: &str) -> Result<bool>;
            async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>>;
            async fn gets_bytes(
                &self,
                limit: Option<u32>,
                direction: Direction,
                cursor: (Option<String>, Option<String>),
            ) -> Result<Vec<KeyValue>>;
        }

        #[async_trait]
        impl Store for Store {
            type Transaction = MockTransaction;
            fn begin_tx(&mut self) -> Result<MockTransaction>;
        }
    }

    #[test]
    fn test_store_error_partial_eq_storage() {
        let a = crate::StoreError::Storage("foo".into());
        let b = crate::StoreError::Storage("foo".into());
        assert_eq!(a, b);
        assert_ne!(a, crate::StoreError::Storage("bar".into()));
    }

    #[test]
    fn test_store_error_partial_eq_other() {
        let a: crate::StoreError = From::from("msg1");
        let b: crate::StoreError = From::from("msg2");
        assert_ne!(a, b);
    }

    #[test]
    fn test_store_error_cross_variant_not_equal() {
        let storage = crate::StoreError::Storage("x".into());
        let other: crate::StoreError = From::from("y");
        // Even though the inner messages could match, cross-variant comparison is false
        assert_ne!(storage, other);
    }

    #[test]
    fn test_store_error_from_str() {
        let err: crate::StoreError = From::from("oops");
        match err {
            crate::StoreError::Other(msg) => assert_eq!(msg, "oops"),
            _ => panic!("expected Other variant"),
        }
    }

    #[test]
    fn test_store_error_from_string() {
        let err: crate::StoreError = From::from(String::from("boom"));
        match err {
            crate::StoreError::Other(msg) => assert_eq!(msg, "boom"),
            _ => panic!("expected Other variant"),
        }
    }

    #[test]
    fn test_store_error_from_utf8() {
        // Exercises the From<&str> impl path — a UTF-8 error is typically produced internally
        // by converting &str to String, so this tests the conversion chain.
        let err: crate::StoreError = From::from("valid utf-8");
        match err {
            crate::StoreError::Other(msg) => assert_eq!(msg, "valid utf-8"),
            _ => panic!("expected Other variant"),
        }
    }

    #[tokio::test]
    async fn test_store_ext_save_mocked() {
        let mut mock_store = MockStore::new();

        // 1st call: Return a full batch of 256 items to force pagination to continue
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(256) && *dir == Direction::Next && cursor.0.is_none()
            })
            .returning(|_, _, _| {
                let batch = (0..256)
                    .map(|i| KeyValue {
                        key: format!("k{i:03}"),
                        value: format!("v{i}").into_bytes(),
                    })
                    .collect();
                Ok(batch)
            });

        // 2nd call: Return an empty batch with the cursor set to the 256th item ("k255")
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(256)
                    && *dir == Direction::Next
                    && cursor.0.as_deref() == Some("k255")
            })
            .returning(|_, _, _| Ok(vec![]));

        let bytes = mock_store.save().await.unwrap();
        assert!(!bytes.is_empty());
    }

    #[tokio::test]
    async fn test_store_ext_save_empty_mocked() {
        let mut mock_store = MockStore::new();

        mock_store
            .expect_gets_bytes()
            .once()
            .returning(|_, _, _| Ok(vec![]));

        let bytes = mock_store.save().await.unwrap();
        // Even an empty store yields a valid, version-identifiable artifact.
        let mut expected = Vec::new();
        write_snapshot_header(&mut expected);
        assert_eq!(bytes, expected);
    }

    #[tokio::test]
    async fn test_store_ext_load_mocked() {
        let mut mock_store = MockStore::new();

        // Craft a binary payload for single key-value ("key1", "val1")
        let mut payload = Vec::new();
        write_snapshot_header(&mut payload);
        // key len = 4, key = "key1"
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(b"key1");
        // val len = 4, val = "val1"
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(b"val1");

        mock_store.expect_begin_tx().once().returning(|| {
            let mut mock_tx = MockTransaction::new();

            mock_tx
                .expect_set_bytes()
                .withf(|key, val| key == "key1" && val == b"val1")
                .once()
                .returning(|_, _| Ok(None));

            mock_tx.expect_commit().once().returning(|| Ok(()));

            Ok(mock_tx)
        });

        let loaded_count = mock_store.load(&payload).await.unwrap();
        assert_eq!(loaded_count, 1);
    }

    #[tokio::test]
    async fn test_store_ext_load_corrupted_data_mocked() {
        let mut mock_store = MockStore::new();

        // `begin_tx` is called before decoding begins
        mock_store
            .expect_begin_tx()
            .returning(|| Ok(MockTransaction::new()));

        // Truncated key length header (< 4 bytes)
        assert!(mock_store.load(&[1, 2]).await.is_err());

        // Truncated key payload
        let bad_key_payload = vec![10, 0, 0, 0, b'a', b'b'];
        assert!(mock_store.load(&bad_key_payload).await.is_err());

        // Truncated value length header
        let bad_val_header = vec![1, 0, 0, 0, b'a', 0, 0];
        assert!(mock_store.load(&bad_val_header).await.is_err());

        // Truncated value payload
        let bad_val_payload = vec![1, 0, 0, 0, b'a', 5, 0, 0, 0, b'x'];
        assert!(mock_store.load(&bad_val_payload).await.is_err());
    }

    fn json_kv(key: &str, value: &serde_json::Value) -> KeyValue {
        KeyValue {
            key: key.to_string(),
            value: serde_json::to_vec(&value).expect("serializable"),
        }
    }

    #[tokio::test]
    async fn test_gets_with_query_matches_only_valid_json_documents() {
        let mut mock_store = MockStore::new();
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(256) && *dir == Direction::Next && cursor.0.is_none()
            })
            .returning(|_, _, _| {
                Ok(vec![
                    json_kv("match", &serde_json::json!({ "lang": "rust" })),
                    KeyValue {
                        key: "invalid".to_string(),
                        value: b"not json".to_vec(),
                    },
                    json_kv("other", &serde_json::json!({ "lang": "go" })),
                    KeyValue {
                        key: "scalar".to_string(),
                        value: serde_json::to_vec(&serde_json::json!("rust")).unwrap(),
                    },
                ])
            });

        let found = mock_store
            .gets(None, Direction::Next, (None, None), Some("lang:rust"))
            .await
            .expect("gets succeeds");
        let keys: Vec<&str> = found.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, ["match"]);
    }

    #[tokio::test]
    async fn test_gets_paginates_through_full_batches() {
        let mut mock_store = MockStore::new();

        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(256) && *dir == Direction::Next && cursor.0.is_none()
            })
            .returning(|_, _, _| {
                Ok((0..256)
                    .map(|i| {
                        json_kv(
                            &format!("k{i:03}"),
                            &serde_json::json!({ "ok": i % 2 == 0 }),
                        )
                    })
                    .collect())
            });

        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(256)
                    && *dir == Direction::Next
                    && cursor.0.as_deref() == Some("k255")
            })
            .returning(|_, _, _| Ok(Vec::new()));

        let found = mock_store
            .gets(None, Direction::Next, (None, None), Some("ok:true"))
            .await
            .expect("gets succeeds");
        assert_eq!(found.len(), 128);
        assert_eq!(found[0].key, "k000");
        assert_eq!(found[127].key, "k254");
    }

    #[tokio::test]
    async fn test_gets_on_empty_store_returns_empty() {
        let mut mock_store = MockStore::new();
        mock_store
            .expect_gets_bytes()
            .times(1)
            .returning(|_, _, _| Ok(Vec::new()));

        let found = mock_store
            .gets(None, Direction::Next, (None, None), Some("anything"))
            .await
            .expect("gets succeeds");
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn test_gets_with_invalid_query_returns_error_without_scanning() {
        let mut mock_store = MockStore::new();
        // No gets_bytes expectation: the scan must never start.
        mock_store.expect_gets_bytes().never();

        let result = mock_store
            .gets(None, Direction::Next, (None, None), Some("a AND"))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_gets_supports_field_paths_and_ranges() {
        let mut mock_store = MockStore::new();
        mock_store
            .expect_gets_bytes()
            .times(1)
            .returning(|_, _, _| {
                Ok(vec![
                    json_kv(
                        "user1",
                        &serde_json::json!({ "name": "Ada", "age": 36, "tags": ["math"] }),
                    ),
                    json_kv(
                        "user2",
                        &serde_json::json!({ "name": "Alan", "age": 41, "tags": ["code"] }),
                    ),
                ])
            });

        let young = mock_store
            .gets(
                None,
                Direction::Next,
                (None, None),
                Some(r"name:?da AND age:[30 TO 40]"),
            )
            .await
            .expect("gets succeeds");
        assert_eq!(young.len(), 1);
        assert_eq!(young[0].key, "user1");
    }

    #[tokio::test]
    async fn test_gets_without_query_passes_through_to_gets_bytes() {
        let mut mock_store = MockStore::new();
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(10)
                    && *dir == Direction::Prev
                    && cursor.0.as_deref() == Some("z")
                    && cursor.1.as_deref() == Some("a")
            })
            .returning(|_, _, _| {
                // Non-JSON values are returned untouched when no query is given
                Ok(vec![
                    KeyValue {
                        key: "b".to_string(),
                        value: b"raw bytes".to_vec(),
                    },
                    json_kv("c", &serde_json::json!({ "lang": "rust" })),
                ])
            });

        let found = mock_store
            .gets(
                Some(10),
                Direction::Prev,
                (Some("z".into()), Some("a".into())),
                None,
            )
            .await
            .expect("gets succeeds");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].value, b"raw bytes".to_vec());
    }

    #[tokio::test]
    async fn test_gets_with_query_limits_matched_results() {
        let mut mock_store = MockStore::new();
        mock_store
            .expect_gets_bytes()
            .times(1)
            .returning(|_, _, _| {
                Ok((0..5)
                    .map(|i| json_kv(&format!("k{i}"), &serde_json::json!({ "hit": true })))
                    .collect())
            });

        let found = mock_store
            .gets(Some(2), Direction::Next, (None, None), Some("hit:true"))
            .await
            .expect("gets succeeds");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].key, "k0");
        assert_eq!(found[1].key, "k1");
    }

    #[tokio::test]
    async fn test_gets_with_query_respects_direction_and_range() {
        let mut mock_store = MockStore::new();
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|_, dir, cursor| {
                *dir == Direction::Prev && cursor.0.is_none() && cursor.1.as_deref() == Some("k9")
            })
            .returning(|_, _, _| {
                Ok(vec![
                    json_kv("k8", &serde_json::json!({ "v": 8 })),
                    json_kv("k7", &serde_json::json!({ "v": 7 })),
                ])
            });

        let found = mock_store
            .gets(
                None,
                Direction::Prev,
                (None, Some("k9".into())),
                Some("v:[7 TO 8]"),
            )
            .await
            .expect("gets succeeds");
        let keys: Vec<&str> = found.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, ["k8", "k7"]);
    }

    // -- streaming save/load -------------------------------------------------

    /// Encodes records the same way the save path does, for building payloads.
    /// Builds a complete snapshot payload (header + records) for feeding the
    /// load paths.
    fn encode_snapshot(pairs: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        write_snapshot_header(&mut buf);
        for (key, value) in pairs {
            encode_record(&mut buf, key, value).unwrap();
        }
        buf
    }

    /// Raw header bytes for an arbitrary format version.
    fn raw_header(version: u32) -> Vec<u8> {
        let mut header = SNAPSHOT_MAGIC.to_vec();
        header.extend_from_slice(&version.to_le_bytes());
        header
    }

    #[tokio::test]
    async fn test_save_stream_chunks_concatenate_to_expected_encoding() {
        let mut mock_store = MockStore::new();
        // Two full pages plus a short final page forces multiple fetches and
        // exercises the inclusive-cursor skip between pages.
        let page = |offset: usize| -> Vec<KeyValue> {
            (0..SAVE_BATCH_SIZE)
                .map(|i| KeyValue {
                    key: format!("k{:06}", offset + i as usize),
                    value: format!("v{}", offset + i as usize).into_bytes(),
                })
                .collect()
        };
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(SAVE_BATCH_SIZE) && *dir == Direction::Next && cursor.0.is_none()
            })
            .returning(move |_, _, _| Ok(page(0)));
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(SAVE_BATCH_SIZE)
                    && *dir == Direction::Next
                    && cursor.0.as_deref() == Some("k000255")
            })
            .returning(move |_, _, _| Ok(page(256)));
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(SAVE_BATCH_SIZE)
                    && *dir == Direction::Next
                    && cursor.0.as_deref() == Some("k000511")
            })
            .returning(|_, _, _| {
                Ok((0..3)
                    .map(|i| KeyValue {
                        key: format!("k{:06}", 512 + i),
                        value: b"tail".to_vec(),
                    })
                    .collect())
            });

        let streamed = {
            let mut collected = Vec::new();
            let mut chunks = mock_store.save_stream();
            while let Some(chunk) = chunks.next().await {
                collected.extend_from_slice(&chunk.unwrap());
            }
            collected
        };
        // Compare against an independently encoded payload rather than calling
        // save(), whose pagination would exhaust the mock's one-shot pages.
        let mut all_pairs: Vec<(&str, &[u8])> = (0..512)
            .map(|i| {
                let key: &str = &*Box::leak(format!("k{i:06}").into_boxed_str());
                let val: &[u8] = &*Box::leak(format!("v{i}").into_bytes().into_boxed_slice());
                // Leak is fine in tests; keeps lifetimes simple.
                (key, val)
            })
            .collect();
        all_pairs.push(("k000512", b"tail"));
        all_pairs.push(("k000513", b"tail"));
        all_pairs.push(("k000514", b"tail"));
        let expected = encode_snapshot(&all_pairs);
        assert_eq!(streamed, expected);
    }

    #[tokio::test]
    async fn test_save_stream_on_empty_store_yields_header_only() {
        let mut mock_store = MockStore::new();
        mock_store
            .expect_gets_bytes()
            .once()
            .returning(|_, _, _| Ok(vec![]));

        let mut chunks = mock_store.save_stream();
        // Exactly one chunk: the header alone is a complete empty snapshot.
        let first = chunks.next().await.unwrap().unwrap();
        assert_eq!(first.len(), SNAPSHOT_HEADER_LEN);
        assert!(chunks.next().await.is_none());
    }

    #[tokio::test]
    async fn test_load_stream_survives_single_byte_chunks() {
        let pairs: Vec<(String, Vec<u8>)> = vec![
            ("a".into(), b"apple".to_vec()),
            ("kk".into(), Vec::new()),
            ("kkk-longer-key".into(), vec![7u8; 40]),
        ];
        let payload = encode_snapshot(
            &pairs
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_slice()))
                .collect::<Vec<_>>(),
        );

        let pair_count = pairs.len();
        for chunk_size in [1usize, 2, 3, 4, 5, 7, 11] {
            let log = Arc::new(Mutex::new(Vec::new()));
            let mut mock_store = MockStore::new();
            let tx_log = Arc::clone(&log);
            mock_store.expect_begin_tx().once().returning(move || {
                let mut tx = MockTransaction::new();
                let log = Arc::clone(&tx_log);
                tx.expect_set_bytes()
                    .times(pair_count)
                    .withf(move |key: &str, value: &[u8]| {
                        log.lock().unwrap().push((key.to_string(), value.to_vec()));
                        true
                    })
                    .returning(|_, _| Ok(None));
                tx.expect_commit().once().returning(|| Ok(()));
                Ok(tx)
            });

            let chunks = payload
                .chunks(chunk_size)
                .map(<[u8]>::to_vec)
                .map(Ok::<_, StoreError>);
            let loaded = load_stream(&mut mock_store, stream::iter(chunks))
                .await
                .unwrap();
            assert_eq!(loaded, pairs.len(), "chunk_size {chunk_size}");
            assert_eq!(*log.lock().unwrap(), pairs, "chunk_size {chunk_size}");
        }
    }

    #[tokio::test]
    async fn test_load_stream_splits_at_every_offset() {
        let pairs = [
            ("k1", "v1".as_bytes()),
            ("kk2", b"".as_slice()),
            ("k3", b"xyz"),
        ];
        let payload = encode_snapshot(&pairs);

        let pair_count = pairs.len();
        for split in 1..payload.len() {
            let log = Arc::new(Mutex::new(Vec::new()));
            let mut mock_store = MockStore::new();
            let tx_log = Arc::clone(&log);
            mock_store.expect_begin_tx().once().returning(move || {
                let mut tx = MockTransaction::new();
                let log = Arc::clone(&tx_log);
                tx.expect_set_bytes()
                    .times(pair_count)
                    .withf(move |key: &str, value: &[u8]| {
                        log.lock().unwrap().push((key.to_string(), value.to_vec()));
                        true
                    })
                    .returning(|_, _| Ok(None));
                tx.expect_commit().once().returning(|| Ok(()));
                Ok(tx)
            });

            let first = payload[..split].to_vec();
            let rest = payload[split..].to_vec();
            let loaded = load_stream(
                &mut mock_store,
                stream::iter([Ok::<Vec<u8>, StoreError>(first), Ok(rest)]),
            )
            .await
            .unwrap();
            assert_eq!(loaded, pairs.len(), "split at {split}");
            let expected: Vec<(String, Vec<u8>)> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.to_vec()))
                .collect();
            assert_eq!(*log.lock().unwrap(), expected, "split at {split}");
        }
    }

    #[tokio::test]
    async fn test_load_stream_truncated_at_end_errors_without_commit() {
        let mut mock_store = MockStore::new();
        mock_store.expect_begin_tx().once().returning(|| {
            let mut tx = MockTransaction::new();
            // Complete records are staged before the truncation is discovered;
            // they must never be committed.
            tx.expect_set_bytes().once().returning(|_, _| Ok(None));
            tx.expect_commit().never();
            Ok(tx)
        });

        // A complete record followed by a dangling length header.
        let mut payload = encode_snapshot(&[("k", b"v")]);
        payload.extend_from_slice(&99u32.to_le_bytes());

        let err = load_stream(
            &mut mock_store,
            stream::iter([Ok::<Vec<u8>, StoreError>(payload)]),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, StoreError::Serialization(_)));
    }

    #[tokio::test]
    async fn test_load_stream_rejects_foreign_magic_before_any_writes() {
        let mut mock_store = MockStore::new();
        mock_store.expect_begin_tx().once().returning(|| {
            let mut tx = MockTransaction::new();
            tx.expect_set_bytes().never();
            tx.expect_commit().never();
            Ok(tx)
        });

        // Structurally plausible payload with the wrong magic.
        let mut payload = b"JUNK".to_vec();
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend(encode_snapshot(&[("k", b"v")])[SNAPSHOT_HEADER_LEN..].to_vec());

        let err = load_stream(
            &mut mock_store,
            stream::iter([Ok::<Vec<u8>, StoreError>(payload)]),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, StoreError::Serialization(ref msg) if msg.contains("magic")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_load_stream_rejects_unknown_format_version() {
        let mut mock_store = MockStore::new();
        mock_store.expect_begin_tx().once().returning(|| {
            let mut tx = MockTransaction::new();
            tx.expect_set_bytes().never();
            tx.expect_commit().never();
            Ok(tx)
        });

        let mut payload = raw_header(SNAPSHOT_VERSION + 1);
        payload.extend(encode_snapshot(&[("k", b"v")])[SNAPSHOT_HEADER_LEN..].to_vec());

        let err = load_stream(
            &mut mock_store,
            stream::iter([Ok::<Vec<u8>, StoreError>(payload)]),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, StoreError::Serialization(ref msg)
                if msg.contains("unsupported oxkv snapshot version")),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_load_stream_rejects_truncated_header() {
        let mut mock_store = MockStore::new();
        mock_store.expect_begin_tx().once().returning(|| {
            let mut tx = MockTransaction::new();
            tx.expect_set_bytes().never();
            tx.expect_commit().never();
            Ok(tx)
        });

        // Only three of the eight header bytes ever arrive.
        let err = load_stream(
            &mut mock_store,
            stream::iter([Ok::<Vec<u8>, StoreError>(b"OXK".to_vec())]),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, StoreError::Serialization(_)));
    }

    #[tokio::test]
    async fn test_load_stream_propagates_chunk_errors_and_skips_commit() {
        let mut mock_store = MockStore::new();
        // The transaction is opened up-front so writes can be staged as chunks
        // arrive; a mid-stream error drops it (rollback) before any commit.
        mock_store.expect_begin_tx().once().returning(|| {
            let mut tx = MockTransaction::new();
            tx.expect_set_bytes().never();
            tx.expect_commit().never();
            Ok(tx)
        });

        let result: Result<usize> = load_stream(
            &mut mock_store,
            stream::iter([Err::<std::vec::Vec<u8>, _>("")]),
        )
        .await;
        assert!(result.is_err());
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn test_save_load_stream_round_trip_through_btree_store() {
        let mut source = BTreeStore::default();
        for i in 0..600u32 {
            source
                .set_bytes(&format!("key{i:04}"), format!("value-{i}").as_bytes())
                .await
                .unwrap();
        }

        // Stream the save into a fresh store, chunk by chunk.
        let mut restored = BTreeStore::default();
        {
            let mut chunks = source.save_stream();
            let mut pending: Vec<Vec<u8>> = Vec::new();
            while let Some(chunk) = chunks.next().await {
                pending.push(chunk.unwrap());
            }

            let count = load_stream(
                &mut restored,
                stream::iter(pending.into_iter().map(Ok::<_, StoreError>)),
            )
            .await
            .unwrap();
            assert_eq!(count, 600);
        }

        let original = source
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .unwrap();
        let copied = restored
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(original.len(), copied.len());
        for (a, b) in original.iter().zip(&copied) {
            assert_eq!(a.key, b.key);
            assert_eq!(a.value, b.value);
        }
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn test_save_stream_flushes_multiple_chunks_and_preserves_large_records() {
        let mut store = BTreeStore::default();
        // Many mid-size records force several flushes at the 16 KiB target...
        for i in 0..2000u32 {
            store
                .set_bytes(&format!("key{i:05}"), format!("value-{i}").as_bytes())
                .await
                .unwrap();
        }
        // ...while one oversized record proves boundaries stay between records.
        let big_value = vec![42u8; 50 * 1024];
        store.set_bytes("big", &big_value).await.unwrap();

        let mut chunk_count = 0;
        let mut reassembled = Vec::new();
        {
            let mut chunks = store.save_stream();
            while let Some(chunk) = chunks.next().await {
                reassembled.extend_from_slice(&chunk.unwrap());
                chunk_count += 1;
            }
        }
        assert!(
            chunk_count >= 2,
            "expected several chunks, got {chunk_count}"
        );

        // Reassembled bytes must equal the plain save() encoding exactly.
        let expected = store.save().await.unwrap();
        assert_eq!(reassembled, expected);
    }
}
