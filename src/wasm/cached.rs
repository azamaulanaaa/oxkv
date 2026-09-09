//! WASM bindings for [`store::CachedOxKvStore`] -- write-through RAM mirror.
//!
//! JS names mirror the Rust names (`CachedOxKvStore`, `CachedOxKvTx`).

use wasm_bindgen::prelude::*;

use crate::store::{self, GetSet, GetSetExt, Store, StoreExt, Transaction, load_stream};

use super::{Direction, json_compatible};

// Manual wasm-bindgen wrappers for CachedOxKvStore — memory-backed on wasm.

/// Wrapper around the write-through [`store::CachedOxKvStore`] for WASM.
///
/// Same API as [`JsOxKvStore`](super::oxkv::JsOxKvStore), but every key is
/// mirrored in memory: reads serve without storage I/O while writes keep
/// WAL durability. Backed by an in-memory [`store::MemStorage`], so contents
/// live only as long as the page (no browser persistence yet — OPFS arrives
/// as another [`store::Storage`] backend). Snapshots are byte-identical with
/// every other backend, so `save` bytes restore anywhere via `load`.
///
/// The wrapper holds an `Arc<Mutex<CachedOxKvStore>>` so that multiple
/// JavaScript calls share one underlying store. Each method acquires the lock,
/// runs the operation (async), and releases before returning.
#[wasm_bindgen(js_name = CachedOxKvStore)]
pub struct JsCachedOxKvStore {
    inner: std::sync::Arc<futures::lock::Mutex<store::CachedOxKvStore>>,
}

