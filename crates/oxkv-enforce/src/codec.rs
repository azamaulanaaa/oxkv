//! Canonical byte encoding for hashes.
//!
//! Every value uses a length prefix or a fixed width. The order is fixed.
//! The encoding does not use a host type width.

/// Appends one unsigned 64-bit integer in little-endian order.
pub(crate) fn push_u64(buffer: &mut Vec<u8>, value: u64) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

/// Appends one unsigned byte.
pub(crate) fn push_u8(buffer: &mut Vec<u8>, value: u8) {
    buffer.push(value);
}

/// Appends one length-prefixed byte string.
pub(crate) fn push_bytes(buffer: &mut Vec<u8>, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    push_u64(buffer, length);
    buffer.extend_from_slice(bytes);
}

/// Appends one optional unsigned 64-bit integer.
pub(crate) fn push_option_u64(buffer: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            push_u8(buffer, 1);
            push_u64(buffer, value);
        }
        None => push_u8(buffer, 0),
    }
}

/// Appends one domain tag and a zero separator.
pub(crate) fn push_tag(buffer: &mut Vec<u8>, tag: &[u8]) {
    buffer.extend_from_slice(tag);
    push_u8(buffer, 0);
}
