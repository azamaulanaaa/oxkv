//! Canonical byte helpers for scope pages and replica index artifacts.
//!
//! The encoding is big endian and length prefixed. It uses no floats and no
//! platform-dependent layout. The same input produces the same bytes on every
//! target.

/// A canonical byte encoder.
pub(crate) struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    /// Creates an empty encoder.
    pub(crate) fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Creates an encoder with reserved capacity.
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    /// Appends one byte.
    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    /// Appends one big endian `u16`.
    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends one big endian `u64`.
    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends a length-prefixed byte string.
    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.u64(as_u64(value.len()));
        self.bytes.extend_from_slice(value);
    }

    /// Appends a length-prefixed text string.
    pub(crate) fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// Returns the encoded bytes.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the encoded length.
    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }
}

/// A canonical byte decoder.
pub(crate) struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    /// Creates a decoder over one byte slice.
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// Returns true when every byte was read.
    pub(crate) fn is_finished(&self) -> bool {
        self.position == self.bytes.len()
    }

    /// Reads one byte.
    #[cfg(test)]
    pub(crate) fn u8(&mut self) -> Option<u8> {
        let slice = self.take(1)?;
        Some(slice[0])
    }

    /// Reads one big endian `u16`.
    pub(crate) fn u16(&mut self) -> Option<u16> {
        let slice = self.take(2)?;
        let mut bytes = [0_u8; 2];
        bytes.copy_from_slice(slice);
        Some(u16::from_be_bytes(bytes))
    }

    /// Reads one big endian `u64`.
    pub(crate) fn u64(&mut self) -> Option<u64> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Some(u64::from_be_bytes(bytes))
    }

    /// Reads a length-prefixed byte string.
    pub(crate) fn bytes(&mut self) -> Option<&'a [u8]> {
        let length = usize::try_from(self.u64()?).ok()?;
        self.take(length)
    }

    /// Reads a length-prefixed UTF-8 string.
    pub(crate) fn text(&mut self) -> Option<&'a str> {
        core::str::from_utf8(self.bytes()?).ok()
    }

    /// Reads a collection length. Every entry needs at least one byte.
    pub(crate) fn count(&mut self, entry_minimum: usize) -> Option<usize> {
        let count = usize::try_from(self.u64()?).ok()?;
        let remaining = self.bytes.len().saturating_sub(self.position);
        if count > remaining / entry_minimum.max(1) {
            return None;
        }
        Some(count)
    }

    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(length)?;
        let slice = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(slice)
    }
}

/// Returns one `usize` as `u64`.
pub(crate) fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Decoder, Encoder, as_u64};

    #[test]
    fn a_round_trip_reads_every_field() {
        let mut encoder = Encoder::new();
        encoder.u8(7);
        encoder.u16(9);
        encoder.u64(11);
        encoder.bytes(b"abc");
        encoder.text("hello");
        let bytes = encoder.into_bytes();
        let mut decoder = Decoder::new(&bytes);
        assert_eq!(decoder.u8(), Some(7));
        assert_eq!(decoder.u16(), Some(9));
        assert_eq!(decoder.u64(), Some(11));
        assert_eq!(decoder.bytes(), Some(b"abc".as_slice()));
        assert_eq!(decoder.text(), Some("hello"));
        assert!(decoder.is_finished());
    }

    #[test]
    fn a_truncated_input_fails() {
        let mut decoder = Decoder::new(&[0, 0, 0]);
        assert_eq!(decoder.u64(), None);
    }

    #[test]
    fn a_count_larger_than_the_input_fails() {
        let mut encoder = Encoder::new();
        encoder.u64(1_000);
        let bytes = encoder.into_bytes();
        let mut decoder = Decoder::new(&bytes);
        assert_eq!(decoder.count(1), None);
    }

    #[test]
    fn a_usize_converts_to_u64() {
        assert_eq!(as_u64(3), 3);
    }
}