#[wasm_bindgen(js_class = CachedOxKvStore)]
impl JsCachedOxKvStore {
    /// Create a new in-memory cached store (`MemStorage`, probe skipped).
    ///
    /// Acquires ownership and warms every key into memory before resolving;
    /// from then on the instance behaves like any other [`store::Store`]
    /// with zero-I/O reads.
    /// # Errors
    /// * `StoreError` - if the store fails to initialize
    #[wasm_bindgen(
        js_name = "create",
        return_description = "A new CachedOxKvStore handle"
    )]
    pub async fn create(
        #[wasm_bindgen(param_description = "Key prefix inside the store; defaults to js-lsm")]
        prefix: Option<String>,
        #[wasm_bindgen(
            param_description = "Staleness bound for checked reads in milliseconds; defaults to 1000"
        )]
        stale_ttl_ms: Option<u64>,
    ) -> Result<JsCachedOxKvStore, JsValue> {
        let prefix = prefix.unwrap_or_else(|| "js-lsm".to_string());
        let backend = std::sync::Arc::new(store::MemStorage::new());
        let ttl = std::time::Duration::from_millis(stale_ttl_ms.unwrap_or(1000));
        match store::OxKvStore::builder()
            .with_store(backend)
            .with_prefix(store::ObjectPath::from(prefix))
            .skip_probe(true)
            .build_cached()
            .await
        {
            Ok(store) => Ok(Self {
                inner: std::sync::Arc::new(futures::lock::Mutex::new(store.with_stale_ttl(ttl))),
            }),
            Err(e) => Err(e.into()),
        }
    }

    /// Create a persistent LSM store backed by origin private storage (OPFS).
    ///
    /// Same API as [`create`](Self::create), but contents survive page reloads:
    /// objects live as real files under one `oxkv` OPFS directory, with
    /// content-hash etags so fencing and CAS work across sessions. The storage
    /// probe runs on every open, so a broken backend fails fast instead of
    /// corrupting data. Main-thread only; cross-tab races resolve
    /// last-writer-wins.
    /// # Errors
    /// * `StoreError` - if OPFS is unavailable/denied or the store fails to initialize
    #[wasm_bindgen(
        js_name = "createPersistent",
        return_description = "A new persistent CachedOxKvStore handle"
    )]
    pub async fn create_persistent(
        #[wasm_bindgen(param_description = "Key prefix inside the store; defaults to js-lsm")]
        prefix: Option<String>,
        #[wasm_bindgen(
            param_description = "Staleness bound for checked reads in milliseconds; defaults to 1000"
        )]
        stale_ttl_ms: Option<u64>,
    ) -> Result<JsCachedOxKvStore, JsValue> {
        let prefix = prefix.unwrap_or_else(|| "js-lsm".to_string());
        let backend = std::sync::Arc::new(store::OpfsStorage::open().await?);
        let ttl = std::time::Duration::from_millis(stale_ttl_ms.unwrap_or(1000));
        match store::OxKvStore::builder()
            .with_store(backend)
            .with_prefix(store::ObjectPath::from(prefix))
            .build_cached()
            .await
        {
            Ok(store) => Ok(Self {
                inner: std::sync::Arc::new(futures::lock::Mutex::new(store.with_stale_ttl(ttl))),
            }),
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieve a value by key. Returns the raw bytes as a `Uint8Array`, or `null` if the key does not exist.
    /// # Errors
    /// * `StoreError` - if an I/O error occurs reading from the store
    #[wasm_bindgen(return_description = "Raw bytes as a Uint8Array, or null when not found")]
    pub async fn get_bytes(
        &self,
        #[wasm_bindgen(param_description = "The key to retrieve")] key: &str,
    ) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.get_bytes(key).await {
            Ok(Some(bytes)) => {
                let arr = js_sys::Uint8Array::from(&bytes[..]);
                Ok(arr.into())
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Checks if a key exists in the store.
    /// # Errors
    /// * `StoreError` - if an I/O error occurs reading from the store
    #[wasm_bindgen(return_description = "true when the key exists, false otherwise")]
    pub async fn has(
        &self,
        #[wasm_bindgen(param_description = "The key to check for existence")] key: &str,
    ) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.has(key).await {
            Ok(exists) => Ok(JsValue::from(exists)),
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieve a value by key, revalidating the mirror first when the
    /// staleness bound elapsed. Plain [`get_bytes`](Self::get_bytes) is
    /// zero-I/O; use this when another tab may have taken the epoch.
    /// # Errors
    /// * `StoreError` - if revalidation fails or the mirror is fenced
    #[wasm_bindgen(
        js_name = "getBytesChecked",
        return_description = "Raw bytes as a Uint8Array, or null when not found"
    )]
    pub async fn get_bytes_checked(
        &self,
        #[wasm_bindgen(param_description = "The key to retrieve")] key: &str,
    ) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.get_bytes_checked(key).await {
            Ok(Some(bytes)) => {
                let arr = js_sys::Uint8Array::from(&bytes[..]);
                Ok(arr.into())
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Applies every missing generation to the mirror.
    ///
    /// Same-owner appends replay incrementally; a new ownership epoch or a
    /// missed flush window rebuilds from a full scan instead.
    /// # Errors
    /// * `StoreError` - if the manifest, a WAL file, or the rebuild scan fails
    #[wasm_bindgen(return_description = "Number of key records applied to the mirror")]
    pub async fn refresh(&self) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.refresh().await {
            Ok(applied) => {
                let count = u32::try_from(applied)
                    .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
                Ok(JsValue::from(count))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Reports whether the mirror lags the durable core.
    ///
    /// Polls `manifest.json` conditionally; ownership is not read here.
    /// # Errors
    /// * `StoreError` - if the manifest poll fails
    #[wasm_bindgen(
        js_name = "checkStale",
        return_description = "true when a refresh would apply changes"
    )]
    pub async fn check_stale(&self) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.check_stale().await {
            Ok(stale) => Ok(JsValue::from(stale)),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a key-value pair (inserts if absent, updates if present).
    /// # Errors
    /// * `StoreError` - if an I/O error occurs writing to the store
    #[wasm_bindgen(
        return_description = "Previous value as a Uint8Array if the key already existed, or null if it was newly inserted"
    )]
    pub async fn set_bytes(
        &self,
        #[wasm_bindgen(param_description = "The key to set")] key: &str,
        #[wasm_bindgen(param_description = "Byte array of the value to store under the given key")]
        value: &[u8],
    ) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.set_bytes(key, value).await {
            Ok(Some(prev)) => {
                let arr = js_sys::Uint8Array::from(&prev[..]);
                Ok(arr.into())
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a key to an arbitrary JSON-shaped value.
    /// # Errors
    /// * `StoreError` - if serialization of the value or an I/O error occurs
    #[wasm_bindgen(js_name = "set")]
    pub async fn set(
        &self,
        #[wasm_bindgen(param_description = "The key to set")] key: &str,
        #[wasm_bindgen(param_description = "An arbitrary JSON-shaped value from JavaScript")]
        value: JsValue,
    ) -> Result<JsValue, JsValue> {
        let json_value: serde_json::Value = serde_wasm_bindgen::from_value(value)
            .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
        let store = self.inner.lock().await;
        match store.set(key, &json_value).await {
            Ok(Some(prev)) => {
                let js_value = json_compatible(&prev)?;
                Ok(js_value)
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieve a JSON-shaped value by key, or null when not found.
    /// # Errors
    /// * `StoreError` - if deserialization of the stored value or an I/O error occurs
    #[wasm_bindgen(js_name = "get")]
    pub async fn get(
        &self,
        #[wasm_bindgen(param_description = "The key to retrieve")] key: &str,
    ) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.get::<serde_json::Value>(key).await {
            Ok(Some(value)) => {
                let js_value = json_compatible(&value)?;
                Ok(js_value)
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a key and its associated value. Returns `true` if the key was present and removed, `false` otherwise.
    /// # Errors
    /// * `StoreError` - if an I/O error occurs during deletion
    #[wasm_bindgen(
        return_description = "true if a key existed and was removed; false when no prior value was present"
    )]
    pub async fn delete(
        &self,
        #[wasm_bindgen(param_description = "The key to remove from storage")] key: &str,
    ) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.delete(key).await {
            Ok(deleted) => Ok(JsValue::from(deleted)),
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieve key-value pairs with cursor-based pagination.
    /// # Errors
    /// * `StoreError` - if an I/O error occurs reading from the store
    #[wasm_bindgen(return_description = "An array of key-value objects")]
    pub async fn gets_bytes(
        &self,
        #[wasm_bindgen(
            param_description = "Optional maximum number of results to return; omit (None) for all matches"
        )]
        limit: Option<u32>,
        #[wasm_bindgen(param_description = "Sort order for pagination — ascending or descending")]
        direction: Direction,
        #[wasm_bindgen(
            param_description = "Optional start key for the range; keys *at* this cursor are included when present"
        )]
        start_cursor: Option<String>,
        #[wasm_bindgen(
            param_description = "Optional end key for the range; keys *at* this cursor are included when present"
        )]
        end_cursor: Option<String>,
    ) -> Result<Vec<js_sys::Object>, JsValue> {
        let cursor = (start_cursor, end_cursor);

        let store = self.inner.lock().await;
        match store.gets_bytes(limit, direction.into(), cursor).await {
            Ok(kvs) => kvs
                .into_iter()
                .map(|value| {
                    let obj = js_sys::Object::new();
                    let js_key = js_sys::JsString::from(value.key);
                    let js_val = js_sys::Uint8Array::from(&value.value[..]);
                    js_sys::Reflect::set(&obj, &"key".into(), &js_key)?;
                    js_sys::Reflect::set(&obj, &"value".into(), &js_val)?;
                    Ok(obj)
                })
                .collect::<Result<_, JsValue>>(),
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieve JSON documents with cursor-based pagination, optionally filtered
    /// by a Lucene-style query string.
    /// # Errors
    /// * `StoreError` - if the query is invalid or an I/O error occurs
    #[wasm_bindgen(
        return_description = "An array of key-value objects where `value` is the parsed JSON document"
    )]
    pub async fn gets(
        &self,
        #[wasm_bindgen(param_description = "Optional maximum number of results to return")]
        limit: Option<u32>,
        #[wasm_bindgen(param_description = "Sort order for pagination — ascending or descending")]
        direction: Direction,
        #[wasm_bindgen(
            param_description = "Optional start key for the range; keys *at* this cursor are included when present"
        )]
        start_cursor: Option<String>,
        #[wasm_bindgen(
            param_description = "Optional end key for the range; keys *at* this cursor are included when present"
        )]
        end_cursor: Option<String>,
        #[wasm_bindgen(
            param_description = "Optional Lucene-style query string (e.g. \"age:[30 TO 40] AND tags:rust\")"
        )]
        query: Option<String>,
    ) -> Result<Vec<js_sys::Object>, JsValue> {
        let store = self.inner.lock().await;
        match store
            .gets(
                limit,
                direction.into(),
                (start_cursor, end_cursor),
                query.as_deref(),
            )
            .await
        {
            Ok(kvs) => kvs
                .into_iter()
                .map(|kv| {
                    let obj = js_sys::Object::new();
                    let js_key = js_sys::JsString::from(kv.key);
                    let js_val =
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&kv.value) {
                            json_compatible(&value)?
                        } else {
                            js_sys::Uint8Array::from(&kv.value[..]).into()
                        };
                    js_sys::Reflect::set(&obj, &"key".into(), &js_key)?;
                    js_sys::Reflect::set(&obj, &"value".into(), &js_val)?;
                    Ok(obj)
                })
                .collect(),
            Err(e) => Err(e.into()),
        }
    }

    /// Begin a new write transaction. The returned handle lets you stage CRUD operations
    /// that are invisible to other readers until `commit` is called.
    /// # Errors
    /// * `StoreError` - if an I/O error occurs while acquiring the lock
    #[wasm_bindgen(return_description = "A new transaction handle")]
    pub async fn begin_tx(&self) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.begin_tx() {
            Ok(tx) => {
                let js_tx = JsCachedOxKvTx {
                    inner: std::sync::Arc::new(futures::lock::Mutex::new(Some(tx))),
                };
                Ok(js_tx.into())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Serializes all key-value pairs into a single contiguous `Uint8Array`.
    ///
    /// Byte-identical with every other backend, so the bytes restore anywhere
    /// via `load` — including `BTreeStore` and native `CachedOxKvStore`.
    ///
    /// # Errors
    /// * `StoreError` - if retrieval fails while serializing the store
    #[wasm_bindgen(return_description = "Serialized key-value store as a Uint8Array")]
    pub async fn save(&self) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.save().await {
            Ok(bytes) => {
                let arr = js_sys::Uint8Array::from(&bytes[..]);
                Ok(arr.into())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Loads key-value pairs from a binary slice into the store.
    ///
    /// # Errors
    /// * `StoreError` - if the binary payload is invalid or storage fails
    #[wasm_bindgen(return_description = "Number of key-value pairs successfully loaded")]
    pub async fn load(
        &self,
        #[wasm_bindgen(param_description = "The binary data to load key-value pairs from")]
        data: &[u8],
    ) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.load(data).await {
            Ok(count) => {
                let count_u32 = u32::try_from(count)
                    .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
                Ok(JsValue::from(count_u32))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Streams the entire store as a `ReadableStream` of `Uint8Array` chunks.
    ///
    /// Chunks concatenate to exactly what [`save`](Self::save) returns; chunk
    /// boundaries always fall between whole records, so each chunk can be
    /// decoded independently downstream (e.g. piped straight into a file or
    /// fetch upload).
    ///
    /// The stream reads lazily and holds the store lock only while a chunk is
    /// being produced; cancelling the consumer releases it automatically.
    ///
    /// # Errors
    /// * `StoreError` - if retrieval fails while streaming
    #[wasm_bindgen(
        return_description = "A ReadableStream of Uint8Array chunks containing the serialized store"
    )]
    pub fn save_stream(&self) -> web_sys::ReadableStream {
        use futures::{SinkExt, StreamExt, TryStreamExt};

        // The core save_stream borrows its source, but JS streams must own
        // their data ('static). Drive the borrowed stream in a background task
        // through a bounded channel, which also provides backpressure: the
        // next page is only fetched once the consumer drains a chunk. When the
        // consumer cancels, the sink errors out, the task exits, and the lock
        // is released.
        let (mut sender, receiver) = futures::channel::mpsc::channel::<store::Result<Vec<u8>>>(16);
        let inner = std::sync::Arc::clone(&self.inner);
        wasm_bindgen_futures::spawn_local(async move {
            let store = inner.lock().await;
            let mut chunks = store.save_stream();
            while let Some(chunk) = chunks.next().await {
                if sender.send(chunk).await.is_err() {
                    break; // consumer cancelled
                }
            }
        });

        let chunks = receiver
            .map_ok(|chunk| js_sys::Uint8Array::from(&chunk[..]).into())
            .map_err(store::StoreError::into);
        wasm_streams::readable::ReadableStream::from_stream(chunks).into_raw()
    }

    /// Loads key-value pairs from a `ReadableStream` of byte chunks into the
    /// store inside one transaction.
    ///
    /// Chunk boundaries are arbitrary — chunks may split mid-record; decoding
    /// is incremental, so memory stays bounded regardless of payload size.
    /// Typical sources: `File.stream()`, `fetch()` bodies, or the stream
    /// returned by [`save_stream`](Self::save_stream).
    ///
    /// # Errors
    /// * `StoreError` - if any chunk fails to decode as bytes, the payload is
    ///   malformed or truncated, or storage fails. On error nothing is committed.
    #[wasm_bindgen(return_description = "Number of key-value pairs successfully loaded")]
    pub async fn load_stream(
        &self,
        #[wasm_bindgen(
            param_description = "A ReadableStream of Uint8Array chunks containing the serialized store"
        )]
        stream: web_sys::ReadableStream,
    ) -> Result<JsValue, JsValue> {
        use futures::stream::StreamExt;

        let readable = wasm_streams::readable::ReadableStream::from_raw(stream);
        let chunks = readable.into_stream().map(|item| {
            item.map_err(|e| store::StoreError::Other(format!("stream read failed: {e:?}")))
                .and_then(|value| {
                    value
                        .dyn_into::<js_sys::Uint8Array>()
                        .map(|array| array.to_vec())
                        .map_err(|e| {
                            store::StoreError::Other(format!("expected Uint8Array chunk: {e:?}"))
                        })
                })
        });

        let store = self.inner.lock().await;
        let count = load_stream(&*store, chunks).await?;
        let count_u32 =
            u32::try_from(count).map_err(|e| store::StoreError::Serialization(e.to_string()))?;
        Ok(JsValue::from(count_u32))
    }
}

/// Transaction handle for [`JsCachedOxKvStore`]: staged overlay, durable only on `commit`.
#[wasm_bindgen(js_name = CachedOxKvTx)]
#[derive(Clone)]
pub struct JsCachedOxKvTx {
    inner: std::sync::Arc<
        futures::lock::Mutex<Option<<store::CachedOxKvStore as store::Store>::Transaction>>,
    >,
}

#[wasm_bindgen(js_class = CachedOxKvTx)]
impl JsCachedOxKvTx {
    async fn take_tx(
        self,
    ) -> Result<<store::CachedOxKvStore as store::Store>::Transaction, store::StoreError> {
        let mut guard = self.inner.lock().await;
        guard.take().ok_or_else(|| {
            store::StoreError::Other("transaction already committed or rolled back".into())
        })
    }

    /// Retrieve a value within an active transaction.
    /// # Errors
    /// * `StoreError` - if the transaction was already committed or rolled back, or if an I/O error occurs
    #[wasm_bindgen(return_description = "Raw bytes as a Uint8Array, or null when not found")]
    pub async fn get_bytes(
        &self,
        #[wasm_bindgen(param_description = "The key to retrieve")] key: &str,
    ) -> Result<JsValue, JsValue> {
        let mut guard = self.inner.lock().await;
        let tx = guard.as_mut().ok_or_else(|| {
            store::StoreError::Other("transaction already committed or rolled back".into())
        })?;
        match tx.get_bytes(key).await {
            Ok(Some(bytes)) => {
                let arr = js_sys::Uint8Array::from(&bytes[..]);
                Ok(arr.into())
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Checks if a key exists within an active transaction.
    /// # Errors
    /// * `StoreError` - if the transaction was already committed or rolled back, or if an I/O error occurs
    #[wasm_bindgen(
        return_description = "true when the key exists within the transaction, false otherwise"
    )]
    pub async fn has(
        &self,
        #[wasm_bindgen(param_description = "The key to check for existence")] key: &str,
    ) -> Result<JsValue, JsValue> {
        let mut guard = self.inner.lock().await;
        let tx = guard.as_mut().ok_or_else(|| {
            store::StoreError::Other("transaction already committed or rolled back".into())
        })?;
        match tx.has(key).await {
            Ok(exists) => Ok(JsValue::from(exists)),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a key-value pair within an active transaction.
    /// # Errors
    /// * `StoreError` - if the transaction was already committed or rolled back, or if an I/O error occurs
    #[wasm_bindgen(
        return_description = "Previous value as a Uint8Array if the key already existed, or null if it was newly inserted"
    )]
    pub async fn set_bytes(
        &self,
        #[wasm_bindgen(param_description = "The key to set")] key: &str,
        #[wasm_bindgen(param_description = "Byte array of the value to store under the given key")]
        value: &[u8],
    ) -> Result<JsValue, JsValue> {
        let mut guard = self.inner.lock().await;
        let tx = guard.as_mut().ok_or_else(|| {
            store::StoreError::Other("transaction already committed or rolled back".into())
        })?;
        match tx.set_bytes(key, value).await {
            Ok(Some(prev)) => {
                let arr = js_sys::Uint8Array::from(&prev[..]);
                Ok(arr.into())
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a key to an arbitrary JSON-shaped value within an active transaction.
    /// # Errors
    /// * `StoreError` - if serialization of the value or an I/O error occurs
    #[wasm_bindgen(js_name = "set")]
    pub async fn set(
        &self,
        #[wasm_bindgen(param_description = "The key to set")] key: &str,
        #[wasm_bindgen(param_description = "A JSON-shaped value from JavaScript")] value: JsValue,
    ) -> Result<JsValue, JsValue> {
        let json_value: serde_json::Value = serde_wasm_bindgen::from_value(value)
            .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
        let mut guard = self.inner.lock().await;
        let tx = guard.as_mut().ok_or_else(|| {
            store::StoreError::Other("transaction already committed or rolled back".into())
        })?;
        match tx.set(key, &json_value).await {
            Ok(Some(prev)) => {
                let js_value = json_compatible(&prev)?;
                Ok(js_value)
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieve a JSON-shaped value from within an active transaction.
    /// # Errors
    /// * `StoreError` - if deserialization of the stored value or an I/O error occurs
    #[wasm_bindgen(js_name = "get")]
    pub async fn get(
        &self,
        #[wasm_bindgen(param_description = "The key to retrieve")] key: &str,
    ) -> Result<JsValue, JsValue> {
        let mut guard = self.inner.lock().await;
        let tx = guard.as_mut().ok_or_else(|| {
            store::StoreError::Other("transaction already committed or rolled back".into())
        })?;
        match tx.get::<serde_json::Value>(key).await {
            Ok(Some(value)) => {
                let js_value = json_compatible(&value)?;
                Ok(js_value)
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a key from within an active transaction.
    /// # Errors
    /// * `StoreError` - if the transaction was already committed or rolled back, or if an I/O error occurs
    #[wasm_bindgen(
        return_description = "true if a key existed and was removed; false when no prior value was present"
    )]
    pub async fn delete(
        &self,
        #[wasm_bindgen(param_description = "The key to remove from storage")] key: &str,
    ) -> Result<JsValue, JsValue> {
        let mut guard = self.inner.lock().await;
        let tx = guard.as_mut().ok_or_else(|| {
            store::StoreError::Other("transaction already committed or rolled back".into())
        })?;
        match tx.delete(key).await {
            Ok(deleted) => Ok(JsValue::from(deleted)),
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieve key-value pairs within an active transaction.
    /// # Errors
    /// * `StoreError` - if the transaction was already committed or rolled back, or if an I/O error occurs
    #[wasm_bindgen(return_description = "An array of key-value objects")]
    pub async fn gets_bytes(
        &self,
        #[wasm_bindgen(
            param_description = "Optional maximum number of results to return; omit (None) for all matches"
        )]
        limit: Option<u32>,
        #[wasm_bindgen(param_description = "Sort order for pagination — ascending or descending")]
        direction: Direction,
        #[wasm_bindgen(
            param_description = "Optional start key for the range; keys *at* this cursor are included when present"
        )]
        start_cursor: Option<String>,
        #[wasm_bindgen(
            param_description = "Optional end key for the range; keys *at* this cursor are included when present"
        )]
        end_cursor: Option<String>,
    ) -> Result<Vec<js_sys::Object>, JsValue> {
        let cursor = (start_cursor, end_cursor);

        let mut guard = self.inner.lock().await;
        let tx = guard.as_mut().ok_or_else(|| {
            store::StoreError::Other("transaction already committed or rolled back".into())
        })?;
        match tx.gets_bytes(limit, direction.into(), cursor).await {
            Ok(kvs) => kvs
                .into_iter()
                .map(|value| {
                    let obj = js_sys::Object::new();
                    let js_key = js_sys::JsString::from(value.key);
                    let js_val = js_sys::Uint8Array::from(&value.value[..]);
                    js_sys::Reflect::set(&obj, &"key".into(), &js_key)?;
                    js_sys::Reflect::set(&obj, &"value".into(), &js_val)?;
                    Ok(obj)
                })
                .collect::<Result<_, JsValue>>(),
            Err(e) => Err(e.into()),
        }
    }

    /// Commit the transaction, making all staged changes permanent.
    /// # Errors
    /// * `StoreError` - if an I/O error occurs while committing the transaction
    #[wasm_bindgen(return_description = "Undefined on success")]
    pub async fn commit(self) -> Result<JsValue, JsValue> {
        let tx = self.take_tx().await?;
        match tx.commit().await {
            Ok(()) => Ok(JsValue::undefined()),
            Err(e) => Err(e.into()),
        }
    }

    /// Rollback the transaction, discarding every staged change.
    /// # Errors
    /// * `StoreError` - if an I/O error occurs while rolling back the transaction
    #[wasm_bindgen(return_description = "Undefined on success")]
    pub async fn rollback(self) -> Result<JsValue, JsValue> {
        let tx = self.take_tx().await?;
        match tx.rollback().await {
            Ok(()) => Ok(JsValue::undefined()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(all(test, feature = "oxkv"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use wasm_bindgen_test::*;

    #[cfg(feature = "btree")]
    use super::super::btree::JsBTreeStore;
    use super::*;

    fn to_bytes(value: &JsValue) -> Vec<u8> {
        assert!(!value.is_null(), "expected a Uint8Array, got null");
        js_sys::Uint8Array::new(value).to_vec()
    }

    fn ok_bytes(result: Result<JsValue, JsValue>) -> Option<Vec<u8>> {
        let value = result.expect("operation failed");
        if value.is_null() {
            None
        } else {
            Some(to_bytes(&value))
        }
    }

    fn ok(result: Result<JsValue, JsValue>) -> JsValue {
        result.expect("operation failed")
    }

    #[wasm_bindgen_test]
    async fn cached_save_load_stream_roundtrip() {
        use futures::stream::{self, StreamExt};

        let js_store = new_cached().await;
        for n in 0..5u32 {
            ok(js_store
                .set_bytes(&format!("k{n}"), format!("v{n}").as_bytes())
                .await);
        }

        // Drain the JS ReadableStream back into Rust chunks.
        let stream = js_store.save_stream();
        let readable = wasm_streams::readable::ReadableStream::from_raw(stream);
        let mut chunks = readable.into_stream();
        let mut reassembled: Vec<Vec<u8>> = Vec::new();
        while let Some(item) = chunks.next().await {
            let value = item.expect("chunk read failed");
            let array = value
                .dyn_into::<js_sys::Uint8Array>()
                .expect("chunk is not a Uint8Array");
            reassembled.push(array.to_vec());
        }

        // Chunks must concatenate to exactly what save() produces.
        let expected = to_bytes(&ok(js_store.save().await));
        let total: Vec<u8> = reassembled.concat();
        assert!(!reassembled.is_empty());
        assert_eq!(total, expected);

        // Feed the same chunks (re-wrapped as a fresh ReadableStream) into a
        // new store and confirm the round trip restores every entry.
        let restored = new_cached().await;
        let input = wasm_streams::readable::ReadableStream::from_stream(stream::iter(
            reassembled
                .into_iter()
                .map(|chunk| Ok::<_, JsValue>(js_sys::Uint8Array::from(&chunk[..]).into())),
        ))
        .into_raw();
        let count = restored
            .load_stream(input)
            .await
            .expect("load_stream failed");
        assert_eq!(count.as_f64(), Some(f64::from(5)));

        for n in 0..5u32 {
            assert_eq!(
                ok_bytes(restored.get_bytes(&format!("k{n}")).await),
                Some(format!("v{n}").into_bytes())
            );
        }
    }

    async fn new_cached() -> JsCachedOxKvStore {
        JsCachedOxKvStore::create(None, None)
            .await
            .expect("create LSM store")
    }

    async fn begin_cached_tx(js_store: &JsCachedOxKvStore) -> JsCachedOxKvTx {
        let guard = js_store.inner.lock().await;
        let tx = guard.begin_tx().expect("begin_tx failed");
        JsCachedOxKvTx {
            inner: std::sync::Arc::new(futures::lock::Mutex::new(Some(tx))),
        }
    }

    #[wasm_bindgen_test]
    async fn cached_crud_roundtrip() {
        let js_store = new_cached().await;

        assert_eq!(ok_bytes(js_store.set_bytes("k", b"v1").await), None);
        assert_eq!(
            ok_bytes(js_store.set_bytes("k", b"v2").await),
            Some(b"v1".to_vec())
        );
        assert_eq!(
            ok_bytes(js_store.get_bytes("k").await),
            Some(b"v2".to_vec())
        );
        assert!(ok(js_store.has("k").await).as_bool().unwrap_or(false));

        let deleted = ok(js_store.delete("k").await)
            .as_bool()
            .expect("delete should return a boolean");
        assert!(deleted);
        assert!(ok(js_store.get_bytes("k").await).is_null());
    }

    #[wasm_bindgen_test]
    async fn cached_tx_commit_staged() {
        let js_store = new_cached().await;
        let tx = begin_cached_tx(&js_store).await;
        ok(tx.set_bytes("tx-k", b"tx-v").await);
        // Invisible outside before commit.
        assert!(ok(js_store.get_bytes("tx-k").await).is_null());
        ok(tx.commit().await);
        assert_eq!(
            ok_bytes(js_store.get_bytes("tx-k").await),
            Some(b"tx-v".to_vec())
        );
    }

    #[wasm_bindgen_test]
    async fn cached_save_load_roundtrip() {
        let js_store = new_cached().await;
        ok(js_store.set_bytes("a", b"1").await);
        ok(js_store.set_bytes("b", b"2").await);
        let snapshot = to_bytes(&ok(js_store.save().await));

        let restored = new_cached().await;
        let count = ok(restored.load(&snapshot).await)
            .as_f64()
            .expect("load returns a count") as u32;
        assert_eq!(count, 2);
        assert_eq!(ok_bytes(restored.get_bytes("a").await), Some(b"1".to_vec()));
    }

    #[cfg(feature = "btree")]
    #[wasm_bindgen_test]
    async fn snapshot_portable_btree_to_cached() {
        let btree = JsBTreeStore::new();
        ok(btree.set_bytes("shared", b"payload").await);
        let snapshot = to_bytes(&ok(btree.save().await));

        let lsm = new_cached().await;
        let count = ok(lsm.load(&snapshot).await)
            .as_f64()
            .expect("load returns a count") as u32;
        assert_eq!(count, 1);
        assert_eq!(
            ok_bytes(lsm.get_bytes("shared").await),
            Some(b"payload".to_vec())
        );
    }

    #[wasm_bindgen_test]
    async fn cached_persistent_roundtrip_survives_reopen() {
        // OPFS exists only in browsers; Node runs the rest of the suite.
        // (Browser CI executes this test for real.)
        if web_sys::window().is_none() {
            return;
        }
        let prefix = Some("persist-smoke".to_string());
        let js_store = JsCachedOxKvStore::create_persistent(prefix.clone(), None)
            .await
            .expect("open persistent store");
        ok(js_store.set_bytes("k", b"v").await);
        drop(js_store);

        // Reopen on the same prefix: OPFS files (not memory) serve the read.
        let reopened = JsCachedOxKvStore::create_persistent(prefix, None)
            .await
            .expect("reopen persistent store");
        assert_eq!(ok_bytes(reopened.get_bytes("k").await), Some(b"v".to_vec()));
        ok(reopened.delete("k").await);
    }
}
