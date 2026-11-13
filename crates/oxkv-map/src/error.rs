//! Named shard map errors.
//!
//! Each validation error has one stable code from the contract error registry.

/// An error from shard map validation, decoding, or routing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    /// The map schema or canonical format version is unknown.
    #[error("The map version {found} is unknown.")]
    UnknownVersion {
        /// The version text that the reader does not support.
        found: String,
    },
    /// A key does not use key encoding v1.
    #[error("The key version is unknown.")]
    UnknownKeyVersion {
        /// The first key byte, when the key is not empty.
        found: Option<u8>,
    },
    /// The key does not contain a valid table prefix.
    #[error("The encoded key is malformed: {reason}.")]
    MalformedKey {
        /// The rule that the key breaks.
        reason: &'static str,
    },
    /// Two shard ranges overlap.
    #[error("The shard ranges {left} and {right} overlap.")]
    Overlap {
        /// The lower range, as hexadecimal text.
        left: String,
        /// The upper range, as hexadecimal text.
        right: String,
    },
    /// A shard range does not cover the full table prefix.
    ///
    /// A gap between two table ranges is normal. This error means that one
    /// shard range does not cover the full prefix of its own table.
    #[error("The shard range {shard_id} for table {table} has a gap.")]
    Gap {
        /// The shard identifier, as hexadecimal text.
        shard_id: String,
        /// The table name.
        table: String,
    },
    /// Two shard entries use the same shard identifier.
    #[error("The shard identifier {shard_id} occurs more than one time.")]
    DuplicateId {
        /// The duplicate identifier, as hexadecimal text.
        shard_id: String,
    },
    /// A shard range is empty or inverted.
    #[error("The shard range {shard_id} is empty.")]
    EmptyRange {
        /// The shard identifier, as hexadecimal text.
        shard_id: String,
    },
    /// The map does not contain a declared table.
    #[error("The map does not contain the table {table}.")]
    TableUnmapped {
        /// The missing table name.
        table: String,
    },
    /// A table occurs in more than one shard.
    #[error("The table {table} spans more than one shard.")]
    TableSplitUnsupported {
        /// The split table name.
        table: String,
    },
    /// A table name does not match the table name grammar.
    #[error("The table name {name} is not valid.")]
    InvalidTable {
        /// The invalid name.
        name: String,
    },
    /// The canonical map bytes are malformed.
    #[error("The canonical map bytes are malformed at offset {offset}: {reason}.")]
    MalformedCanonical {
        /// The byte offset where the reader stopped.
        offset: usize,
        /// The rule that the bytes break.
        reason: &'static str,
    },
}

/// Returns the lowercase hexadecimal form of one byte slice.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

impl MapError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownVersion { .. } => "map.unknown_version",
            Self::UnknownKeyVersion { .. } => "key.unknown_version",
            Self::MalformedKey { .. } => "map.malformed_key",
            Self::InvalidTable { .. } => "map.invalid_table",
            Self::Overlap { .. } => "map.overlap",
            Self::Gap { .. } => "map.gap",
            Self::DuplicateId { .. } => "map.duplicate_id",
            Self::EmptyRange { .. } => "map.empty_range",
            Self::TableUnmapped { .. } => "map.table_unmapped",
            Self::TableSplitUnsupported { .. } => "map.table_split_unsupported",
            Self::MalformedCanonical { .. } => "map.malformed_canonical",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_one_registered_code() {
        let errors = [
            MapError::UnknownVersion {
                found: "oxkv/map/v2".to_owned(),
            },
            MapError::UnknownKeyVersion { found: Some(2) },
            MapError::MalformedKey {
                reason: "no table delimiter",
            },
            MapError::Overlap {
                left: "00".to_owned(),
                right: "01".to_owned(),
            },
            MapError::Gap {
                shard_id: "01".to_owned(),
                table: "accounts".to_owned(),
            },
            MapError::DuplicateId {
                shard_id: "01".to_owned(),
            },
            MapError::EmptyRange {
                shard_id: "01".to_owned(),
            },
            MapError::TableUnmapped {
                table: "accounts".to_owned(),
            },
            MapError::TableSplitUnsupported {
                table: "accounts".to_owned(),
            },
            MapError::InvalidTable {
                name: "Accounts".to_owned(),
            },
            MapError::MalformedCanonical {
                offset: 0,
                reason: "bad magic",
            },
        ];
        for error in errors {
            assert_ne!(error.code(), "");
        }
    }

    #[test]
    fn the_map_errors_use_the_map_namespace() {
        let errors = [
            MapError::UnknownVersion {
                found: "oxkv/map/v2".to_owned(),
            },
            MapError::Overlap {
                left: String::new(),
                right: String::new(),
            },
            MapError::Gap {
                shard_id: String::new(),
                table: String::new(),
            },
            MapError::DuplicateId {
                shard_id: String::new(),
            },
            MapError::EmptyRange {
                shard_id: String::new(),
            },
            MapError::TableUnmapped {
                table: String::new(),
            },
            MapError::TableSplitUnsupported {
                table: String::new(),
            },
        ];
        for error in errors {
            assert!(error.code().starts_with("map."));
        }
    }
}
