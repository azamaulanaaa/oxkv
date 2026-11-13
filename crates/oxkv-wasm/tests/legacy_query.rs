//! Tests for the legacy Lucene-style query filter.
//!
//! The `oxkv-wasm` bindings expose this filter through `gets`. The filter is
//! not the 1.0 search contract. The 1.0 search contract is regex-free. The
//! legacy filter supports regular expressions.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxkv_query::{eval, parse};

fn matches(document: &serde_json::Value, query: &str) -> bool {
    let parsed = parse(query).expect("the query parses");
    eval(&parsed, document)
}

#[test]
fn the_legacy_filter_matches_a_field_query() {
    let document = serde_json::json!({ "name": "Ada", "age": 36, "lang": "rust" });
    assert!(matches(&document, "lang:rust"));
    assert!(!matches(&document, "lang:go"));
}

#[test]
fn the_legacy_filter_matches_a_range_query() {
    let document = serde_json::json!({ "age": 36 });
    assert!(matches(&document, "age:[30 TO 45]"));
    assert!(!matches(&document, "age:[50 TO 60]"));
}

#[test]
fn the_legacy_filter_matches_a_regex_query() {
    let document = serde_json::json!({ "lang": "rust" });
    assert!(matches(&document, "lang:/^ru.*/"));
    assert!(!matches(&document, "lang:/^go.*/"));
}
