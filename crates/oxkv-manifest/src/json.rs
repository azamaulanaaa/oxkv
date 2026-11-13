//! The canonical JSON subset for manifest v1.
//!
//! The subset has integers in the i64 range, strings, booleans, arrays, and
//! objects. It has no floating point values and no null value. Object keys
//! sort by byte order. The parser rejects a duplicate key.
//!
//! The encoder and the parser are independent of serde. The bytes stay stable
//! across hosts and targets.

use std::collections::BTreeMap;

use crate::error::ManifestError;

/// The maximum nesting depth of one canonical value.
const MAX_DEPTH: usize = 128;

/// One value in the canonical JSON subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CanonicalValue {
    /// A signed 64-bit integer.
    Integer(i64),
    /// A UTF-8 string.
    String(String),
    /// A boolean.
    Bool(bool),
    /// An ordered list.
    Array(Vec<Self>),
    /// A map with keys in byte order.
    Object(BTreeMap<String, Self>),
}

impl CanonicalValue {
    /// Returns the canonical value name.
    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::Integer(_) => "an integer",
            Self::String(_) => "a string",
            Self::Bool(_) => "a boolean",
            Self::Array(_) => "an array",
            Self::Object(_) => "an object",
        }
    }

    /// Returns the array.
    pub(crate) fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(array) => Some(array),
            _ => None,
        }
    }

    /// Returns the string.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    /// Returns the integer.
    pub(crate) const fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the boolean.
    pub(crate) const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Encodes the value as canonical JSON bytes.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut text = String::new();
        write_value(self, &mut text);
        text.into_bytes()
    }
}

/// Parses one canonical JSON value.
///
/// # Errors
///
/// Returns [`ManifestError::JsonSyntax`] for malformed text. Returns
/// [`ManifestError::DuplicateKey`] for a duplicate object key. Returns
/// [`ManifestError::FloatNotAllowed`] for a number that is not an i64 integer.
pub(crate) fn parse(text: &str) -> Result<CanonicalValue, ManifestError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        index: 0,
    };
    parser.skip_whitespace();
    let value = parser.parse_value("$", 0)?;
    parser.skip_whitespace();
    if parser.index != parser.bytes.len() {
        return Err(syntax("The JSON text has trailing data."));
    }
    Ok(value)
}

/// One recursive-descent parser.
struct Parser<'a> {
    /// The input bytes.
    bytes: &'a [u8],
    /// The current byte offset.
    index: usize,
}

