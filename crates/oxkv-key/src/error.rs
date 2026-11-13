//! Stable key encoding errors.
//!
//! Each error names one rule. The text names a byte offset where possible.
//! The text never echoes the key bytes.
//!
//! [`KeyError::code`] returns a code from the shared error registry.

use core::fmt;

use crate::{MAX_COMPONENT_BYTES, MAX_COMPONENTS, MAX_KEY_BYTES};

/// The size limit that a key operation exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyLimit {
    /// The total key size.
    KeyBytes,
    /// The payload size of one component.
    ComponentBytes,
    /// The component count.
    ComponentCount,
}

impl KeyLimit {
    /// Returns the limit value.
    #[must_use]
    pub const fn value(self) -> usize {
        match self {
            Self::KeyBytes => MAX_KEY_BYTES,
            Self::ComponentBytes => MAX_COMPONENT_BYTES,
            Self::ComponentCount => MAX_COMPONENTS,
        }
    }

    /// Returns the limit unit.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::KeyBytes | Self::ComponentBytes => "bytes",
            Self::ComponentCount => "components",
        }
    }
}

impl fmt::Display for KeyLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyBytes => formatter.write_str("key size"),
            Self::ComponentBytes => formatter.write_str("component size"),
            Self::ComponentCount => formatter.write_str("component count"),
        }
    }
}

/// An error from key encoding or key decoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyError {
    /// The first byte is not the v1 key encoding version.
    UnknownVersion {
        /// The unknown version byte.
        version: u8,
    },
    /// The key structure is malformed.
    Malformed {
        /// The byte offset of the defect.
        offset: usize,
    },
    /// The key does not match its declared table schema.
    NonCanonical {
        /// The byte offset of the first component.
        offset: usize,
    },
    /// The key belongs to a different table.
    OutsideTable {
        /// The expected table name. The name is at most 63 bytes.
        table: String,
    },
    /// A text component is not valid UTF-8.
    InvalidUtf8 {
        /// The byte offset of the defect.
        offset: usize,
    },
    /// A size limit is exceeded.
    TooLarge {
        /// The limit that is exceeded.
        limit: KeyLimit,
        /// The observed size.
        found: usize,
    },
    /// A user key uses `0xFF` as a component tag.
    ReservedSystemMarker {
        /// The byte offset of the marker.
        offset: usize,
    },
    /// A system key uses an unregistered system identifier.
    UnknownSystemId {
        /// The unknown identifier.
        id: u8,
    },
}

impl KeyError {
    /// Returns the stable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownVersion { .. } => "key.unknown_version",
            Self::Malformed { .. } => "key.malformed",
            Self::NonCanonical { .. } => "key.non_canonical",
            Self::OutsideTable { .. } => "key.outside_table",
            Self::InvalidUtf8 { .. } => "key.invalid_utf8",
            Self::TooLarge { .. } => "key.too_large",
            Self::ReservedSystemMarker { .. } => "key.reserved_system_marker",
            Self::UnknownSystemId { .. } => "key.unknown_system_id",
        }
    }

    /// Returns the byte offset of the defect when the error has one.
    #[must_use]
    pub const fn offset(&self) -> Option<usize> {
        match self {
            Self::Malformed { offset }
            | Self::NonCanonical { offset }
            | Self::InvalidUtf8 { offset }
            | Self::ReservedSystemMarker { offset } => Some(*offset),
            Self::UnknownVersion { .. }
            | Self::OutsideTable { .. }
            | Self::TooLarge { .. }
            | Self::UnknownSystemId { .. } => None,
        }
    }
}

impl fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownVersion { version } => {
                write!(
                    formatter,
                    "The key encoding version {version:#04x} is unknown."
                )
            }
            Self::Malformed { offset } => {
                write!(formatter, "The key is malformed at byte offset {offset}.")
            }
            Self::NonCanonical { offset } => {
                write!(
                    formatter,
                    "The key is not canonical at byte offset {offset}."
                )
            }
            Self::OutsideTable { table } => {
                write!(formatter, "The key is outside the table {table}.")
            }
            Self::InvalidUtf8 { offset } => {
                write!(
                    formatter,
                    "The text component is not valid UTF-8 at byte offset {offset}."
                )
            }
            Self::TooLarge { limit, found } => {
                write!(
                    formatter,
                    "The {limit} is too large. The limit is {} {}. The value is {found}.",
                    limit.value(),
                    limit.unit()
                )
            }
            Self::ReservedSystemMarker { offset } => {
                write!(
                    formatter,
                    "A user component uses the reserved system marker at byte offset {offset}."
                )
            }
            Self::UnknownSystemId { id } => {
                write!(
                    formatter,
                    "The system identifier {id:#04x} is not registered."
                )
            }
        }
    }
}

impl std::error::Error for KeyError {}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::error::lookup;

    fn every_error() -> Vec<KeyError> {
        vec![
            KeyError::UnknownVersion { version: 2 },
            KeyError::Malformed { offset: 3 },
            KeyError::NonCanonical { offset: 4 },
            KeyError::OutsideTable {
                table: "accounts".to_owned(),
            },
            KeyError::InvalidUtf8 { offset: 5 },
            KeyError::TooLarge {
                limit: KeyLimit::KeyBytes,
                found: 5000,
            },
            KeyError::ReservedSystemMarker { offset: 6 },
            KeyError::UnknownSystemId { id: 8 },
        ]
    }

    #[test]
    fn every_error_code_is_registered() {
        for error in every_error() {
            assert!(
                lookup(error.code()).is_some(),
                "the code {} is not registered",
                error.code()
            );
        }
    }

    #[test]
    fn every_error_text_names_the_rule() {
        for error in every_error() {
            assert_ne!(error.to_string(), "");
        }
    }
}
