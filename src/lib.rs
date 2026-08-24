//! A transactional key-value store with WASM bindings and Lucene-style JSON
//! queries.
#![allow(clippy::multiple_crate_versions)]

/// Wasm Bindings
#[cfg(target_arch = "wasm32")]
pub mod wasm;

mod store;
pub use store::*;
mod query;
pub use query::*;
