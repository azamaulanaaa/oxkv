//! Canonical shard map bytes and the BLAKE3 map digest.
//!
//! The canonical form is versioned. It starts with the byte string
//! `OXKVMAP`, one canonical format version, the map schema text, the map
//! version, and the entry count. Each entry lists its shard identifier, role,
//! table name, and half-open bounds. All integers are big-endian.
//!
//! The map digest is the raw BLAKE3 hash of the canonical map bytes.

use oxkv_contracts::TableName;
use oxkv_contracts::version::{MapDigest, MapVersion, PrefixBounds, ShardId};

use crate::MAP_SCHEMA;
use crate::entry::{ShardEntry, ShardRole};
use crate::error::MapError;
use crate::map::ShardMap;

/// The canonical map magic.
pub const MAP_MAGIC: &[u8; 7] = b"OXKVMAP";

/// The canonical map format version.
pub const CANONICAL_FORMAT_VERSION: u16 = 1;

/// The minimum encoded size of one entry.
const MIN_ENTRY_BYTES: usize = 17;

impl ShardMap {
    /// Returns the canonical byte form of the map.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAP_MAGIC);
        write_u16(&mut bytes, CANONICAL_FORMAT_VERSION);
        write_bytes(&mut bytes, MAP_SCHEMA.as_bytes());
        write_u64(&mut bytes, self.map_version().get());
        write_u32(&mut bytes, count_u32(self.entries().len()));
        for entry in self.entries() {
            write_bytes(&mut bytes, entry.shard_id().as_bytes());
            bytes.push(entry.role().wire_tag());
            write_bytes(&mut bytes, entry.table().as_str().as_bytes());
            write_bytes(&mut bytes, entry.bounds().lower());
            write_bytes(&mut bytes, entry.bounds().upper());
        }
        bytes
    }

    /// Returns the BLAKE3 digest of the canonical map bytes.
    #[must_use]
    pub fn digest(&self) -> MapDigest {
        MapDigest::from_bytes(*blake3::hash(&self.canonical_bytes()).as_bytes())
    }

    /// Decodes canonical map bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::UnknownVersion`] when the schema or canonical
    /// format is unknown. Returns [`MapError::MalformedCanonical`] when the
    /// bytes are not a complete canonical map. Returns every validation error
    /// from [`ShardMap::new`].
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, MapError> {
        decode(bytes)
    }
}

/// Decodes one map.
fn decode(bytes: &[u8]) -> Result<ShardMap, MapError> {
    let mut reader = Reader::new(bytes);
    let magic = reader.read_exact(MAP_MAGIC.len())?;
    if magic != MAP_MAGIC {
        return Err(MapError::MalformedCanonical {
            offset: 0,
            reason: "the map magic is wrong",
        });
    }

    let format_version = reader.read_u16()?;
    if format_version != CANONICAL_FORMAT_VERSION {
        return Err(MapError::UnknownVersion {
            found: format!("map-format/v{format_version}"),
        });
    }

    let schema_bytes = reader.read_sized()?;
    let schema = core::str::from_utf8(schema_bytes).map_err(|_| MapError::MalformedCanonical {
        offset: reader.offset(),
        reason: "the map schema is not UTF-8",
    })?;
    if schema != MAP_SCHEMA {
        return Err(MapError::UnknownVersion {
            found: schema.to_owned(),
        });
    }

    let map_version = MapVersion::new(reader.read_u64()?);
    let entry_count = reader.read_u32()?;
    let entry_count = usize::try_from(entry_count).unwrap_or(usize::MAX);
    if entry_count > reader.remaining() / MIN_ENTRY_BYTES {
        return Err(MapError::MalformedCanonical {
            offset: reader.offset(),
            reason: "the entry count is too large",
        });
    }

    let mut entries = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        let offset = reader.offset();
        let shard_id = ShardId::new(reader.read_sized()?.to_vec());
        let role_tag = reader.read_u8()?;
        let role = ShardRole::from_wire_tag(role_tag).ok_or(MapError::MalformedCanonical {
            offset,
            reason: "the shard role is unknown",
        })?;
        let table_name = reader.read_sized()?;
        let table_name =
            core::str::from_utf8(table_name).map_err(|_| MapError::MalformedCanonical {
                offset,
                reason: "the table name is not UTF-8",
            })?;
        let table = TableName::parse(table_name).map_err(|_| MapError::InvalidTable {
            name: table_name.to_owned(),
        })?;
        let expected_role = if table.is_catalog() {
            ShardRole::Catalog
        } else {
            ShardRole::Data
        };
        if role != expected_role {
            return Err(MapError::MalformedCanonical {
                offset,
                reason: "the shard role does not match the table",
            });
        }
        let lower = reader.read_sized()?.to_vec();
        let upper = reader.read_sized()?.to_vec();
        entries.push(ShardEntry::from_parts(
            shard_id,
            table,
            PrefixBounds::new(lower, upper),
        ));
    }

    if reader.remaining() != 0 {
        return Err(MapError::MalformedCanonical {
            offset: reader.offset(),
            reason: "the map has trailing bytes",
        });
    }

    ShardMap::new(map_version, entries)
}

