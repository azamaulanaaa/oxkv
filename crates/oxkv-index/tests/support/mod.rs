#![allow(missing_docs)]
#![allow(dead_code)] // One test binary uses the golden loader and another uses the fixtures.

//! Shared support for the index golden vectors and integration tests.

use oxkv_contracts::version::{CommitVersion, PrefixBounds};
use oxkv_index::{
    DocumentField, FieldSpec, FlagDecl, IndexDocument, IndexManifest, ScorerSpec, SearchRequest,
    ShardIndex, TokenBudget, TokenizerIdentity, build_scope_index,
};
use serde_json::Value;

/// One golden file.
pub(crate) struct GoldenFile {
    pub(crate) contract: String,
    pub(crate) value: Value,
}

/// Loads one golden file.
pub(crate) fn load(text: &str) -> GoldenFile {
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    let contract = value["contract"]
        .as_str()
        .expect("the golden file names a contract")
        .to_owned();
    GoldenFile { contract, value }
}

/// Returns the lowercase hexadecimal form of a byte slice.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// Returns the text array of a case field.
pub(crate) fn text_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("the field is an array")
        .iter()
        .map(|item| item.as_str().expect("the array item is text").to_owned())
        .collect()
}

/// Returns the integer array of a case field.
pub(crate) fn integer_array(value: &Value) -> Vec<u64> {
    value
        .as_array()
        .expect("the field is an array")
        .iter()
        .map(|item| item.as_u64().expect("the array item is an integer"))
        .collect()
}

/// Returns the sample manifest for the index vectors.
pub(crate) fn sample_manifest() -> IndexManifest {
    let title = FieldSpec::new("title", TokenizerIdentity::unicode_word())
        .with_positions(true)
        .with_filterable(true)
        .with_filter_flags(vec![
            FlagDecl::new("published", 0).expect("the flag is valid"),
        ])
        .expect("the filter flags are valid")
        .with_search_flags(vec![
            FlagDecl::new("promoted", 0).expect("the flag is valid"),
        ])
        .expect("the search flags are valid");
    let body = FieldSpec::new("body", TokenizerIdentity::unicode_word());
    IndexManifest::new(
        vec![title, body],
        TokenizerIdentity::unicode_word(),
        ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
        100_000,
        TokenBudget::new(64, 32),
    )
    .expect("the manifest is valid")
}

/// Returns the sample shard bounds.
pub(crate) fn sample_bounds() -> PrefixBounds {
    PrefixBounds::new(vec![b'a'], vec![b'd'])
}

/// Returns the sample documents for the index vectors.
pub(crate) fn sample_documents() -> Vec<IndexDocument> {
    vec![
        IndexDocument::new(
            b"a".to_vec(),
            vec![
                DocumentField::new("title", "Rust rust search", 1, 0),
                DocumentField::new("body", "Fast store", 0, 0),
            ],
        ),
        IndexDocument::new(
            b"b".to_vec(),
            vec![
                DocumentField::new("title", "Rust database", 1, 1),
                DocumentField::new("body", "Search index", 0, 0),
            ],
        ),
        IndexDocument::new(
            b"c".to_vec(),
            vec![
                DocumentField::new("title", "Café search", 1, 0),
                DocumentField::new("body", "Crustacean", 0, 0),
            ],
        ),
    ]
}

/// Builds the sample replica index at version 1.
pub(crate) fn sample_index() -> ShardIndex {
    build_scope_index(
        &sample_manifest(),
        &sample_bounds(),
        &oxkv_index::AllScope,
        sample_documents(),
        CommitVersion::new(1),
    )
    .expect("the sample index builds")
}

/// Returns the sample search request.
pub(crate) fn sample_search() -> SearchRequest {
    SearchRequest::new("rust search", CommitVersion::new(1))
        .with_fields(vec!["title".to_owned()])
        .with_positions(true)
        .with_limit(10)
}
