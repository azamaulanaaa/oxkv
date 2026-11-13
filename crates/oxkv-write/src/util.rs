//! Canonical encoding helpers.

/// Appends a length-prefixed byte string.
pub(crate) fn push_bytes(buffer: &mut Vec<u8>, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).expect("the byte length fits in u64");
    push_u64(buffer, length);
    buffer.extend_from_slice(bytes);
}

/// Appends an unsigned 64-bit integer in little-endian form.
pub(crate) fn push_u64(buffer: &mut Vec<u8>, value: u64) {
    buffer.extend_from_slice(&value.to_le_bytes());
}
