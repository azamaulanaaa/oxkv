#![allow(missing_docs)]

//! Shared helpers for the shard map tests.

use oxkv_contracts::TableName;
use serde_json::Value;

/// Loads one JSON document.
pub(crate) fn load(text: &str) -> Value {
    serde_json::from_str(text).expect("the vector file is valid JSON")
}

/// Returns the lowercase hexadecimal form of one byte slice.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// Parses one hexadecimal byte string.
pub(crate) fn bytes(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0, "the hex text has an even length");
    (0..text.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&text[index..index + 2], 16).expect("the hex text is valid")
        })
        .collect()
}

/// Parses one table name.
pub(crate) fn table(name: &str) -> TableName {
    TableName::parse(name).expect("the table name is valid")
}

/// Reads one string field.
pub(crate) fn field_str<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("the vector has no {field} text field"))
}

/// Reads one integer field.
pub(crate) fn field_u64(value: &Value, field: &str) -> u64 {
    value[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the vector has no {field} integer field"))
}
