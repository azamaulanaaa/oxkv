use serde::Serialize;
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

/// Serializes a value into a plain, JSON-compatible `JsValue`.
///
/// Unlike [`serde_wasm_bindgen::to_value`], which encodes maps as ES6 `Map`
/// objects (opaque `{}` to JavaScript property access and `JSON.stringify`),
/// this produces plain objects so callers see real JSON documents.
fn json_compatible<T: Serialize>(value: &T) -> Result<JsValue, store::StoreError> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| store::StoreError::Serialization(e.to_string()))
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
                let js_value = json_compatible(&prev)?;
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
                let js_value = json_compatible(&prev)?;
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
                let js_value = json_compatible(&value)?;
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

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::*;

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

    fn entries(result: Result<Vec<js_sys::Object>, JsValue>) -> Vec<js_sys::Object> {
        result.expect("operation failed")
    }

    fn entry_key(entry: &js_sys::Object) -> String {
        let key = js_sys::Reflect::get(entry, &"key".into()).expect("missing key field");
        key.as_string().expect("key is not a string")
    }

    fn entry_bytes(entry: &js_sys::Object) -> Vec<u8> {
        let value = js_sys::Reflect::get(entry, &"value".into()).expect("missing value field");
        to_bytes(&value)
    }

    fn entry_json(entry: &js_sys::Object) -> serde_json::Value {
        let value = js_sys::Reflect::get(entry, &"value".into()).expect("missing value field");
        serde_wasm_bindgen::from_value(value).expect("value is not JSON")
    }

    async fn begin_test_tx(js_store: &JsBTreeStore) -> JsBTreeTx {
        let mut guard = js_store.inner.lock().await;
        let tx = guard.begin_tx().expect("begin_tx failed");
        JsBTreeTx {
            inner: std::sync::Arc::new(futures::lock::Mutex::new(Some(tx))),
        }
    }

    fn snapshot_entries(snapshot: &[u8]) -> Vec<(String, Vec<u8>)> {
        fn take<'a>(data: &mut &'a [u8], len: usize, message: &str) -> &'a [u8] {
            assert!(data.len() >= len, "{message}");
            let (head, tail) = data.split_at(len);
            *data = tail;
            head
        }

        let mut entries = Vec::new();
        let mut data = snapshot;

        while !data.is_empty() {
            let key_len = u32::from_le_bytes(
                take(&mut data, 4, "truncated key length header in snapshot")
                    .try_into()
                    .expect("exactly 4 bytes"),
            ) as usize;
            let key =
                std::str::from_utf8(take(&mut data, key_len, "truncated key data in snapshot"))
                    .expect("snapshot key is UTF-8")
                    .to_owned();
            let value_len = u32::from_le_bytes(
                take(&mut data, 4, "truncated value length header in snapshot")
                    .try_into()
                    .expect("exactly 4 bytes"),
            ) as usize;
            entries.push((
                key,
                take(&mut data, value_len, "truncated value data in snapshot").to_vec(),
            ));
        }

        entries
    }

    #[wasm_bindgen_test]
    fn direction_maps_to_store_direction() {
        assert_eq!(
            store::Direction::from(Direction::Next),
            store::Direction::Next
        );
        assert_eq!(
            store::Direction::from(Direction::Prev),
            store::Direction::Prev
        );
    }

    #[wasm_bindgen_test]
    fn store_error_converts_to_js_error() {
        let value = JsValue::from(store::StoreError::Other("boom".to_owned()));
        assert!(value.is_instance_of::<js_sys::Error>());
        let message = js_sys::Reflect::get(&value, &"message".into())
            .expect("error has no message")
            .as_string()
            .expect("message is not a string");
        assert_eq!(message, "boom");
    }

    #[wasm_bindgen_test]
    async fn get_missing_key_returns_null() {
        let js_store = JsBTreeStore::new();
        let result = js_store.get_bytes("absent").await;
        assert!(result.expect("get failed").is_null());
    }

    #[wasm_bindgen_test]
    async fn set_then_get_bytes_roundtrip() {
        let js_store = JsBTreeStore::new();

        let inserted = ok_bytes(js_store.set_bytes("k", b"v1").await);
        assert_eq!(inserted, None);

        let previous = ok_bytes(js_store.set_bytes("k", b"v2").await);
        assert_eq!(previous, Some(b"v1".to_vec()));

        assert_eq!(
            ok_bytes(js_store.get_bytes("k").await),
            Some(b"v2".to_vec())
        );
    }

    #[wasm_bindgen_test]
    async fn delete_reports_presence() {
        let js_store = JsBTreeStore::new();
        ok(js_store.set_bytes("k", b"v").await);

        let deleted = ok(js_store.delete("k").await)
            .as_bool()
            .expect("delete should return a boolean");
        assert!(deleted);

        let deleted_again = ok(js_store.delete("k").await)
            .as_bool()
            .expect("delete should return a boolean");
        assert!(!deleted_again);
    }

    #[wasm_bindgen_test]
    async fn json_set_get_roundtrip() {
        let js_store = JsBTreeStore::new();
        let doc = serde_json::json!({ "name": "oxkv", "tags": ["a", "b"], "count": 2 });
        let js_doc = json_compatible(&doc).expect("serialize to JsValue");

        let inserted = ok(js_store.set("doc", js_doc).await);
        assert!(inserted.is_null());

        let loaded = ok(js_store.get("doc").await);
        let roundtripped: serde_json::Value =
            serde_wasm_bindgen::from_value(loaded).expect("deserialize from JsValue");
        assert_eq!(roundtripped, doc);
    }

    #[wasm_bindgen_test]
    async fn get_returns_plain_json_object_readable_from_js() {
        let js_store = JsBTreeStore::new();
        let doc = json_compatible(&serde_json::json!({ "name": "Ada", "age": 36 }))
            .expect("serialize to JsValue");
        ok(js_store.set("u", doc).await);

        let loaded = ok(js_store.get("u").await);
        assert!(
            !loaded.is_instance_of::<js_sys::Map>(),
            "objects must come back as plain JSON objects, not ES6 Maps"
        );
        let name = js_sys::Reflect::get(&loaded, &"name".into())
            .expect("object has no name property")
            .as_string()
            .expect("name is not a string");
        assert_eq!(name, "Ada");

        // JSON.stringify is what JS consumers and schema validators effectively
        // see; a Map would serialize to "{}" here.
        let serialized = js_sys::JSON::stringify(&loaded).expect("stringify failed");
        let roundtripped: serde_json::Value =
            serde_json::from_str(&serialized.as_string().expect("serialized is not a string"))
                .expect("stringified value is not JSON");
        assert_eq!(
            roundtripped,
            serde_json::json!({ "name": "Ada", "age": 36 }),
            "JSON.stringify must show the full document"
        );
    }

    #[wasm_bindgen_test]
    async fn json_set_returns_previous_document() {
        let js_store = JsBTreeStore::new();

        let first = json_compatible(&serde_json::json!({ "n": 1 })).expect("serialize to JsValue");
        ok(js_store.set("k", first).await);

        let second = json_compatible(&serde_json::json!({ "n": 2 })).expect("serialize to JsValue");
        let previous = ok(js_store.set("k", second).await);
        let previous: serde_json::Value =
            serde_wasm_bindgen::from_value(previous).expect("deserialize from JsValue");
        assert_eq!(previous, serde_json::json!({ "n": 1 }));
    }

    #[wasm_bindgen_test]
    async fn json_set_invalid_value_fails() {
        let js_store = JsBTreeStore::new();
        // Functions cannot be converted to serde_json values.
        let invalid = js_sys::Function::new_no_args("");
        assert!(js_store.set("k", invalid.into()).await.is_err());
    }

    #[wasm_bindgen_test]
    async fn gets_bytes_orders_and_paginates() {
        let js_store = JsBTreeStore::new();
        for (key, value) in [("a", &b"1"[..]), ("b", &b"2"[..]), ("c", &b"3"[..])] {
            ok(js_store.set_bytes(key, value).await);
        }

        let all = entries(js_store.gets_bytes(None, Direction::Next, None, None).await);
        let keys: Vec<_> = all.iter().map(entry_key).collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
        assert_eq!(entry_bytes(&all[0]), b"1");

        let limited = entries(
            js_store
                .gets_bytes(Some(2), Direction::Next, None, None)
                .await,
        );
        let keys: Vec<_> = limited.iter().map(entry_key).collect();
        assert_eq!(keys, vec!["a", "b"]);

        let descending = entries(
            js_store
                .gets_bytes(None, Direction::Prev, Some("c".to_owned()), None)
                .await,
        );
        let keys: Vec<_> = descending.iter().map(entry_key).collect();
        assert_eq!(keys, vec!["c", "b", "a"]);

        let range = entries(
            js_store
                .gets_bytes(
                    None,
                    Direction::Next,
                    Some("a".to_owned()),
                    Some("b".to_owned()),
                )
                .await,
        );
        let keys: Vec<_> = range.iter().map(entry_key).collect();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[wasm_bindgen_test]
    async fn gets_bytes_matches_store_cursor_edge_cases() {
        let js_store = JsBTreeStore::new();
        for key in ["a", "b", "c"] {
            ok(js_store.set_bytes(key, key.as_bytes()).await);
        }

        // Mirrors btree.rs::test_gets_prev_without_start_returns_empty
        let descending = entries(js_store.gets_bytes(None, Direction::Prev, None, None).await);
        assert!(descending.is_empty());

        // Mirrors btree.rs::test_gets_prev_with_start_less_than_end_returns_empty
        let inverted = entries(
            js_store
                .gets_bytes(
                    None,
                    Direction::Prev,
                    Some("a".to_owned()),
                    Some("c".to_owned()),
                )
                .await,
        );
        assert!(inverted.is_empty());

        // Mirrors btree.rs::test_gets_next_invalid_range
        let invalid = entries(
            js_store
                .gets_bytes(
                    None,
                    Direction::Next,
                    Some("c".to_owned()),
                    Some("a".to_owned()),
                )
                .await,
        );
        assert!(invalid.is_empty());

        // Mirrors btree.rs::test_gets_range_end_only
        let up_to = entries(
            js_store
                .gets_bytes(None, Direction::Next, None, Some("b".to_owned()))
                .await,
        );
        let keys: Vec<_> = up_to.iter().map(entry_key).collect();
        assert_eq!(keys, vec!["a", "b"]);

        // Mirrors btree.rs::test_gets_prev_with_limit
        let limited_descending = entries(
            js_store
                .gets_bytes(Some(2), Direction::Prev, Some("c".to_owned()), None)
                .await,
        );
        let keys: Vec<_> = limited_descending.iter().map(entry_key).collect();
        assert_eq!(keys, vec!["c", "b"]);
    }

    #[wasm_bindgen_test]
    async fn gets_without_query_returns_all_documents() {
        let js_store = JsBTreeStore::new();
        for n in 0..3u32 {
            let doc =
                json_compatible(&serde_json::json!({ "n": n })).expect("serialize to JsValue");
            ok(js_store.set(&format!("k{n}"), doc).await);
        }

        let all = entries(js_store.gets(None, Direction::Next, None, None, None).await);
        assert_eq!(all.len(), 3);
        assert_eq!(entry_json(&all[0]), serde_json::json!({ "n": 0 }));

        let filtered = entries(
            js_store
                .gets(None, Direction::Next, None, None, Some("n:1".to_owned()))
                .await,
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(entry_json(&filtered[0]), serde_json::json!({ "n": 1 }));
    }

    #[wasm_bindgen_test]
    async fn gets_with_query_limits_matches_not_scanned_entries() {
        let js_store = JsBTreeStore::new();
        for n in 0..4u32 {
            let doc = json_compatible(&serde_json::json!({ "even": n % 2 == 0 }))
                .expect("serialize to JsValue");
            ok(js_store.set(&format!("k{n}"), doc).await);
        }

        let matches = entries(
            js_store
                .gets(
                    Some(1),
                    Direction::Next,
                    None,
                    None,
                    Some("even:true".to_owned()),
                )
                .await,
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(entry_key(&matches[0]), "k0");
    }

    #[wasm_bindgen_test]
    async fn transaction_changes_hidden_until_commit() {
        let js_store = JsBTreeStore::new();
        ok(js_store.set_bytes("base", b"original").await);

        let tx = begin_test_tx(&js_store).await;

        ok(tx.set_bytes("staged", b"hidden").await);
        ok(tx.delete("base").await);

        assert_eq!(
            ok_bytes(js_store.get_bytes("base").await),
            Some(b"original".to_vec())
        );
        assert_eq!(ok_bytes(js_store.get_bytes("staged").await), None);

        ok(tx.commit().await);

        assert_eq!(ok_bytes(js_store.get_bytes("base").await), None);
        assert_eq!(
            ok_bytes(js_store.get_bytes("staged").await),
            Some(b"hidden".to_vec())
        );
    }

    #[wasm_bindgen_test]
    async fn transaction_rollback_discards_changes() {
        let js_store = JsBTreeStore::new();

        let tx = begin_test_tx(&js_store).await;
        ok(tx.set_bytes("gone", b"x").await);
        ok(tx.rollback().await);

        assert_eq!(ok_bytes(js_store.get_bytes("gone").await), None);
    }

    #[wasm_bindgen_test]
    async fn transaction_read_your_own_writes() {
        let js_store = JsBTreeStore::new();

        let tx = begin_test_tx(&js_store).await;
        ok(tx
            .set(
                "doc",
                json_compatible(&serde_json::json!({ "ok": true })).expect("serialize to JsValue"),
            )
            .await);

        assert_eq!(
            ok_bytes(tx.get_bytes("doc").await),
            Some(br#"{"ok":true}"#.to_vec())
        );

        let staged = entries(tx.gets_bytes(None, Direction::Next, None, None).await);
        assert_eq!(staged.len(), 1);

        ok(tx.rollback().await);
    }

    #[wasm_bindgen_test]
    async fn exists_reports_key_presence() {
        let js_store = JsBTreeStore::new();

        let absent = ok(js_store.has("k").await)
            .as_bool()
            .expect("exists should return a boolean");
        assert!(!absent);

        ok(js_store.set_bytes("k", b"v").await);
        let present = ok(js_store.has("k").await)
            .as_bool()
            .expect("exists should return a boolean");
        assert!(present);

        ok(js_store.delete("k").await);
        let deleted = ok(js_store.has("k").await)
            .as_bool()
            .expect("exists should return a boolean");
        assert!(!deleted);
    }

    #[wasm_bindgen_test]
    async fn transaction_exists_sees_staged_writes() {
        let js_store = JsBTreeStore::new();

        let tx = begin_test_tx(&js_store).await;
        ok(tx.set_bytes("staged", b"v").await);

        let in_tx = ok(tx.has("staged").await)
            .as_bool()
            .expect("exists should return a boolean");
        assert!(in_tx);
        ok(tx.rollback().await);
    }

    #[wasm_bindgen_test]
    async fn committed_transaction_handle_rejects_further_ops() {
        let js_store = JsBTreeStore::new();

        let tx = begin_test_tx(&js_store).await;
        let reused = tx.clone();
        ok(tx.commit().await);

        assert!(reused.get_bytes("k").await.is_err());
        assert!(reused.has("k").await.is_err());
        assert!(begin_test_tx(&js_store).await.inner.lock().await.is_some());
    }

    #[wasm_bindgen_test]
    async fn rolled_back_transaction_handle_rejects_further_ops() {
        let js_store = JsBTreeStore::new();

        let tx = begin_test_tx(&js_store).await;
        let reused = tx.clone();
        ok(tx.rollback().await);

        assert!(reused.delete("k").await.is_err());
    }

    #[wasm_bindgen_test]
    async fn save_load_roundtrip_across_instances() {
        let js_store = JsBTreeStore::new();
        for n in 0..3u32 {
            ok(js_store
                .set_bytes(&format!("k{n}"), format!("v{n}").as_bytes())
                .await);
        }

        let snapshot = to_bytes(&ok(js_store.save().await));
        assert!(!snapshot.is_empty());

        let restored = JsBTreeStore::new();
        let count = ok(restored.load(&snapshot).await);
        assert_eq!(count.as_f64(), Some(f64::from(3)));

        for n in 0..3u32 {
            assert_eq!(
                ok_bytes(restored.get_bytes(&format!("k{n}")).await),
                Some(format!("v{n}").into_bytes())
            );
        }
    }

    #[wasm_bindgen_test]
    async fn load_rejects_corrupted_payload() {
        let js_store = JsBTreeStore::new();
        assert!(js_store.load(&[1, 2]).await.is_err());
    }

    #[wasm_bindgen_test]
    async fn save_empty_store_produces_empty_buffer() {
        let js_store = JsBTreeStore::new();
        assert!(to_bytes(&ok(js_store.save().await)).is_empty());
    }

    #[wasm_bindgen_test]
    async fn json_documents_persist_as_real_json_in_snapshots() {
        let js_store = JsBTreeStore::new();
        let doc = serde_json::json!({
            "name": "oxkv",
            "nested": { "ok": true, "n": 42 },
            "arr": [1, 2, 3]
        });
        let js_doc = json_compatible(&doc).expect("serialize to JsValue");
        ok(js_store.set("doc", js_doc).await);

        let snapshot = to_bytes(&ok(js_store.save().await));
        let entries = snapshot_entries(&snapshot);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "doc");

        // The bytes on disk must be the actual JSON document, not a
        // serialized-opaque-handle placeholder like {}.
        let stored: serde_json::Value =
            serde_json::from_slice(&entries[0].1).expect("stored bytes are valid JSON");
        assert_eq!(stored, doc);
        assert_ne!(stored, serde_json::json!({}));
    }

    #[wasm_bindgen_test]
    async fn committed_transaction_changes_reach_snapshots() {
        let js_store = JsBTreeStore::new();
        let tx = begin_test_tx(&js_store).await;
        ok(tx.set_bytes("k", b"v").await);
        ok(tx.commit().await);

        let snapshot = to_bytes(&ok(js_store.save().await));
        let entries = snapshot_entries(&snapshot);
        assert_eq!(entries, vec![("k".to_owned(), b"v".to_vec())]);
    }
}
