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

#[cfg(feature = "btree")]
mod btree;
#[cfg(feature = "oxkv")]
mod cached;
#[cfg(feature = "oxkv")]
mod oxkv;

#[cfg(feature = "btree")]
#[cfg(feature = "oxkv")]
pub use cached::{JsCachedOxKvStore, JsCachedOxKvTx};

#[cfg(feature = "btree")]
pub use btree::{JsBTreeStore, JsBTreeTx};
#[cfg(feature = "oxkv")]
#[cfg(feature = "oxkv")]
pub use oxkv::{JsOxKvStore, JsOxKvTx};
