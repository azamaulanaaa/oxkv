#![allow(missing_docs)]

//! Shared support for the replication golden vectors.

use serde_json::Value;

/// One golden file.
pub(crate) struct GoldenFile {
    pub(crate) contract: String,
    pub(crate) cases: Vec<GoldenCase>,
}

/// One golden case.
pub(crate) struct GoldenCase {
    pub(crate) name: String,
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
            expected: case["expected"].clone(),
        })
        .collect();
    GoldenFile { contract, cases }
}

/// Returns one expected string field.
pub(crate) fn expected_str<'a>(case: &'a GoldenCase, field: &str) -> &'a str {
    case.expected[field]
        .as_str()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one expected string field that may be null.
pub(crate) fn expected_optional_str<'a>(case: &'a GoldenCase, field: &str) -> Option<&'a str> {
    case.expected[field].as_str()
}

/// Returns one expected boolean field.
pub(crate) fn expected_bool(case: &GoldenCase, field: &str) -> bool {
    case.expected[field]
        .as_bool()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one expected unsigned field.
pub(crate) fn expected_u64(case: &GoldenCase, field: &str) -> u64 {
    case.expected[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one expected unsigned array.
pub(crate) fn expected_u64_list(case: &GoldenCase, field: &str) -> Vec<u64> {
    case.expected[field]
        .as_array()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
        .iter()
        .map(|value| {
            value
                .as_u64()
                .unwrap_or_else(|| panic!("the case {} has a non-integer value", case.name))
        })
        .collect()
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
