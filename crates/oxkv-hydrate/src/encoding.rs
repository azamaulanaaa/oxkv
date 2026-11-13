//! Canonical binary encoding helpers.
//!
//! Every multi-byte integer uses little-endian order. A byte string is a
//! `u32` length followed by the bytes. The encoding is deterministic.

/// A canonical binary writer.
#[derive(Debug, Default)]
pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    /// Creates an empty writer.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Writes one byte.
    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    /// Writes one `u16` value.
    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Writes one `u32` value.
    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Writes one `u64` value.
    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Writes raw bytes without a length prefix.
    pub(crate) fn raw(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    /// Writes a length-prefixed byte string.
    pub(crate) fn bytes(&mut self, value: &[u8]) {
        let length = u32::try_from(value.len()).unwrap_or(u32::MAX);
        self.u32(length);
        self.bytes.extend_from_slice(value);
    }

    /// Writes a 32-byte hash value.
    pub(crate) fn hash32(&mut self, value: &[u8; 32]) {
        self.bytes.extend_from_slice(value);
    }

    /// Returns the written bytes.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// An error from canonical decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeError {
    /// The input ends before the required bytes.
    Truncated {
        /// The expected byte count.
        expected: usize,
        /// The available byte count.
        actual: usize,
    },
    /// The input has bytes that the format does not allow.
    Invalid,
}

/// A canonical binary reader.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    /// Creates a reader over the given bytes.
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Returns the current byte offset.
    pub(crate) const fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the number of unread bytes.
    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.offset.checked_add(count).ok_or(DecodeError::Invalid)?;
        if end > self.bytes.len() {
            return Err(DecodeError::Truncated {
                expected: end,
                actual: self.bytes.len(),
            });
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    /// Reads one byte.
    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    /// Reads one `u16` value.
    pub(crate) fn u16(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Reads one `u32` value.
    pub(crate) fn u32(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Reads one `u64` value.
    pub(crate) fn u64(&mut self) -> Result<u64, DecodeError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Reads a length-prefixed byte string.
    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let length = self.u32()?;
        let length = usize::try_from(length).map_err(|_| DecodeError::Invalid)?;
        self.take(length)
    }

    /// Reads a 32-byte hash value.
    pub(crate) fn hash32(&mut self) -> Result<[u8; 32], DecodeError> {
        let bytes = self.take(32)?;
        let mut hash = [0_u8; 32];
        hash.copy_from_slice(bytes);
        Ok(hash)
    }

    /// Reads exactly `count` bytes.
    pub(crate) fn exact(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        self.take(count)
    }

    /// Checks that the input has no unread bytes.
    pub(crate) fn finish(&self) -> Result<(), DecodeError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(DecodeError::Invalid)
        }
    }
}