impl Parser<'_> {
    /// Skips JSON whitespace.
    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.bytes.get(self.index) {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.index += 1;
            } else {
                break;
            }
        }
    }

    /// Returns the next byte without a move.
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    /// Consumes one expected byte.
    fn expect(&mut self, byte: u8) -> Result<(), ManifestError> {
        if self.peek() == Some(byte) {
            self.index += 1;
            Ok(())
        } else {
            Err(syntax("The JSON text has an unexpected byte."))
        }
    }

    /// Parses one value.
    fn parse_value(&mut self, path: &str, depth: usize) -> Result<CanonicalValue, ManifestError> {
        if depth > MAX_DEPTH {
            return Err(syntax("The JSON value is too deep."));
        }
        match self.peek() {
            Some(b'{') => self.parse_object(path, depth),
            Some(b'[') => self.parse_array(path, depth),
            Some(b'"') => self.parse_string().map(CanonicalValue::String),
            Some(b't') => {
                self.parse_keyword("true")?;
                Ok(CanonicalValue::Bool(true))
            }
            Some(b'f') => {
                self.parse_keyword("false")?;
                Ok(CanonicalValue::Bool(false))
            }
            Some(b'n') => Err(syntax("The canonical JSON subset has no null value.")),
            Some(b'-' | b'0'..=b'9') => self.parse_number(path),
            _ => Err(syntax("The JSON text has an unexpected value.")),
        }
    }

    /// Parses an object.
    fn parse_object(&mut self, path: &str, depth: usize) -> Result<CanonicalValue, ManifestError> {
        self.expect(b'{')?;
        let mut object = BTreeMap::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.index += 1;
            return Ok(CanonicalValue::Object(object));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            let child = format!("{path}.{key}");
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.parse_value(&child, depth + 1)?;
            if object.insert(key.clone(), value).is_some() {
                return Err(ManifestError::DuplicateKey { key });
            }
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.index += 1;
                }
                Some(b'}') => {
                    self.index += 1;
                    return Ok(CanonicalValue::Object(object));
                }
                _ => return Err(syntax("The JSON object is not closed.")),
            }
        }
    }

    /// Parses an array.
    fn parse_array(&mut self, path: &str, depth: usize) -> Result<CanonicalValue, ManifestError> {
        self.expect(b'[')?;
        let mut array = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.index += 1;
            return Ok(CanonicalValue::Array(array));
        }
        loop {
            self.skip_whitespace();
            let child = format!("{path}[{}]", array.len());
            let value = self.parse_value(&child, depth + 1)?;
            array.push(value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.index += 1;
                }
                Some(b']') => {
                    self.index += 1;
                    return Ok(CanonicalValue::Array(array));
                }
                _ => return Err(syntax("The JSON array is not closed.")),
            }
        }
    }

    /// Parses a string.
    fn parse_string(&mut self) -> Result<String, ManifestError> {
        self.expect(b'"')?;
        let mut text = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(syntax("The JSON string is not closed."));
            };
            match byte {
                b'"' => {
                    self.index += 1;
                    return Ok(text);
                }
                b'\\' => {
                    self.index += 1;
                    self.parse_escape(&mut text)?;
                }
                0x00..=0x1F => {
                    return Err(syntax("A JSON string holds an unescaped control byte."));
                }
                _ => {
                    let start = self.index;
                    while let Some(next) = self.peek() {
                        if next == b'"' || next == b'\\' || next < 0x20 {
                            break;
                        }
                        self.index += 1;
                    }
                    let segment = std::str::from_utf8(&self.bytes[start..self.index])
                        .map_err(|_| syntax("The JSON string is not valid UTF-8."))?;
                    text.push_str(segment);
                }
            }
        }
    }

    /// Parses one escape sequence.
    fn parse_escape(&mut self, text: &mut String) -> Result<(), ManifestError> {
        let Some(byte) = self.peek() else {
            return Err(syntax("The JSON escape is not complete."));
        };
        self.index += 1;
        match byte {
            b'"' => text.push('"'),
            b'\\' => text.push('\\'),
            b'/' => text.push('/'),
            b'b' => text.push('\u{0008}'),
            b'f' => text.push('\u{000C}'),
            b'n' => text.push('\n'),
            b'r' => text.push('\r'),
            b't' => text.push('\t'),
            b'u' => {
                let first = self.parse_hex4()?;
                let scalar = if (0xD800..=0xDBFF).contains(&first) {
                    if self.peek() != Some(b'\\') {
                        return Err(syntax("The JSON string has an unpaired surrogate."));
                    }
                    self.index += 1;
                    if self.peek() != Some(b'u') {
                        return Err(syntax("The JSON string has an unpaired surrogate."));
                    }
                    self.index += 1;
                    let second = self.parse_hex4()?;
                    if !(0xDC00..=0xDFFF).contains(&second) {
                        return Err(syntax("The JSON string has an unpaired surrogate."));
                    }
                    let scalar = 0x1_0000
                        + ((u32::from(first) - 0xD800) << 10)
                        + (u32::from(second) - 0xDC00);
                    char::from_u32(scalar)
                        .ok_or_else(|| syntax("The JSON string has an invalid scalar."))?
                } else if (0xDC00..=0xDFFF).contains(&first) {
                    return Err(syntax("The JSON string has an unpaired surrogate."));
                } else {
                    char::from_u32(u32::from(first))
                        .ok_or_else(|| syntax("The JSON string has an invalid scalar."))?
                };
                text.push(scalar);
            }
            _ => return Err(syntax("The JSON escape is not valid.")),
        }
        Ok(())
    }

    /// Parses four hexadecimal digits.
    fn parse_hex4(&mut self) -> Result<u16, ManifestError> {
        let Some(end) = self.index.checked_add(4) else {
            return Err(syntax("The JSON escape is not complete."));
        };
        let digits = self
            .bytes
            .get(self.index..end)
            .ok_or_else(|| syntax("The JSON escape is not complete."))?;
        let mut value = 0_u16;
        for digit in digits {
            let nibble = match digit {
                b'0'..=b'9' => u16::from(digit - b'0'),
                b'a'..=b'f' => u16::from(digit - b'a') + 10,
                b'A'..=b'F' => u16::from(digit - b'A') + 10,
                _ => return Err(syntax("The JSON escape has a non-hex digit.")),
            };
            value = (value << 4) | nibble;
        }
        self.index = end;
        Ok(value)
    }

    /// Parses a keyword.
    fn parse_keyword(&mut self, keyword: &str) -> Result<(), ManifestError> {
        let end = self.index + keyword.len();
        if self.bytes.get(self.index..end) == Some(keyword.as_bytes()) {
            self.index = end;
            Ok(())
        } else {
            Err(syntax("The JSON keyword is not valid."))
        }
    }

    /// Parses an integer number.
    fn parse_number(&mut self, path: &str) -> Result<CanonicalValue, ManifestError> {
        let start = self.index;
        if self.peek() == Some(b'-') {
            self.index += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.index += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(syntax("The JSON number has a leading zero."));
                }
            }
            Some(b'1'..=b'9') => {
                self.index += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.index += 1;
                }
            }
            _ => return Err(syntax("The JSON number has no digit.")),
        }
        if matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return Err(ManifestError::FloatNotAllowed {
                path: path.to_owned(),
            });
        }
        let text = std::str::from_utf8(&self.bytes[start..self.index])
            .map_err(|_| syntax("The JSON number is not valid UTF-8."))?;
        let value = text
            .parse::<i64>()
            .map_err(|_| ManifestError::FloatNotAllowed {
                path: path.to_owned(),
            })?;
        Ok(CanonicalValue::Integer(value))
    }
}

