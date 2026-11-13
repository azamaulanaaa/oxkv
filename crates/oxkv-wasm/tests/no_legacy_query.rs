//! Static checks for the 1.0 WASM surface.
//!
//! These checks fail when the crate adds the `oxkv-query` dependency or a
//! query filter. The checks read the manifest and the source files. They run
//! on the native target and need no browser.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn manifest_has_no_query_dependency() {
    let manifest = read(&manifest_dir().join("Cargo.toml"));
    assert!(
        !manifest.contains("oxkv-query"),
        "crates/oxkv-wasm/Cargo.toml must not depend on oxkv-query"
    );
}

#[test]
fn sources_have_no_query_filter() {
    let source_dir = manifest_dir().join("src");
    let mut checked = 0;

    for entry in fs::read_dir(&source_dir).expect("read the src directory") {
        let path = entry.expect("read one src entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }

        let source = read(&path).to_lowercase();
        assert!(
            !source.contains("oxkv_query") && !source.contains("oxkv-query"),
            "{} must not use the oxkv-query crate",
            path.display()
        );
        assert!(
            !source.contains("query"),
            "{} must not hold a query filter",
            path.display()
        );
        checked += 1;
    }

    assert!(checked >= 4, "expected at least four source files");
}
