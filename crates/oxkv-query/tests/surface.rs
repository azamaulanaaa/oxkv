//! The 1.0 search surface gate.
//!
//! The 1.0 search language has no regular expressions. A `/regex/` term fails
//! with the named policy error. A wildcard term still works.

use oxkv_query::{REGEX_NOT_SUPPORTED_CODE, eval, parse};

#[test]
fn the_one_zero_search_surface_rejects_a_regex_term() {
    for query in [
        "lang:/^rust$/",
        "title:/rus.*/",
        "tags:(+kv /^sys.*/)",
        "name:/a\\/b/",
    ] {
        let error = parse(query).expect_err("a regex term must fail");
        assert!(
            error.contains(REGEX_NOT_SUPPORTED_CODE),
            "the query {query} returned {error}"
        );
    }
}

#[test]
fn the_one_zero_search_surface_still_accepts_a_wildcard_term() {
    let document = serde_json::json!({ "lang": "rust" });
    let query = parse("lang:r*").expect("a wildcard term parses");
    assert!(eval(&query, &document));
}
