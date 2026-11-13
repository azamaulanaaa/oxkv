//! Canonical byte encoding for the portable contracts.
//!
//! Every integer uses a fixed width and big-endian byte order. Every length
//! prefix is a big-endian `u64`. Every text value is UTF-8 with a length
//! prefix. The format has no floating-point field.
//!
//! The encoder is append-only. The decoder rejects a truncated input, a
//! trailing byte, an invalid boolean, an invalid UTF-8 value, and an
//! impossible length.

/// An error from canonical encoding or decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    /// The input ends before the field is complete.
    #[error("The input ends before the field is complete.")]
    Truncated,
    /// The input has bytes after the last field.
    #[error("The input has bytes after the last field.")]
    TrailingBytes,
    /// A boolean field is not zero or one.
    #[error("A boolean field is not zero or one.")]
    InvalidBoolean,
    /// A text field is not valid UTF-8.
    #[error("A text field is not valid UTF-8.")]
    InvalidUtf8,
    /// A length prefix does not fit in memory.
    #[error("A length prefix is too large.")]
    LengthOverflow,
    /// A hexadecimal text value is malformed.
    #[error("A hexadecimal text value is malformed.")]
    InvalidHex,
}

impl EncodeError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Truncated => "format.truncated_snapshot",
            Self::TrailingBytes | Self::InvalidBoolean | Self::InvalidUtf8 | Self::InvalidHex => {
                "format.corrupt_page"
            }
            Self::LengthOverflow => "format.page_too_large",
        }
    }
}

/// A canonical byte encoder.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    /// Creates an empty encoder.
    #[must_use]
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Appends one byte.
    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    /// Appends a big-endian `u16`.
    pub fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends a big-endian `u32`.
    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends a big-endian `u64`.
    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends a big-endian `i64`.
    pub fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends one boolean byte.
    pub fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    /// Appends a `u64` length and the raw bytes.
    pub fn bytes(&mut self, value: &[u8]) {
        self.u64(u64::try_from(value.len()).unwrap_or(u64::MAX));
        self.bytes.extend_from_slice(value);
    }

    /// Appends a `u64` length and the UTF-8 bytes.
    pub fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// Appends raw bytes with no length prefix.
    pub fn raw(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    /// Returns the encoded bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the encoded bytes without a copy.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Appends one optional `u64` with a presence byte.
    pub fn optional_u64(&mut self, value: Option<u64>) {
        match value {
            Some(value) => {
                self.u8(1);
                self.u64(value);
            }
            None => self.u8(0),
        }
    }
}

/// A canonical byte decoder.
#[derive(Debug, Clone)]
pub struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    /// Creates a decoder over the input bytes.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// Returns the number of unread bytes.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    /// Returns true when every byte is read.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.position == self.bytes.len()
    }

    /// Rejects trailing bytes.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::TrailingBytes`] when bytes remain.
    pub fn finish(&self) -> Result<(), EncodeError> {
        if self.is_finished() {
            Ok(())
        } else {
            Err(EncodeError::TrailingBytes)
        }
    }

    /// Reads raw bytes with no length prefix.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    pub fn take(&mut self, length: usize) -> Result<&'a [u8], EncodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(EncodeError::LengthOverflow)?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(EncodeError::Truncated)?;
        self.position = end;
        Ok(slice)
    }

    /// Reads one byte.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    pub fn u8(&mut self) -> Result<u8, EncodeError> {
        let slice = self.take(1)?;
        Ok(slice[0])
    }

    /// Reads a big-endian `u16`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    pub fn u16(&mut self) -> Result<u16, EncodeError> {
        let slice = self.take(2)?;
        let mut bytes = [0_u8; 2];
        bytes.copy_from_slice(slice);
        Ok(u16::from_be_bytes(bytes))
    }

    /// Reads a big-endian `u32`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    pub fn u32(&mut self) -> Result<u32, EncodeError> {
        let slice = self.take(4)?;
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(slice);
        Ok(u32::from_be_bytes(bytes))
    }

    /// Reads a big-endian `u64`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    pub fn u64(&mut self) -> Result<u64, EncodeError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(u64::from_be_bytes(bytes))
    }

    /// Reads a big-endian `i64`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    pub fn i64(&mut self) -> Result<i64, EncodeError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(i64::from_be_bytes(bytes))
    }

    /// Reads one boolean byte.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    /// Returns [`EncodeError::InvalidBoolean`] when the byte is not zero or
    /// one.
    pub fn bool(&mut self) -> Result<bool, EncodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(EncodeError::InvalidBoolean),
        }
    }

    /// Reads a `u64` length and the raw bytes.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    /// Returns [`EncodeError::LengthOverflow`] when the length does not fit.
    pub fn bytes(&mut self) -> Result<&'a [u8], EncodeError> {
        let length = self.u64()?;
        let length = usize::try_from(length).map_err(|_| EncodeError::LengthOverflow)?;
        self.take(length)
    }

    /// Reads a `u64` length and a UTF-8 value.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    /// Returns [`EncodeError::InvalidUtf8`] when the bytes are not UTF-8.
    pub fn text(&mut self) -> Result<String, EncodeError> {
        let bytes = self.bytes()?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| EncodeError::InvalidUtf8)
    }

    /// Reads one optional `u64` with a presence byte.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Truncated`] when the input is too short.
    /// Returns [`EncodeError::InvalidBoolean`] when the presence byte is not
    /// zero or one.
    pub fn optional_u64(&mut self) -> Result<Option<u64>, EncodeError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.u64()?)),
            _ => Err(EncodeError::InvalidBoolean),
        }
    }
}

