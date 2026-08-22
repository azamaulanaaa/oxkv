//! A minimal WASM library template with strict linting.
#![allow(clippy::multiple_crate_versions)]

/// Wasm Bindings
#[cfg(target_arch = "wasm32")]
pub mod wasm;

mod store;
pub use store::*;
mod query;
pub use query::*;
