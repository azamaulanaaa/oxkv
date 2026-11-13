//! Deterministic byte encoding for policy and manifest records.
//!
//! Every integer uses little-endian order. Every variable-length value carries
//! a length prefix. A map uses its key order. The encoding does not depend on
//! the host, the target, or the hash map seed.

/// Appends one unsigned 64-bit integer.
pub(crate) fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

/// Appends one unsigned 32-bit integer.
pub(crate) fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

/// Appends one byte string with a length prefix.
pub(crate) fn push_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    push_u64(output, bytes.len() as u64);
    output.extend_from_slice(bytes);
}

/// Appends one text value with a length prefix.
pub(crate) fn push_str(output: &mut Vec<u8>, text: &str) {
    push_bytes(output, text.as_bytes());
}

/// Appends one boolean value.
pub(crate) fn push_bool(output: &mut Vec<u8>, value: bool) {
    output.push(u8::from(value));
}

/// Appends one enum tag.
pub(crate) fn push_tag(output: &mut Vec<u8>, tag: u8) {
    output.push(tag);
}
