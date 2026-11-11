#![allow(clippy::multiple_crate_versions)]

//! Browser storage for `oxkv` on the origin private file system (OPFS).
//!
//! `OpfsStorage` implements [`oxkv::Storage`] over real OPFS files. The
//! contents survive page reloads. This backend supports the main thread only.
//! Async file handles work on the main thread. Sync access handles are
//! worker-only.
//!
//! The crate is empty on native targets. The backend compiles only for
//! `wasm32`.

#[cfg(target_arch = "wasm32")]
include!("impl.rs");
