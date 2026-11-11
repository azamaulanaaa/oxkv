//! WASM bindings for [`store::CachedOxKvStore`], the write-through RAM mirror.
//!
//! The JS names match the Rust names (`CachedOxKvStore`, `CachedOxKvTx`).

use wasm_bindgen::prelude::*;

use crate::store::{self, GetSet, GetSetExt, Store, StoreExt, Transaction, load_stream};

use super::{Direction, json_compatible};

/// Wrapper around the write-through [`store::CachedOxKvStore`] for WASM.
///
/// This store has the same API as [`JsOxKvStore`](super::oxkv::JsOxKvStore). It
/// mirrors every key in memory. Reads serve without storage I/O. Writes keep WAL
/// durability. An in-memory [`store::MemStorage`] backs the store, so the
/// contents live only as long as the page. The store has no browser persistence
/// yet. OPFS arrives later as another [`store::Storage`] backend. The snapshots
/// are byte-identical with the snapshots of every other backend, so `save`
/// bytes restore anywhere via `load`.
///
/// The wrapper holds an `Arc<Mutex<CachedOxKvStore>>`. This lets multiple
/// JavaScript calls share one underlying store. Each method acquires the lock,
/// runs the operation (async), then releases the lock before it returns.
#[wasm_bindgen(js_name = CachedOxKvStore)]
pub struct JsCachedOxKvStore {
    inner: std::sync::Arc<futures::lock::Mutex<store::CachedOxKvStore>>,
}

