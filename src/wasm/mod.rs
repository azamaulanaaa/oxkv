//! WASM bindings — `BTreeStore` (light baseline), `OxKvStore` (LSM engine),
//! and `CachedOxKvStore` (write-through RAM mirror).
//!
//! The OXKV snapshot wire format (`snapshot.rs` magic `OXKV` + version 1) is
//! identical across `BTreeStore` and `OxKvStore` on every target, so
//! `save` bytes restore everywhere via `load`. `OxKvStore` binds the LSM
//! engine to an in-memory [`store::MemStorage`]; durable browser storage
//! (OPFS) arrives later as another [`store::Storage`] backend.

use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::store::{self};

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

/// Announces that a browser-only test is about to assert nothing here.
///
/// `wasm-pack test --node` — the job most contributors run locally, and the
/// one CI's `wasm` job runs — has no `window`, so OPFS tests there would pass
/// without checking anything. `eprintln!` is a no-op on
/// `wasm32-unknown-unknown` and the Node harness swallows `console.*`, so we
/// write straight to the host process's stderr and fall back to
/// `console.error` in a browser. The Chrome CI job
/// (`wasm-pack test --headless --chrome`) is what actually runs these tests.
#[cfg(test)]
pub(crate) fn announce_skip(test: &str, reason: &str) {
    let message = format!("SKIPPED {test}: {reason}\n");
    if write_host_stderr(&message) {
        return;
    }
    web_sys::console::error_1(&JsValue::from_str(&message));
}

/// Writes `message` to the host process's stderr; `false` when there is none.
#[cfg(test)]
fn write_host_stderr(message: &str) -> bool {
    fn property(object: &JsValue, key: &str) -> Result<JsValue, JsValue> {
        js_sys::Reflect::get(object, &JsValue::from_str(key))
    }
    let Ok(process) = property(&js_sys::global(), "process") else {
        return false;
    };
    let Ok(stderr) = property(&process, "stderr") else {
        return false;
    };
    let Ok(write) = property(&stderr, "write").and_then(JsCast::dyn_into::<js_sys::Function>)
    else {
        return false;
    };
    write.call1(&stderr, &JsValue::from_str(message)).is_ok()
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

/// Generates the `wasm_bindgen` transaction wrapper shared by every backend.
///
/// `BTreeStore`, `OxKvStore` and `CachedOxKvStore` each carried a ~220-line
/// copy of this wrapper, differing only in the store type and the JS class
/// name — eight copies of the same "transaction already committed or rolled
/// back" guard, free to drift apart. Only the store type varies, so it is a
/// macro parameter and the wrapper is written once. The JS surface is the
/// same for all three; only the store behind it differs.
macro_rules! js_tx {
    ($name:ident, $js_name:literal, $store:ty, $doc:literal) => {
        #[doc = $doc]
        #[wasm_bindgen(js_name = $js_name)]
        #[derive(Clone)]
        pub struct $name {
            inner: std::sync::Arc<
                futures::lock::Mutex<Option<<$store as store::Store>::Transaction>>,
            >,
        }

        #[wasm_bindgen(js_class = $js_name)]
        impl $name {
            async fn take_tx(
                self,
            ) -> Result<<$store as store::Store>::Transaction, store::StoreError> {
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
    };
}

#[cfg(feature = "btree")]
mod btree;
#[cfg(all(feature = "btree", feature = "oxkv"))]
mod cached;
#[cfg(feature = "oxkv")]
mod oxkv;

#[cfg(all(feature = "btree", feature = "oxkv"))]
pub use cached::{JsCachedOxKvStore, JsCachedOxKvTx};

#[cfg(feature = "btree")]
pub use btree::{JsBTreeStore, JsBTreeTx};
#[cfg(feature = "oxkv")]
#[cfg(feature = "oxkv")]
pub use oxkv::{JsOxKvStore, JsOxKvTx};
