//! A transactional key-value store. It provides WASM bindings (`BTreeStore` and
//! `OxKvStore`). It also provides Lucene-style JSON queries.
//!
//! With the `otel` feature enabled, `OtelStore` wraps any backend. It adds
//! OpenTelemetry traces and metrics. See the `OtelStore` docs for wiring
//! guidance.
#![allow(clippy::multiple_crate_versions)]
//! The [README](https://github.com/azamaulanaaa/oxkv/blob/main/README.md)
//! below is the user guide. Its live Rust examples run as doctests under
//! `--all-features`. The `text` fences illustrate feature-gated wiring. The
//! tested versions of that wiring live in the linked API docs.
#![doc = include_str!("../README.md")]

/// Wasm Bindings
#[cfg(target_arch = "wasm32")]
pub mod wasm;

mod store;
pub use store::*;
mod query;
pub use query::*;
