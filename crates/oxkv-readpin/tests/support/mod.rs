#![allow(missing_docs)]

//! Shared support for the read-pin golden vectors.

use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId, VersionVector};
use serde_json::Value;

/// One golden file.
pub(crate) struct GoldenFile {
    pub(crate) contract: String,
    pub(crate) cases: Vec<GoldenCase>,
}

/// One golden case.
pub(crate) struct GoldenCase {
    pub(crate) name: String,
    pub(crate) input: Value,
    pub(crate) expected: Value,
}

/// Loads one golden file.
pub(crate) fn load(text: &str) -> GoldenFile {
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    let contract = value["contract"]
        .as_str()
        .expect("the golden file names a contract")
        .to_owned();
    let cases = value["cases"]
        .as_array()
        .expect("the golden file has a case list")
        .iter()
        .map(|case| GoldenCase {
            name: case["name"]
                .as_str()
                .expect("the case names a vector")
                .to_owned(),
            input: case["input"].clone(),
            expected: case["expected"].clone(),
        })
        .collect();
    GoldenFile { contract, cases }
}

/// Returns one case by name.
pub(crate) fn case<'a>(file: &'a GoldenFile, name: &str) -> &'a GoldenCase {
    file.cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("the golden case {name} is missing"))
}

/// Returns one expected string.
pub(crate) fn expected_str<'a>(case: &'a GoldenCase, field: &str) -> &'a str {
    case.expected[field]
        .as_str()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one expected optional string.
pub(crate) fn expected_opt_str<'a>(case: &'a GoldenCase, field: &str) -> Option<&'a str> {
    case.expected[field].as_str()
}

/// Returns one expected boolean.
pub(crate) fn expected_bool(case: &GoldenCase, field: &str) -> bool {
    case.expected[field]
        .as_bool()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one expected unsigned integer.
pub(crate) fn expected_u64(case: &GoldenCase, field: &str) -> u64 {
    case.expected[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one expected version vector.
pub(crate) fn expected_vector(case: &GoldenCase, field: &str) -> VersionVector {
    vector(&case.expected[field])
}

/// Returns one input version vector.
pub(crate) fn input_vector(case: &GoldenCase, field: &str) -> VersionVector {
    vector(&case.input[field])
}

/// Decodes a version vector from a JSON object.
pub(crate) fn vector(value: &Value) -> VersionVector {
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("the value is not a version vector object"));
    let mut vector = VersionVector::new();
    for (shard, commit_version) in object {
        vector.set(
            shard_id(shard),
            CommitVersion::new(
                commit_version
                    .as_u64()
                    .unwrap_or_else(|| panic!("the shard {shard} has no commit version")),
            ),
        );
    }
    vector
}

/// Decodes a scope version vector from a JSON object.
pub(crate) fn scope_vector(value: &Value) -> ScopeVersionVector {
    ScopeVersionVector::from_vector(vector(value))
}

/// Decodes a shard identifier from hexadecimal text.
pub(crate) fn shard_id(text: &str) -> ShardId {
    assert!(
        text.len().is_multiple_of(2),
        "the shard id has an even length"
    );
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().as_chunks::<2>().0 {
        let pair = core::str::from_utf8(pair).expect("the shard id is ASCII");
        bytes.push(u8::from_str_radix(pair, 16).expect("the shard id is hexadecimal"));
    }
    ShardId::new(bytes)
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
