#![allow(clippy::multiple_crate_versions)]
// The lint fires in code that `wasm_bindgen` generates, not in this crate.
#![allow(clippy::float_cmp)]

//! WASM bindings for `BTreeStore` (light baseline), `OxKvStore` (LSM engine),
//! and `CachedOxKvStore` (write-through RAM mirror).
//!
//! The OXKV snapshot wire format is identical across `BTreeStore` and
//! `OxKvStore` on every target. The format comes from `snapshot.rs`. It uses
//! the magic `OXKV` and version 1. Bytes from `save` restore everywhere via
//! `load`. `OxKvStore` binds the LSM engine to an in-memory
//! [`oxkv::MemStorage`]. The `oxkv-opfs` crate provides durable browser
//! storage through [`oxkv::Storage`].
//!
//! The crate is empty on native targets. The bindings compile only for
//! `wasm32`.

#[cfg(target_arch = "wasm32")]
include!("impl.rs");
