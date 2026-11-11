#![allow(clippy::multiple_crate_versions)]

//! WASM bindings for `BTreeStore` (light baseline), `OxKvStore` (LSM engine),
//! and `CachedOxKvStore` (write-through RAM mirror).
//!
//! The OXKV snapshot wire format is identical across `BTreeStore` and
//! `OxKvStore` on every target. The format comes from `snapshot.rs`. It uses
//! the magic `OXKV` and version 1. Bytes from `save` restore everywhere via
//! `load`. `OxKvStore` binds the LSM engine to an in-memory
//! [`store::MemStorage`]. Durable browser storage (OPFS) arrives later as
//! another [`store::Storage`] backend.
//!
//! The crate is empty on native targets. The bindings compile only for
//! `wasm32`.

#[cfg(target_arch = "wasm32")]
include!("impl.rs");
