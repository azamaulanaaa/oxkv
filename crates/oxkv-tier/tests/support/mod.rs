#![allow(missing_docs)]

//! Shared support for the tier golden vectors.

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

/// Returns one expected text field.
pub(crate) fn expected_str<'a>(case: &'a GoldenCase, field: &str) -> &'a str {
    case.expected[field]
        .as_str()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one expected integer field.
pub(crate) fn expected_u64(case: &GoldenCase, field: &str) -> u64 {
    case.expected[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns one expected boolean field.
pub(crate) fn expected_bool(case: &GoldenCase, field: &str) -> bool {
    case.expected[field]
        .as_bool()
        .unwrap_or_else(|| panic!("the case {} has no {field} field", case.name))
}

/// Returns an optional expected code field.
pub(crate) fn expected_code<'a>(case: &'a GoldenCase, field: &str) -> Option<&'a str> {
    case.expected[field].as_str()
}

/// Returns true when the file has a case with the given name.
pub(crate) fn has_case(file: &GoldenFile, name: &str) -> bool {
    file.cases.iter().any(|case| case.name == name)
}
