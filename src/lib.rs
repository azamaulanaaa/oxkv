//! A transactional key-value store. It provides the `BTreeStore` and
//! `OxKvStore` backends.
//!
//! The `oxkv-query` crate provides the Lucene-style JSON query engine. The
//! `oxkv-wasm` crate provides the WASM bindings. The `oxkv-otel` crate
//! provides `OtelStore`. It wraps any backend and adds OpenTelemetry traces and
//! metrics. The `oxkv-s3` crate provides the `object_store` adapter.
#![allow(clippy::multiple_crate_versions)]
//! The [README](https://github.com/azamaulanaaa/oxkv/blob/main/README.md)
//! below is the user guide. Its live Rust examples run as doctests under
//! `--all-features`. The `text` fences illustrate feature-gated wiring. The
//! tested versions of that wiring live in the linked API docs.
#![doc = include_str!("../README.md")]

mod store;
pub use store::*;
