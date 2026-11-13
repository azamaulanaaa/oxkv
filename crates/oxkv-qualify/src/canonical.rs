//! Canonical byte encoding and deterministic text helpers.
//!
//! Every value on a semantic path uses fixed-width integers and big-endian
//! byte order. No function in this module uses floating point or a locale.

use core::fmt::Write as _;

use oxkv_contracts::version::Hash32;
use serde_json::Value;

/// An error from canonical encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalError {
    /// A JSON number is not an integer.
    FloatNotAllowed,
    /// A JSON value has no canonical form.
    UnsupportedValue,
}

impl CanonicalError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::FloatNotAllowed => "manifest.float_not_allowed",
            Self::UnsupportedValue => "manifest.unknown_field",
        }
    }
}

/// Writes canonical, length-prefixed bytes.
#[derive(Debug, Default, Clone)]
pub struct CanonicalWriter {
    bytes: Vec<u8>,
}

impl CanonicalWriter {
    /// Creates an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Appends one byte.
    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    /// Appends an unsigned 32-bit integer in big-endian order.
    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends an unsigned 64-bit integer in big-endian order.
    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends a signed 64-bit integer in big-endian order.
    pub fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Appends a length prefix and a byte string.
    pub fn bytes(&mut self, value: &[u8]) {
        self.u64(u64::try_from(value.len()).unwrap_or(u64::MAX));
        self.bytes.extend_from_slice(value);
    }

    /// Appends a length prefix and a UTF-8 string.
    pub fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// Returns the encoded bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Returns the lowercase hexadecimal form of a byte slice.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// Decodes a lowercase or uppercase hexadecimal string.
///
/// # Errors
///
/// Returns `None` when the text has an odd length or a non-hexadecimal byte.
#[must_use]
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().as_chunks::<2>().0 {
        let high = nibble(pair[0])?;
        let low = nibble(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Some(bytes)
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Encodes one JSON value in a canonical form.
///
/// Object keys are sorted. Numbers must be integers. Strings escape only the
/// characters that JSON requires.
///
/// # Errors
///
/// Returns [`CanonicalError::FloatNotAllowed`] for a non-integer number.
/// Returns [`CanonicalError::UnsupportedValue`] for a value with no encoding.
pub fn canonical_json(value: &Value) -> Result<String, CanonicalError> {
    let mut text = String::new();
    write_json(value, &mut text)?;
    Ok(text)
}

fn write_json(value: &Value, output: &mut String) -> Result<(), CanonicalError> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(flag) => output.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(integer) = number.as_u64() {
                let _ = write!(output, "{integer}");
            } else if let Some(integer) = number.as_i64() {
                let _ = write!(output, "{integer}");
            } else {
                return Err(CanonicalError::FloatNotAllowed);
            }
        }
        Value::String(text) => write_json_string(text, output),
        Value::Array(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_json(item, output)?;
            }
            output.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            output.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_json_string(key, output);
                output.push(':');
                if let Some(item) = map.get(key) {
                    write_json(item, output)?;
                }
            }
            output.push('}');
        }
    }
    Ok(())
}

fn write_json_string(text: &str, output: &mut String) {
    output.push('"');
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            character if character < '\u{20}' => {
                let _ = write!(output, "\\u{:04x}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

/// Returns a BLAKE3 digest of a canonical domain and bytes.
#[must_use]
pub fn digest(domain: &str, bytes: &[u8]) -> Hash32 {
    let mut writer = CanonicalWriter::new();
    writer.text(domain);
    writer.bytes(bytes);
    Hash32::of(&writer.finish())
}

/// Returns a canonical ordering key for a text value.
///
/// The key uses the UTF-8 bytes of the text. It does not use a locale.
#[must_use]
pub fn order_key(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}
