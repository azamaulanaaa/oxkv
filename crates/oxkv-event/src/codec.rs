//! Canonical byte encoding for the event contracts.
//!
//! Every integer uses big-endian byte order. Every variable-length value uses
//! a `u64` length prefix. The encoding does not depend on the platform, the
//! pointer width, or the Rust enum layout.

/// An error from a canonical decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodeError;

/// A canonical big-endian encoder.
pub(crate) struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    /// Creates an empty encoder.
    pub(crate) fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Returns the encoded bytes.
    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }

    /// Writes one byte.
    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    /// Writes one unsigned 64-bit integer.
    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes one signed 64-bit integer.
    pub(crate) fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes one boolean value.
    pub(crate) fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    /// Writes one raw 32-byte hash.
    pub(crate) fn raw32(&mut self, value: &[u8; 32]) {
        self.bytes.extend_from_slice(value);
    }

    /// Writes one length-prefixed byte string.
    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.u64(u64::try_from(value.len()).unwrap_or(u64::MAX));
        self.bytes.extend_from_slice(value);
    }

    /// Writes one length-prefixed UTF-8 string.
    pub(crate) fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }
}

/// A canonical big-endian decoder.
pub(crate) struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    /// Creates a decoder over one byte slice.
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// Requires that the decoder consumed every byte.
    pub(crate) fn finish(&self) -> Result<(), DecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(DecodeError)
        }
    }

    /// Returns the bytes that the decoder did not consume.
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.position.checked_add(length).ok_or(DecodeError)?;
        let slice = self.bytes.get(self.position..end).ok_or(DecodeError)?;
        self.position = end;
        Ok(slice)
    }

    /// Reads one byte.
    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        let slice = self.take(1)?;
        Ok(slice[0])
    }

    /// Reads one unsigned 64-bit integer.
    pub(crate) fn u64(&mut self) -> Result<u64, DecodeError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(u64::from_be_bytes(bytes))
    }

    /// Reads one signed 64-bit integer.
    pub(crate) fn i64(&mut self) -> Result<i64, DecodeError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(i64::from_be_bytes(bytes))
    }

    /// Reads one boolean value.
    pub(crate) fn bool(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError),
        }
    }

    /// Reads one raw 32-byte hash.
    pub(crate) fn raw32(&mut self) -> Result<[u8; 32], DecodeError> {
        let slice = self.take(32)?;
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(slice);
        Ok(bytes)
    }

    /// Reads one length-prefixed byte string.
    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let length = self.u64()?;
        let length = usize::try_from(length).map_err(|_| DecodeError)?;
        self.take(length)
    }

    /// Reads one length-prefixed UTF-8 string.
    pub(crate) fn text(&mut self) -> Result<String, DecodeError> {
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError)
    }

    /// Reads one element count. The count cannot exceed the remaining bytes.
    pub(crate) fn count(&mut self) -> Result<usize, DecodeError> {
        let count = self.u64()?;
        let count = usize::try_from(count).map_err(|_| DecodeError)?;
        if count > self.remaining() {
            return Err(DecodeError);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_use_big_endian_order() {
        let mut encoder = Encoder::new();
        encoder.u64(0x0102_0304_0506_0708);
        assert_eq!(encoder.finish(), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_decoder_rejects_trailing_bytes() {
        let mut decoder = Decoder::new(&[1, 2]);
        assert_eq!(decoder.u8(), Ok(1));
        assert_eq!(decoder.finish(), Err(DecodeError));
    }
}
