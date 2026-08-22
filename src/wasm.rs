use wasm_bindgen::prelude::*;

use crate::store::{self, GetSet, GetSetExt, Store, StoreExt, Transaction};

/// Entry point for the WASM module.
#[wasm_bindgen(start)]
fn init() {
    console_error_panic_hook::set_once();
}

/// Direction for cursor-based pagination.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Traverse keys in ascending order (from the start cursor or from the beginning).
    Next,
    /// Traverse keys in descending order (from the end cursor or from the end).
    Prev,
}

impl From<Direction> for store::Direction {
    fn from(value: Direction) -> Self {
        match value {
            Direction::Next => store::Direction::Next,
            Direction::Prev => store::Direction::Prev,
        }
    }
}

impl From<store::StoreError> for JsValue {
    fn from(value: store::StoreError) -> Self {
        JsError::new(value.to_string().as_str()).into()
    }
}

// Manual wasm-bindgen wrappers for BTreeStore — inlined from `make_wasm_store!` macro.

/// Wrapper around the concrete [`store::BTreeStore`] for use in a WASM environment.
///
/// The wrapper holds an `Arc<Mutex<BTreeStore>>` so that multiple concurrent
/// JavaScript calls share one underlying store without needing to copy it. Each
/// method acquires the lock, runs the operation (async), and releases before returning,
/// giving callers a simple promise-based API.
#[wasm_bindgen(js_name = BTreeStore)]
pub struct JsBTreeStore {
    inner: std::sync::Arc<futures::lock::Mutex<store::BTreeStore>>,
}

impl Default for JsBTreeStore {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen(js_class = BTreeStore)]
impl JsBTreeStore {
    /// Create a new WASM store instance.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(futures::lock::Mutex::new(store::BTreeStore::default())),
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
        let mut store = self.inner.lock().await;
        match store.set_bytes(key, value).await {
            Ok(Some(prev)) => {
                let arr = js_sys::Uint8Array::from(&prev[..]);
                Ok(arr.into())
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a key to an arbitrary JSON-shaped value. Accepts any `JsValue` from JavaScript —
    /// objects, arrays, strings, numbers, booleans, or nested structures. The value is serialized
    /// with `serde_json`, stored as raw bytes, and the previous value (if any) is returned as a
    /// deserialized `Option<T>`.
    ///
    /// This is the JSON-level counterpart to [`set_bytes`]; use it when you want to work with
    /// typed Rust structs instead of raw byte arrays.
    ///
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
        let mut store = self.inner.lock().await;
        match store.set(key, &json_value).await {
            Ok(Some(prev)) => {
                let js_value = serde_wasm_bindgen::to_value(&prev)
                    .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
                Ok(js_value)
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieve a JSON-shaped value from the store. Accepts any `JsValue` shape — objects, arrays,
    /// strings, numbers, booleans, or nested structures. The stored bytes are deserialized into
    /// an arbitrary `serde_json::Value`, which can be further cast to a typed struct on the caller
    /// side with `.as_object()`, `.as_array()`, etc., or via `serde` from JavaScript.
    ///
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
                let js_value = serde_wasm_bindgen::to_value(&value)
                    .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
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
        let mut store = self.inner.lock().await;
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
    ///
    /// Mirrors `gets_bytes` (`limit`, `direction`, and cursors carry the same
    /// semantics). When `query` is omitted this is a pass-through to
    /// `gets_bytes`. With a query, entries are scanned in the requested order
    /// and only those whose stored bytes deserialize as JSON satisfying the
    /// query are returned; non-JSON entries are skipped. When a query is given,
    /// `limit` caps the number of *matching* entries.
    ///
    /// See the `query` module for query syntax (field paths, ranges, wildcards,
    /// regex, fuzzy, boolean operators).
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
                    // With no query, raw (non-JSON) values may be returned; fall
                    // back to bytes in that case so nothing is silently dropped.
                    let js_val =
                        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&kv.value) {
                            serde_wasm_bindgen::to_value(&value)?
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
        let mut store = self.inner.lock().await;
        match store.begin_tx() {
            Ok(tx) => {
                let js_tx = JsBTreeTx {
                    inner: std::sync::Arc::new(futures::lock::Mutex::new(Some(tx))),
                };
                Ok(js_tx.into())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Serializes all key-value pairs into a single contiguous `Uint8Array`.
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
        let mut store = self.inner.lock().await;
        match store.load(data).await {
            Ok(count) => {
                let count_u32 = u32::try_from(count)
                    .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
                Ok(JsValue::from(count_u32))
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Wrapper around the [`Store::BTreeTx`] produced by [`begin_tx`].
#[wasm_bindgen(js_name = BTreeTx)]
#[derive(Clone)]
pub struct JsBTreeTx {
    inner: std::sync::Arc<
        futures::lock::Mutex<Option<<crate::BTreeStore as store::Store>::Transaction>>,
    >,
}

#[wasm_bindgen(js_class = BTreeTx)]
impl JsBTreeTx {
    async fn take_tx(
        self,
    ) -> Result<<crate::BTreeStore as store::Store>::Transaction, store::StoreError> {
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

    /// Set a key to an arbitrary JSON-shaped value within an active transaction. Accepts any `JsValue` from JavaScript — objects, arrays, strings, numbers, booleans, or nested structures. The value is serialized with `serde_json`, stored as raw bytes, and the previous value (if any) is returned as a deserialized `Option<T>`.
    ///
    /// This is the JSON-level counterpart to [`set_bytes`]; use it when you want to work with typed Rust structs instead of raw byte arrays.
    ///
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
                let js_value = serde_wasm_bindgen::to_value(&prev)
                    .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
                Ok(js_value)
            }
            Ok(None) => Ok(JsValue::null()),
            Err(e) => Err(e.into()),
        }
    }

    /// Retrieve a JSON-shaped value from within an active transaction. Returns the stored value deserialized into an arbitrary `serde_json::Value` (objects, arrays, strings, numbers, booleans, or nested structures), or null when not found.
    ///
    /// This is the JSON-level counterpart to [`get_bytes`]; use it when you want typed access instead of raw byte arrays.
    ///
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
                let js_value = serde_wasm_bindgen::to_value(&value)
                    .map_err(|e| store::StoreError::Serialization(e.to_string()))?;
                Ok(js_value)
            }
            Ok(None) => Ok(JsValue::null()),
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
