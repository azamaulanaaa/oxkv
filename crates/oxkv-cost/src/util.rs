//! Small deterministic helpers.
//!
//! The helpers avoid casts. A value that does not fit becomes the maximum of
//! the target type. That choice over-reports cost and never under-reports it.

/// Encodes bytes as lowercase hexadecimal text.
#[must_use]
pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// Converts a length to `u64`. A value that does not fit becomes `u64::MAX`.
#[must_use]
pub(crate) fn len_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Converts a length to `u32`. A value that does not fit becomes `u32::MAX`.
#[must_use]
pub(crate) fn len_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Converts a `u32` to `usize`. A value that does not fit becomes `usize::MAX`.
#[must_use]
pub(crate) fn u32_to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Encodes one JSON string. The output uses one fixed escape form.
#[must_use]
pub(crate) fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            low if low < '\u{0020}' => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let value = u32::from(low);
                out.push_str("\\u00");
                let high = u8::try_from((value >> 4) & 0x0f).unwrap_or(0);
                let low_nibble = u8::try_from(value & 0x0f).unwrap_or(0);
                out.push(char::from(HEX[usize::from(high)]));
                out.push(char::from(HEX[usize::from(low_nibble)]));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Encodes one JSON string from bytes. The bytes become hexadecimal text.
#[must_use]
pub(crate) fn json_hex(bytes: &[u8]) -> String {
    json_string(&hex(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_uses_lowercase_and_two_digits_per_byte() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(hex(&[]), "");
    }

    #[test]
    fn json_string_escapes_the_fixed_character_set() {
        assert_eq!(json_string("a\"b\\c\n\t"), "\"a\\\"b\\\\c\\n\\t\"");
        assert_eq!(json_string("\u{0001}"), "\"\\u0001\"");
        assert_eq!(json_string("plain"), "\"plain\"");
    }

    #[test]
    fn length_conversions_saturate() {
        assert_eq!(len_to_u64(7), 7);
        assert_eq!(len_to_u32(7), 7);
        assert_eq!(u32_to_usize(7), 7);
    }
}