/// Returns a syntax error.
fn syntax(detail: &str) -> ManifestError {
    ManifestError::JsonSyntax {
        detail: detail.to_owned(),
    }
}

/// Writes one value as canonical JSON.
fn write_value(value: &CanonicalValue, text: &mut String) {
    match value {
        CanonicalValue::Integer(number) => text.push_str(&number.to_string()),
        CanonicalValue::String(string) => write_string(string, text),
        CanonicalValue::Bool(boolean) => {
            text.push_str(if *boolean { "true" } else { "false" });
        }
        CanonicalValue::Array(array) => {
            text.push('[');
            for (index, item) in array.iter().enumerate() {
                if index != 0 {
                    text.push(',');
                }
                write_value(item, text);
            }
            text.push(']');
        }
        CanonicalValue::Object(object) => {
            text.push('{');
            for (index, (key, item)) in object.iter().enumerate() {
                if index != 0 {
                    text.push(',');
                }
                write_string(key, text);
                text.push(':');
                write_value(item, text);
            }
            text.push('}');
        }
    }
}

/// Writes one string with the minimal JSON escapes.
fn write_string(value: &str, text: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    text.push('"');
    for character in value.chars() {
        match character {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            '\u{0008}' => text.push_str("\\b"),
            '\u{000C}' => text.push_str("\\f"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '\u{0000}'..='\u{001F}' => {
                let code = u32::from(character);
                text.push_str("\\u00");
                text.push(char::from(
                    HEX[usize::try_from((code >> 4) & 0xF).unwrap_or(0)],
                ));
                text.push(char::from(HEX[usize::try_from(code & 0xF).unwrap_or(0)]));
            }
            _ => text.push(character),
        }
    }
    text.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_key_sorts_by_bytes() {
        let value = parse(r#"{"b":1,"a":2}"#).expect("the text is valid");
        assert_eq!(
            value.to_bytes(),
            br#"{"a":2,"b":1}"#.to_vec(),
            "the object keys do not sort"
        );
    }

    #[test]
    fn a_duplicate_key_is_rejected() {
        let error = parse(r#"{"a":1,"a":2}"#).expect_err("the key is duplicated");
        assert_eq!(error.code(), "manifest.duplicate_key");
    }

    #[test]
    fn a_float_is_rejected() {
        for text in ["1.5", "1e3", "-0.0", "1E2"] {
            let error = parse(text).expect_err("the number is a float");
            assert_eq!(error.code(), "manifest.float_not_allowed");
        }
    }

    #[test]
    fn an_integer_outside_i64_is_rejected() {
        let error = parse("9223372036854775808").expect_err("the integer is too large");
        assert_eq!(error.code(), "manifest.float_not_allowed");
    }

    #[test]
    fn a_number_round_trips_through_the_canonical_form() {
        for text in ["0", "-1", "9223372036854775807", "-9223372036854775808"] {
            let value = parse(text).expect("the integer is valid");
            assert_eq!(value.to_bytes(), text.as_bytes().to_vec());
        }
    }

    #[test]
    fn a_string_uses_the_minimal_escape() {
        let value = parse(r#""a\"b\\c\n\u00e9\uD83D\uDE00""#).expect("the string is valid");
        assert_eq!(
            value.to_bytes(),
            "\"a\\\"b\\\\c\\n\u{00E9}\u{1F600}\"".as_bytes().to_vec()
        );
    }

    #[test]
    fn null_and_trailing_data_are_rejected() {
        assert_eq!(
            parse("null").expect_err("null is not allowed").code(),
            "manifest.json_syntax"
        );
        assert_eq!(
            parse("1 2").expect_err("the text has trailing data").code(),
            "manifest.json_syntax"
        );
    }
}
