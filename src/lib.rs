//! A transactional key-value store. It provides WASM bindings (`BTreeStore` and
//! `OxKvStore`). It also provides Lucene-style JSON queries.
//!
//! The `oxkv-otel` crate provides `OtelStore`. It wraps any backend and adds
//! OpenTelemetry traces and metrics. See the `oxkv-otel` docs for wiring
//! guidance.
#![allow(clippy::multiple_crate_versions)]
//! The [README](https://github.com/azamaulanaaa/oxkv/blob/main/README.md)
//! below is the user guide. Its live Rust examples run as doctests under
//! `--all-features`. The `text` fences illustrate feature-gated wiring. The
//! tested versions of that wiring live in the linked API docs.
#![doc = include_str!("../README.md")]

mod store;
pub use store::*;
mod query;
pub use query::*;