#[wasm_bindgen(js_class = CachedOxKvStore)]
impl JsCachedOxKvStore {
    /// Parses a fill-policy name into its [`store::WarmMode`].
    ///
    /// This function accepts `eager`, `background`, and `lazy` in exact
    /// lowercase. Any other name produces a [`store::StoreError`], which the
    /// caller surfaces as a rejection.
    ///
    /// The `create` functions call this function to keep their bodies linear.
    /// JavaScript does not call this function.
    fn parse_warm_mode(name: Option<&str>) -> Result<store::WarmMode, JsValue> {
        match name {
            None | Some("eager" | "Eager" | "EAGER") => Ok(store::WarmMode::Eager),
            Some("background") => Ok(store::WarmMode::Background),
            Some("lazy") => Ok(store::WarmMode::Lazy),
            Some(other) => Err(store::StoreError::Other(format!(
                "unknown warm mode `{other}`: expected eager, background, or lazy"
            ))
            .into()),
        }
    }
    /// Create a new in-memory cached store (`MemStorage`, probe skipped).
    ///
    /// This function acquires ownership and warms every key into memory before
    /// it resolves. From then on the instance behaves like any other
    /// [`store::Store`] with zero-I/O reads.
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
        #[wasm_bindgen(
            param_description = "Fill policy: eager, background, or lazy; defaults to eager"
        )]
        warm_mode: Option<String>,
    ) -> Result<JsCachedOxKvStore, JsValue> {
        let prefix = prefix.unwrap_or_else(|| "js-lsm".to_string());
        let backend = std::sync::Arc::new(store::MemStorage::new());
        let ttl = std::time::Duration::from_millis(stale_ttl_ms.unwrap_or(1000));
        let mode = Self::parse_warm_mode(warm_mode.as_deref())?;
        match store::OxKvStore::builder()
            .with_store(backend)
            .with_prefix(store::ObjectPath::from(prefix))
            .skip_probe(true)
            .build()
            .await
        {
            Ok(store) => Ok(Self {
                inner: std::sync::Arc::new(futures::lock::Mutex::new(
                    store::CachedOxKvStore::open_with_mode(store, mode)
                        .await?
                        .with_stale_ttl(ttl),
                )),
            }),
            Err(e) => Err(e.into()),
        }
    }

    /// Create a persistent LSM store backed by origin private storage (OPFS).
    ///
    /// This store has the same API as [`create`](Self::create). Its contents
    /// survive page reloads. The objects live as real files under one `oxkv`
    /// OPFS directory. Content-hash etags let fencing and CAS work across
    /// sessions. The storage probe runs on every open, so a broken backend fails
    /// fast instead of corrupting data. This store works on the main thread
    /// only. Cross-tab races resolve as last-writer-wins.
    /// # Errors
    /// * `StoreError` - if OPFS is unavailable/denied
    /// * `StoreError` - if the store fails to initialize
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
        #[wasm_bindgen(
            param_description = "Fill policy: eager, background, or lazy; defaults to eager"
        )]
        warm_mode: Option<String>,
    ) -> Result<JsCachedOxKvStore, JsValue> {
        let prefix = prefix.unwrap_or_else(|| "js-lsm".to_string());
        let backend = std::sync::Arc::new(store::OpfsStorage::open().await?);
        let ttl = std::time::Duration::from_millis(stale_ttl_ms.unwrap_or(1000));
        let mode = Self::parse_warm_mode(warm_mode.as_deref())?;
        match store::OxKvStore::builder()
            .with_store(backend)
            .with_prefix(store::ObjectPath::from(prefix))
            .build()
            .await
        {
            Ok(store) => Ok(Self {
                inner: std::sync::Arc::new(futures::lock::Mutex::new(
                    store::CachedOxKvStore::open_with_mode(store, mode)
                        .await?
                        .with_stale_ttl(ttl),
                )),
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

    /// Retrieve a value by key. It first revalidates the mirror when the
    /// staleness bound has elapsed. Plain [`get_bytes`](Self::get_bytes) is
    /// zero-I/O. Use this method when another tab may have taken the epoch.
    /// # Errors
    /// * `StoreError` - if the revalidation fails
    /// * `StoreError` - if the mirror is fenced
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

    /// Apply every missing generation to the mirror.
    ///
    /// Same-owner appends replay incrementally. A new ownership epoch rebuilds
    /// from a full scan instead. A missed flush window also rebuilds from a full
    /// scan instead.
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

    /// Report whether the mirror lags the durable core.
    ///
    /// This method polls `manifest.json` conditionally. This method does not
    /// read the ownership.
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

    /// Report whether the full dataset is mirrored.
    ///
    /// This method always returns true for eagerly opened handles. For background
    /// and lazy handles, it returns true after `warm` completes the scan. It also
    /// returns true after enough `warmStep` calls complete the scan.
    #[wasm_bindgen(
        js_name = "isWarmed",
        return_description = "true once every key is mirrored"
    )]
    pub async fn is_warmed(&self) -> bool {
        let store = self.inner.lock().await;
        store.is_warmed().await
    }

    /// Scan the whole store into the mirror. The scan continues until it has
    /// read all pages.
    ///
    /// This method converges background and lazy handles to the eager steady
    /// state.
    /// # Errors
    /// * `StoreError` - if the manifest, a WAL file, or the scan fails
    #[wasm_bindgen(return_description = "Number of key records applied to the mirror")]
    pub async fn warm(&self) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.warm().await {
            Ok(applied) => {
                let count = u32::try_from(applied)
                    .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
                Ok(JsValue::from(count))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Advance an explicitly driven scan by up to `pages` SST pages.
    ///
    /// Call this method from a timer to converge a background handle without
    /// blocking the creation of the handle. This method returns true once every
    /// key is mirrored.
    /// # Errors
    /// * `StoreError` - if the manifest, a WAL file, or the scan fails
    #[wasm_bindgen(
        js_name = "warmStep",
        return_description = "true once every key is mirrored"
    )]
    pub async fn warm_step(
        &self,
        #[wasm_bindgen(param_description = "Maximum SST pages to scan in this step")] pages: u32,
    ) -> Result<JsValue, JsValue> {
        let store = self.inner.lock().await;
        match store.warm_step(pages).await {
            Ok(done) => Ok(JsValue::from(done)),
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
    /// * `StoreError` - if serialization of the value fails
    /// * `StoreError` - if an I/O error occurs
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
    /// * `StoreError` - if deserialization of the stored value fails
    /// * `StoreError` - if an I/O error occurs
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

    /// Retrieve JSON documents with cursor-based pagination. A Lucene-style
    /// query string can filter the result.
    /// # Errors
    /// * `StoreError` - if the query is invalid
    /// * `StoreError` - if an I/O error occurs
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

    /// Serialize all key-value pairs into a single contiguous `Uint8Array`.
    ///
    /// The bytes are byte-identical with the bytes of every other backend. The
    /// bytes restore anywhere via `load`. This includes `BTreeStore` and native
    /// `CachedOxKvStore`.
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
    /// * `StoreError` - if the binary payload is invalid
    /// * `StoreError` - if the storage fails
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
    /// Chunks concatenate to exactly what [`save`](Self::save) returns. Chunk
    /// boundaries always fall between whole records, so each chunk can be
    /// decoded independently downstream. For example, a caller can pipe a chunk
    /// straight into a file or a fetch upload.
    ///
    /// The stream reads lazily. It clones the store handle out of the mutex, so
    /// no other method on this handle is blocked for the lifetime of the
    /// stream.
    ///
    /// # Errors
    /// * `StoreError` - if retrieval fails while streaming
    #[wasm_bindgen(
        return_description = "A ReadableStream of Uint8Array chunks containing the serialized store"
    )]
    pub fn save_stream(&self) -> web_sys::ReadableStream {
        use futures::{SinkExt, StreamExt, TryStreamExt};

        // The core `save_stream` borrows its source, but JS streams must own
        // their data for the `'static` lifetime. A background task drives the
        // borrowed stream through a bounded channel. The channel also provides
        // backpressure. The task fetches the next page only after the consumer
        // drains a chunk. When the consumer cancels, the sink errors out, the
        // task exits, and the lock is released.
        let (mut sender, receiver) = futures::channel::mpsc::channel::<store::Result<Vec<u8>>>(16);
        let inner = std::sync::Arc::clone(&self.inner);
        wasm_bindgen_futures::spawn_local(async move {
            // This task clones out of the mutex instead of holding the guard.
            // A consumer that stalls on a full channel would otherwise park
            // this task while it holds the store lock. That stall would freeze
            // every other method on the handle until the task resumed or the
            // consumer cancelled.
            let store = inner.lock().await.clone();
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
    /// Chunk boundaries are arbitrary. A chunk may split a record. Decoding is
    /// incremental, so memory stays bounded regardless of the payload size.
    /// Typical sources are `File.stream()`, `fetch()` bodies, and the stream
    /// returned by [`save_stream`](Self::save_stream).
    ///
    /// # Errors
    /// * `StoreError` - if a chunk fails to decode as bytes
    /// * `StoreError` - if the payload is malformed or truncated
    /// * `StoreError` - if the storage fails
    ///
    /// On error nothing is committed.
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

// Generated wrapper. See `js_tx!` in `wasm/mod.rs`.
js_tx!(
    JsCachedOxKvTx,
    "CachedOxKvTx",
    store::CachedOxKvStore,
    "Transaction handle for [`JsCachedOxKvStore`]: staged overlay, durable only on `commit`."
);

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

        // Feed the same chunks into a new store. The chunks are re-wrapped as
        // a fresh `ReadableStream`. The round trip must restore every entry.
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
        JsCachedOxKvStore::create(None, None, None)
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
    async fn cached_lazy_warms_on_demand() {
        let js_store = JsCachedOxKvStore::create(None, None, Some("lazy".to_string()))
            .await
            .expect("create lazy store");
        assert!(!js_store.is_warmed().await);
        ok(js_store.set_bytes("k", b"v").await);
        assert_eq!(ok_bytes(js_store.get_bytes("k").await), Some(b"v".to_vec()));
        assert!(ok(js_store.warm_step(16).await).as_bool().unwrap_or(false));
        assert!(js_store.is_warmed().await);
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
        // OPFS exists only in browsers. Node runs the rest of the suite.
        // Browser CI executes this test for real.
        if web_sys::window().is_none() {
            crate::wasm::announce_skip(
                "cached_persistent_roundtrip_survives_reopen",
                "no window/OPFS under Node; executed for real by CI's `wasm-pack test --headless --chrome` job",
            );
            return;
        }
        let prefix = Some("persist-smoke".to_string());
        let js_store = JsCachedOxKvStore::create_persistent(prefix.clone(), None, None)
            .await
            .expect("open persistent store");
        ok(js_store.set_bytes("k", b"v").await);
        drop(js_store);

        // Reopen on the same prefix. The OPFS files serve the read, not memory.
        let reopened = JsCachedOxKvStore::create_persistent(prefix, None, None)
            .await
            .expect("reopen persistent store");
        assert_eq!(ok_bytes(reopened.get_bytes("k").await), Some(b"v".to_vec()));
        ok(reopened.delete("k").await);
    }
}