/// Returns the lowercase hexadecimal form of the bytes.
#[must_use]
pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// Decodes a lowercase or uppercase hexadecimal value.
///
/// # Errors
///
/// Returns [`EncodeError::InvalidHex`] when the text has an odd length or a
/// non-hexadecimal character.
pub fn decode_hex(text: &str) -> Result<Vec<u8>, EncodeError> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(EncodeError::InvalidHex);
    }
    let mut output = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_value(bytes[index]).ok_or(EncodeError::InvalidHex)?;
        let low = hex_value(bytes[index + 1]).ok_or(EncodeError::InvalidHex)?;
        output.push((high << 4) | low);
        index += 2;
    }
    Ok(output)
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_integer_uses_big_endian_fixed_width() {
        let mut encoder = Encoder::new();
        encoder.u8(0x01);
        encoder.u16(0x0203);
        encoder.u32(0x0405_0607);
        encoder.u64(0x0809_0a0b_0c0d_0e0f);
        encoder.i64(-1);
        let bytes = encoder.finish();
        assert_eq!(bytes[0], 0x01);
        assert_eq!(&bytes[1..3], &[0x02, 0x03]);
        assert_eq!(&bytes[3..7], &[0x04, 0x05, 0x06, 0x07]);
        assert_eq!(
            &bytes[7..15],
            &[0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f]
        );
        assert_eq!(&bytes[15..23], &[0xff; 8]);
    }

    #[test]
    fn a_decode_round_trip_keeps_every_value() {
        let mut encoder = Encoder::new();
        encoder.bytes(b"key");
        encoder.text("value");
        encoder.bool(true);
        encoder.optional_u64(Some(7));
        encoder.optional_u64(None);
        let bytes = encoder.finish();
        let mut decoder = Decoder::new(&bytes);
        assert_eq!(decoder.bytes(), Ok(b"key".as_slice()));
        assert_eq!(decoder.text(), Ok("value".to_owned()));
        assert_eq!(decoder.bool(), Ok(true));
        assert_eq!(decoder.optional_u64(), Ok(Some(7)));
        assert_eq!(decoder.optional_u64(), Ok(None));
        assert_eq!(decoder.finish(), Ok(()));
    }

    #[test]
    fn a_truncated_input_fails() {
        let bytes = [0_u8; 3];
        let mut decoder = Decoder::new(&bytes);
        assert_eq!(decoder.u64(), Err(EncodeError::Truncated));
        assert_eq!(EncodeError::Truncated.code(), "format.truncated_snapshot");
    }

    #[test]
    fn a_trailing_byte_fails() {
        let bytes = [1_u8, 2];
        let mut decoder = Decoder::new(&bytes);
        assert_eq!(decoder.u8(), Ok(1));
        assert_eq!(decoder.finish(), Err(EncodeError::TrailingBytes));
    }

    #[test]
    fn an_invalid_boolean_fails() {
        let bytes = [2_u8];
        let mut decoder = Decoder::new(&bytes);
        assert_eq!(decoder.bool(), Err(EncodeError::InvalidBoolean));
    }

    #[test]
    fn hexadecimal_text_round_trips() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(decode_hex("000FA5ff"), Ok(vec![0x00, 0x0f, 0xa5, 0xff]));
        assert_eq!(decode_hex("abc"), Err(EncodeError::InvalidHex));
        assert_eq!(decode_hex("zz"), Err(EncodeError::InvalidHex));
    }
}
