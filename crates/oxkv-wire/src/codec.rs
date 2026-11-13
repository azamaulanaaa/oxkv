//! Canonical byte encoding for wire payloads.

use core::fmt;

use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId, VersionVector};

/// The largest collection that one payload can declare.
pub(crate) const MAX_COLLECTION_ENTRIES: usize = 1_000_000;

/// An error from canonical payload encoding or decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodecError {
    /// The input ended before a field was complete.
    Truncated,
    /// The input has bytes after the last field.
    TrailingBytes,
    /// A length or count does not fit in memory.
    LengthOverflow,
    /// A collection declares too many entries.
    TooManyEntries,
    /// A UTF-8 field is not valid UTF-8.
    InvalidUtf8,
    /// A boolean field is not zero or one.
    InvalidBoolean,
    /// A tag does not name a known value.
    InvalidTag,
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("The payload is truncated."),
            Self::TrailingBytes => formatter.write_str("The payload has trailing bytes."),
            Self::LengthOverflow => formatter.write_str("A payload length is too large."),
            Self::TooManyEntries => formatter.write_str("A payload collection is too large."),
            Self::InvalidUtf8 => formatter.write_str("A text field is not valid UTF-8."),
            Self::InvalidBoolean => formatter.write_str("A boolean field is not zero or one."),
            Self::InvalidTag => formatter.write_str("A payload tag is unknown."),
        }
    }
}

impl std::error::Error for CodecError {}

/// A canonical payload writer.
pub(crate) struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    /// Creates an empty encoder.
    pub(crate) const fn new() -> Self {
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

    /// Writes one big-endian `u16`.
    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes one big-endian `u32`.
    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes one big-endian `u64`.
    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes one canonical boolean.
    pub(crate) fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    /// Writes a length-prefixed byte string.
    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.u32(u32::try_from(value.len()).unwrap_or(u32::MAX));
        self.bytes.extend_from_slice(value);
    }

    /// Writes a length-prefixed UTF-8 string.
    pub(crate) fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// Writes one 32-byte digest.
    pub(crate) fn digest(&mut self, value: &[u8; 32]) {
        self.bytes.extend_from_slice(value);
    }

    /// Writes an optional byte string.
    pub(crate) fn optional_bytes(&mut self, value: Option<&[u8]>) {
        match value {
            Some(bytes) => {
                self.u8(1);
                self.bytes(bytes);
            }
            None => self.u8(0),
        }
    }

    /// Writes a scope version vector in canonical shard order.
    pub(crate) fn version_vector(&mut self, vector: &ScopeVersionVector) {
        self.raw_version_vector(vector.as_vector());
    }

    /// Writes a version vector in canonical shard order.
    pub(crate) fn raw_version_vector(&mut self, vector: &VersionVector) {
        self.u32(u32::try_from(vector.len()).unwrap_or(u32::MAX));
        for (shard_id, commit_version) in vector.iter() {
            self.bytes(shard_id.as_bytes());
            self.u64(commit_version.get());
        }
    }
}

/// A canonical payload reader.
pub(crate) struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    /// Creates a decoder for one payload.
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// Returns the number of unread bytes.
    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    /// Fails when the decoder did not read every byte.
    pub(crate) fn finish(&self) -> Result<(), CodecError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(CodecError::TrailingBytes)
        }
    }

    /// Reads one byte.
    pub(crate) fn u8(&mut self) -> Result<u8, CodecError> {
        let slice = self.take(1)?;
        Ok(slice[0])
    }

    /// Reads one big-endian `u16`.
    pub(crate) fn u16(&mut self) -> Result<u16, CodecError> {
        let slice = self.take(2)?;
        let mut bytes = [0_u8; 2];
        bytes.copy_from_slice(slice);
        Ok(u16::from_be_bytes(bytes))
    }

    /// Reads one big-endian `u32`.
    pub(crate) fn u32(&mut self) -> Result<u32, CodecError> {
        let slice = self.take(4)?;
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(slice);
        Ok(u32::from_be_bytes(bytes))
    }

    /// Reads one big-endian `u64`.
    pub(crate) fn u64(&mut self) -> Result<u64, CodecError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(u64::from_be_bytes(bytes))
    }

    /// Reads one canonical boolean.
    pub(crate) fn bool(&mut self) -> Result<bool, CodecError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CodecError::InvalidBoolean),
        }
    }

    /// Reads a length-prefixed byte string.
    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], CodecError> {
        let length = self.length()?;
        self.take(length)
    }

    /// Reads a length-prefixed UTF-8 string.
    pub(crate) fn text(&mut self) -> Result<String, CodecError> {
        let bytes = self.bytes()?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| CodecError::InvalidUtf8)
    }

    /// Reads one 32-byte digest.
    pub(crate) fn digest(&mut self) -> Result<[u8; 32], CodecError> {
        let slice = self.take(32)?;
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(slice);
        Ok(bytes)
    }

    /// Reads an optional byte string.
    pub(crate) fn optional_bytes(&mut self) -> Result<Option<&'a [u8]>, CodecError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.bytes()?)),
            _ => Err(CodecError::InvalidTag),
        }
    }

    /// Reads a collection count and enforces the global cap.
    pub(crate) fn count(&mut self) -> Result<usize, CodecError> {
        let count = usize::try_from(self.u32()?).map_err(|_| CodecError::LengthOverflow)?;
        if count > MAX_COLLECTION_ENTRIES {
            return Err(CodecError::TooManyEntries);
        }
        if count > self.remaining() {
            return Err(CodecError::Truncated);
        }
        Ok(count)
    }

    /// Reads a scope version vector.
    pub(crate) fn version_vector(&mut self) -> Result<ScopeVersionVector, CodecError> {
        Ok(ScopeVersionVector::from_vector(self.raw_version_vector()?))
    }

    /// Reads a version vector.
    pub(crate) fn raw_version_vector(&mut self) -> Result<VersionVector, CodecError> {
        let count = self.count()?;
        let mut vector = VersionVector::new();
        for _ in 0..count {
            let shard_id = ShardId::new(self.bytes()?.to_vec());
            let commit_version = CommitVersion::new(self.u64()?);
            vector.set(shard_id, commit_version);
        }
        Ok(vector)
    }

    fn length(&mut self) -> Result<usize, CodecError> {
        let length = usize::try_from(self.u32()?).map_err(|_| CodecError::LengthOverflow)?;
        if length > self.remaining() {
            return Err(CodecError::Truncated);
        }
        Ok(length)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(CodecError::LengthOverflow)?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(CodecError::Truncated)?;
        self.position = end;
        Ok(slice)
    }
}
