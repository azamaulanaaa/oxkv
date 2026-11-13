//! Tests for the legacy Lucene-style filter.
//!
//! The legacy filter supports regular expressions. The 1.0 search contract
//! does not. The `oxkv-index` and `oxkv-search` crates hold that contract.

use oxkv_query::{eval, parse};

#[test]
fn the_legacy_filter_matches_a_regex_term() {
    let document = serde_json::json!({ "lang": "rust", "email": "ada@gmail.com" });

    let query = parse("lang:/^ru.*$/").expect("a regex term parses");
    assert!(eval(&query, &document));

    let query = parse("email:/gmail/").expect("a regex term parses");
    assert!(eval(&query, &document));

    let query = parse("lang:/^go$/").expect("a regex term parses");
    assert!(!eval(&query, &document));
}

#[test]
fn the_legacy_filter_still_accepts_a_wildcard_term() {
    let document = serde_json::json!({ "lang": "rust" });
    let query = parse("lang:r*").expect("a wildcard term parses");
    assert!(eval(&query, &document));
}
