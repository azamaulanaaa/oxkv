//! A transactional key-value store with WASM bindings (`BTreeStore` + `OxKvStore`)
//! and Lucene-style JSON queries.
//!
//! With the `otel` feature enabled, `OtelStore` wraps any backend with
//! OpenTelemetry traces and metrics; see its docs for wiring guidance.
#![allow(clippy::multiple_crate_versions)]
//! The [README](https://github.com/azamaulanaaa/oxkv/blob/main/README.md)
//! below is the user guide; its live Rust examples run as doctests under
//! `--all-features`, while `text` fences illustrate feature-gated wiring
//! whose tested versions live in the linked API docs.
#![doc = include_str!("../README.md")]

/// Wasm Bindings
#[cfg(target_arch = "wasm32")]
pub mod wasm;

mod store;
pub use store::*;
mod query;
pub use query::*;