/// A bounds-checked byte reader.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    /// Creates a reader over one byte slice.
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Returns the current offset.
    const fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the number of unread bytes.
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    /// Reads one byte.
    fn read_u8(&mut self) -> Result<u8, MapError> {
        let bytes = self.read_exact(1)?;
        Ok(bytes[0])
    }

    /// Reads one big-endian `u16`.
    fn read_u16(&mut self) -> Result<u16, MapError> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    /// Reads one big-endian `u32`.
    fn read_u32(&mut self) -> Result<u32, MapError> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Reads one big-endian `u64`.
    fn read_u64(&mut self) -> Result<u64, MapError> {
        let bytes = self.read_exact(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Reads one length-prefixed byte string.
    fn read_sized(&mut self) -> Result<&'a [u8], MapError> {
        let length = self.read_u32()?;
        let length = usize::try_from(length).unwrap_or(usize::MAX);
        self.read_exact(length)
    }

    /// Reads exactly `length` bytes.
    fn read_exact(&mut self, length: usize) -> Result<&'a [u8], MapError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(MapError::MalformedCanonical {
                offset: self.offset,
                reason: "the length overflows",
            })?;
        if end > self.bytes.len() {
            return Err(MapError::MalformedCanonical {
                offset: self.offset,
                reason: "the bytes are truncated",
            });
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }
}

/// Writes one big-endian `u16`.
fn write_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

/// Writes one big-endian `u32`.
fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

/// Writes one big-endian `u64`.
fn write_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

/// Writes one length-prefixed byte string.
fn write_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    write_u32(bytes, count_u32(value.len()));
    bytes.extend_from_slice(value);
}

/// Converts a length to `u32`.
fn count_u32(value: usize) -> u32 {
    u32::try_from(value).expect("the map field is smaller than 4 GiB")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str) -> TableName {
        TableName::parse_user(name).expect("the table name is valid")
    }

    fn map() -> ShardMap {
        ShardMap::from_assignments(
            MapVersion::new(7),
            ShardId::new(vec![0]),
            [
                (ShardId::new(vec![1]), table("accounts")),
                (ShardId::new(vec![2]), table("ledger_entry")),
            ],
        )
        .expect("the map is valid")
    }

    #[test]
    fn canonical_bytes_round_trip() {
        let map = map();
        let bytes = map.canonical_bytes();
        let decoded = ShardMap::from_canonical_bytes(&bytes).expect("the map decodes");
        assert_eq!(decoded, map);
        assert_eq!(decoded.canonical_bytes(), bytes);
    }

    #[test]
    fn the_digest_is_the_raw_blake3_hash() {
        let map = map();
        assert_eq!(
            map.digest().as_bytes(),
            blake3::hash(&map.canonical_bytes()).as_bytes()
        );
    }

    #[test]
    fn the_digest_does_not_depend_on_the_entry_order() {
        let entries = [
            ShardEntry::catalog(ShardId::new(vec![0])),
            ShardEntry::data(ShardId::new(vec![1]), table("accounts")).expect("the entry is valid"),
            ShardEntry::data(ShardId::new(vec![2]), table("ledger_entry"))
                .expect("the entry is valid"),
        ];
        let forward = ShardMap::new(MapVersion::new(7), entries.clone()).expect("the map is valid");
        let reversed =
            ShardMap::new(MapVersion::new(7), entries.into_iter().rev()).expect("the map is valid");
        assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
        assert_eq!(forward.digest(), reversed.digest());
    }

    #[test]
    fn a_map_version_change_changes_the_digest() {
        let first = map();
        let second = ShardMap::from_assignments(
            MapVersion::new(8),
            ShardId::new(vec![0]),
            [
                (ShardId::new(vec![1]), table("accounts")),
                (ShardId::new(vec![2]), table("ledger_entry")),
            ],
        )
        .expect("the map is valid");
        assert_ne!(first.digest(), second.digest());
    }

    #[test]
    fn an_unknown_schema_fails() {
        let map = map();
        let mut bytes = map.canonical_bytes();
        let offset = MAP_MAGIC.len() + 2 + 4 + MAP_SCHEMA.len() - 1;
        bytes[offset] = b'2';
        let error = ShardMap::from_canonical_bytes(&bytes).unwrap_err();
        assert_eq!(error.code(), "map.unknown_version");
        assert!(matches!(error, MapError::UnknownVersion { .. }));
    }

    #[test]
    fn an_unknown_format_fails() {
        let map = map();
        let mut bytes = map.canonical_bytes();
        bytes[MAP_MAGIC.len()..MAP_MAGIC.len() + 2].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(
            ShardMap::from_canonical_bytes(&bytes).unwrap_err().code(),
            "map.unknown_version"
        );
    }

    #[test]
    fn trailing_bytes_fail() {
        let map = map();
        let mut bytes = map.canonical_bytes();
        bytes.push(0x00);
        assert_eq!(
            ShardMap::from_canonical_bytes(&bytes).unwrap_err().code(),
            "map.malformed_canonical"
        );
    }

    #[test]
    fn truncated_bytes_fail() {
        let map = map();
        let bytes = map.canonical_bytes();
        assert_eq!(
            ShardMap::from_canonical_bytes(&bytes[..bytes.len() - 1])
                .unwrap_err()
                .code(),
            "map.malformed_canonical"
        );
    }

    #[test]
    fn a_wrong_magic_fails() {
        assert_eq!(
            ShardMap::from_canonical_bytes(b"not a map")
                .unwrap_err()
                .code(),
            "map.malformed_canonical"
        );
    }
}
