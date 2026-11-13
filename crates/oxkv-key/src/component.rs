//! Typed key components.
//!
//! A component starts with a one-byte type tag. Text and bytes escape `0x00`
//! as `0x00 0xFF` and end with `0x00 0x00`. The escape keeps byte order equal
//! to value order.

use core::fmt;

use oxkv_contracts::keyspace::{
    USER_COMPONENT_BOOL, USER_COMPONENT_BYTES, USER_COMPONENT_I64, USER_COMPONENT_TEXT,
    USER_COMPONENT_U64,
};

use crate::{KeyError, KeyLimit, MAX_COMPONENT_BYTES};

/// The type of one key component.
///
/// The declaration order matches the tag order. A derived order on
/// [`Component`] uses the same order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ComponentType {
    /// A UTF-8 text component.
    Text,
    /// An opaque bytes component.
    Bytes,
    /// An unsigned 64-bit component.
    U64,
    /// A signed 64-bit component.
    I64,
    /// A boolean component.
    Bool,
}

impl ComponentType {
    /// Returns the stable wire tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Text => USER_COMPONENT_TEXT,
            Self::Bytes => USER_COMPONENT_BYTES,
            Self::U64 => USER_COMPONENT_U64,
            Self::I64 => USER_COMPONENT_I64,
            Self::Bool => USER_COMPONENT_BOOL,
        }
    }

    /// Reads a component tag.
    ///
    /// Returns `None` for the system marker and for an unknown tag. The caller
    /// maps the system marker to [`KeyError::ReservedSystemMarker`].
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            USER_COMPONENT_TEXT => Some(Self::Text),
            USER_COMPONENT_BYTES => Some(Self::Bytes),
            USER_COMPONENT_U64 => Some(Self::U64),
            USER_COMPONENT_I64 => Some(Self::I64),
            USER_COMPONENT_BOOL => Some(Self::Bool),
            _ => None,
        }
    }

    /// Returns the stable type name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Bytes => "bytes",
            Self::U64 => "u64",
            Self::I64 => "i64",
            Self::Bool => "bool",
        }
    }
}

impl fmt::Display for ComponentType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One typed key component.
///
/// The derived order matches the encoded byte order. The variants appear in
/// tag order: text, bytes, u64, i64, and bool.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Component {
    /// A UTF-8 text component.
    Text(String),
    /// An opaque bytes component.
    Bytes(Vec<u8>),
    /// An unsigned 64-bit component.
    U64(u64),
    /// A signed 64-bit component.
    I64(i64),
    /// A boolean component.
    Bool(bool),
}

impl Component {
    /// Creates a text component.
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// Creates a bytes component.
    #[must_use]
    pub fn bytes(value: impl Into<Vec<u8>>) -> Self {
        Self::Bytes(value.into())
    }

    /// Creates an unsigned 64-bit component.
    #[must_use]
    pub const fn u64(value: u64) -> Self {
        Self::U64(value)
    }

    /// Creates a signed 64-bit component.
    #[must_use]
    pub const fn i64(value: i64) -> Self {
        Self::I64(value)
    }

    /// Creates a boolean component.
    #[must_use]
    pub const fn bool(value: bool) -> Self {
        Self::Bool(value)
    }

    /// Returns the component type.
    #[must_use]
    pub const fn component_type(&self) -> ComponentType {
        match self {
            Self::Text(_) => ComponentType::Text,
            Self::Bytes(_) => ComponentType::Bytes,
            Self::U64(_) => ComponentType::U64,
            Self::I64(_) => ComponentType::I64,
            Self::Bool(_) => ComponentType::Bool,
        }
    }

    /// Writes the tag and payload.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::TooLarge`] when the payload exceeds 1 KiB.
    pub(crate) fn write(&self, out: &mut Vec<u8>) -> Result<(), KeyError> {
        out.push(self.component_type().tag());
        match self {
            Self::Text(value) => write_escaped(out, value.as_bytes())?,
            Self::Bytes(value) => write_escaped(out, value)?,
            Self::U64(value) => out.extend_from_slice(&value.to_be_bytes()),
            Self::I64(value) => {
                let mut bytes = value.to_be_bytes();
                bytes[0] ^= 0x80;
                out.extend_from_slice(&bytes);
            }
            Self::Bool(value) => out.push(u8::from(*value)),
        }
        Ok(())
    }
}

fn write_escaped(out: &mut Vec<u8>, payload: &[u8]) -> Result<(), KeyError> {
    if payload.len() > MAX_COMPONENT_BYTES {
        return Err(KeyError::TooLarge {
            limit: KeyLimit::ComponentBytes,
            found: payload.len(),
        });
    }
    for &byte in payload {
        if byte == 0x00 {
            out.push(0x00);
            out.push(0xFF);
        } else {
            out.push(byte);
        }
    }
    out.push(0x00);
    out.push(0x00);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::keyspace::SYSTEM_MARKER;

    #[test]
    fn every_component_tag_is_stable() {
        assert_eq!(ComponentType::Text.tag(), 0x10);
        assert_eq!(ComponentType::Bytes.tag(), 0x11);
        assert_eq!(ComponentType::U64.tag(), 0x20);
        assert_eq!(ComponentType::I64.tag(), 0x21);
        assert_eq!(ComponentType::Bool.tag(), 0x30);
        assert_eq!(ComponentType::from_tag(SYSTEM_MARKER), None);
        assert_eq!(ComponentType::from_tag(0x99), None);
    }

    #[test]
    fn the_component_order_matches_the_tag_order() {
        let mut components = [
            Component::bool(false),
            Component::i64(0),
            Component::u64(0),
            Component::bytes(Vec::new()),
            Component::text(""),
        ];
        components.sort();
        let tags: Vec<u8> = components
            .iter()
            .map(|component| component.component_type().tag())
            .collect();
        assert_eq!(tags, vec![0x10, 0x11, 0x20, 0x21, 0x30]);
    }

    #[test]
    fn an_oversized_component_fails() {
        let component = Component::bytes(vec![b'a'; MAX_COMPONENT_BYTES + 1]);
        let mut out = Vec::new();
        assert_eq!(
            component.write(&mut out),
            Err(KeyError::TooLarge {
                limit: KeyLimit::ComponentBytes,
                found: MAX_COMPONENT_BYTES + 1
            })
        );
    }
}
