//! Runs the WASM binding suite in a real browser.
//!
//! `wasm-bindgen-test` selects Node.js for the crate's unit tests. Node has no
//! OPFS and no `window`, so the persistent-store cases announce a skip there.
//! This target repeats the same test module in a suite configured for the
//! browser. The Chrome CI job (`wasm-pack test --headless --chrome`) then
//! executes those cases instead of skipping the whole suite.

#![cfg(target_arch = "wasm32")]

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

/// The binding crate source, compiled again as a browser test suite.
#[path = "../src/impl.rs"]
pub mod imp;

pub(crate) use imp::announce_skip;
