//! Canonical binary encoding for projection artifacts.
//!
//! The encoder is deterministic. It uses fixed-width big-endian integers and
//! length-prefixed byte strings. Two writers of the same ordered input produce
//! identical bytes.
//!
//! The decoder is not part of this crate. A transport layer decodes a frame
//! and passes the typed value to [`crate::Projection`].

/// A canonical encoder.
#[derive(Debug, Default)]
pub(crate) struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    /// Creates an empty encoder.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Appends one byte.
    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    /// Appends one big-endian `u64`.
    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends one big-endian `i64`.
    pub(crate) fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends a length-prefixed byte string.
    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.u64(u64::try_from(value.len()).unwrap_or(u64::MAX));
        self.bytes.extend_from_slice(value);
    }

    /// Appends a length-prefixed text string.
    pub(crate) fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// Appends one boolean.
    pub(crate) fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    /// Returns the encoded bytes.
    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// A value that has one canonical byte encoding.
pub trait CanonicalEncode {
    /// Returns the canonical bytes.
    #[must_use]
    fn canonical_bytes(&self) -> Vec<u8>;
}

/// Returns the lowercase hexadecimal form of bytes.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}
