//! The static shard map and its validation rules.
//!
//! Table ranges are not adjacent. A gap between two table ranges is normal. A
//! [`MapError::Gap`] error means that one shard range does not cover the full
//! prefix of its own table. A [`MapError::TableSplitUnsupported`] error means
//! that two entries name one table.

use std::collections::BTreeSet;

use oxkv_contracts::TableName;
use oxkv_contracts::keyspace::{CATALOG_TABLE, KEY_ENCODING_VERSION};
use oxkv_contracts::version::{MapSchemaVersion, MapVersion, ShardId};

use crate::bounds::table_bounds;
use crate::entry::ShardEntry;
use crate::error::{MapError, hex};

/// A static map from every table to exactly one shard.
///
/// A map has one catalog shard and zero or more data shards. A data shard
/// contains one user table. A table never splits across shards. The map
/// validates coverage and disjointness when it is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardMap {
    map_version: MapVersion,
    entries: Vec<ShardEntry>,
}

impl ShardMap {
    /// The map schema version.
    pub const SCHEMA_VERSION: MapSchemaVersion = crate::MAP_SCHEMA_VERSION;

    /// Creates a map from shard entries.
    ///
    /// The constructor sorts the entries into canonical range order. It
    /// rejects an invalid map.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::EmptyRange`] for an empty range. Returns
    /// [`MapError::DuplicateId`] for a duplicate shard identifier. Returns
    /// [`MapError::TableSplitUnsupported`] for a table in two shards. Returns
    /// [`MapError::TableUnmapped`] when `_catalog` is absent. Returns
    /// [`MapError::Overlap`] or [`MapError::Gap`] for a range that does not
    /// match the table prefix.
    pub fn new(
        map_version: MapVersion,
        entries: impl IntoIterator<Item = ShardEntry>,
    ) -> Result<Self, MapError> {
        let mut entries: Vec<ShardEntry> = entries.into_iter().collect();
        validate_entries(&mut entries)?;
        Ok(Self {
            map_version,
            entries,
        })
    }

    /// Creates a map from table assignments.
    ///
    /// The function adds the catalog shard. Each remaining pair assigns one
    /// user table to one data shard. The bounds come from the table name.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::TableSplitUnsupported`] when an assignment names
    /// `_catalog`. Returns any error from [`ShardMap::new`].
    pub fn from_assignments(
        map_version: MapVersion,
        catalog_shard_id: ShardId,
        assignments: impl IntoIterator<Item = (ShardId, TableName)>,
    ) -> Result<Self, MapError> {
        let mut entries = vec![ShardEntry::catalog(catalog_shard_id)];
        for (shard_id, table) in assignments {
            if table.is_catalog() {
                return Err(MapError::TableSplitUnsupported {
                    table: table.to_string(),
                });
            }
            entries.push(ShardEntry::data(shard_id, table)?);
        }
        Self::new(map_version, entries)
    }

    /// Returns the map version.
    #[must_use]
    pub fn map_version(&self) -> MapVersion {
        self.map_version
    }

    /// Returns the entries in canonical range order.
    #[must_use]
    pub fn entries(&self) -> &[ShardEntry] {
        &self.entries
    }

    /// Returns the number of shard entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the map has no entries.
    ///
    /// A valid map always has a catalog entry, so this function returns false
    /// for every map that [`ShardMap::new`] accepts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the entry for one table.
    #[must_use]
    pub fn entry_for_table(&self, table: &TableName) -> Option<&ShardEntry> {
        self.entries.iter().find(|entry| entry.table() == table)
    }

    /// Returns the entry for one table.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::TableUnmapped`] when the map does not contain the
    /// table.
    pub fn shard_for_table(&self, table: &TableName) -> Result<&ShardEntry, MapError> {
        self.entry_for_table(table)
            .ok_or_else(|| MapError::TableUnmapped {
                table: table.to_string(),
            })
    }

    /// Returns the catalog shard entry.
    ///
    /// # Panics
    ///
    /// Panics when the map is invalid. [`ShardMap::new`] rejects a map with no
    /// catalog entry.
    #[must_use]
    pub fn catalog_entry(&self) -> &ShardEntry {
        self.entries
            .iter()
            .find(|entry| entry.is_catalog())
            .expect("a valid map has a catalog entry")
    }

    /// Checks that the map contains every declared table.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::TableUnmapped`] for the first missing table.
    pub fn validate_tables<'a>(
        &self,
        tables: impl IntoIterator<Item = &'a TableName>,
    ) -> Result<(), MapError> {
        for table in tables {
            if self.entry_for_table(table).is_none() {
                return Err(MapError::TableUnmapped {
                    table: table.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Returns the shard entries for a set of tables.
    ///
    /// The result has no duplicate shard. It is sorted by shard identifier.
    /// Use this order for deterministic scope fan-out and merge.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::TableUnmapped`] when a table is absent.
    pub fn shards_for_tables<'a>(
        &self,
        tables: impl IntoIterator<Item = &'a TableName>,
    ) -> Result<Vec<&ShardEntry>, MapError> {
        let mut entries: Vec<&ShardEntry> = Vec::new();
        for table in tables {
            let entry = self.shard_for_table(table)?;
            if !entries
                .iter()
                .any(|seen| seen.shard_id() == entry.shard_id())
            {
                entries.push(entry);
            }
        }
        entries.sort_by(|left, right| left.shard_id().cmp(right.shard_id()));
        Ok(entries)
    }

    /// Routes one encoded key to one shard entry.
    ///
    /// The function reads the key version and the table prefix only. It does
    /// not validate key components. The key encoding crate validates the
    /// components.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::UnknownKeyVersion`] when the key version is not
    /// v1. Returns [`MapError::MalformedKey`] when the key has no valid table
    /// prefix. Returns [`MapError::TableUnmapped`] when the table is not in
    /// the map. Returns [`MapError::Gap`] when the key is outside the shard
    /// range.
    pub fn route(&self, key: &[u8]) -> Result<&ShardEntry, MapError> {
        let table = key_table(key)?;
        let entry = self.shard_for_table(&table)?;
        if !entry.contains(key) {
            return Err(MapError::Gap {
                shard_id: hex(entry.shard_id().as_bytes()),
                table: table.to_string(),
            });
        }
        Ok(entry)
    }

    /// Routes one encoded key to one shard identifier.
    ///
    /// # Errors
    ///
    /// Returns the error from [`ShardMap::route`].
    pub fn route_shard(&self, key: &[u8]) -> Result<&ShardId, MapError> {
        Ok(self.route(key)?.shard_id())
    }
}

/// Reads the table name from one encoded key.
fn key_table(key: &[u8]) -> Result<TableName, MapError> {
    let Some((version, rest)) = key.split_first() else {
        return Err(MapError::UnknownKeyVersion { found: None });
    };
    if *version != KEY_ENCODING_VERSION {
        return Err(MapError::UnknownKeyVersion {
            found: Some(*version),
        });
    }
    let Some(delimiter) = rest.iter().position(|byte| *byte == 0x00) else {
        return Err(MapError::MalformedKey {
            reason: "the key has no table delimiter",
        });
    };
    let name = core::str::from_utf8(&rest[..delimiter]).map_err(|_| MapError::MalformedKey {
        reason: "the table name is not UTF-8",
    })?;
    TableName::parse(name).map_err(|_| MapError::MalformedKey {
        reason: "the table name is not valid",
    })
}

/// Validates and sorts the entries of one map.
fn validate_entries(entries: &mut [ShardEntry]) -> Result<(), MapError> {
    for entry in entries.iter() {
        if entry.bounds().is_empty() {
            return Err(MapError::EmptyRange {
                shard_id: hex(entry.shard_id().as_bytes()),
            });
        }
    }

    let mut shard_ids = BTreeSet::new();
    for entry in entries.iter() {
        if !shard_ids.insert(entry.shard_id()) {
            return Err(MapError::DuplicateId {
                shard_id: hex(entry.shard_id().as_bytes()),
            });
        }
    }

    let mut tables = BTreeSet::new();
    for entry in entries.iter() {
        if !tables.insert(entry.table()) {
            return Err(MapError::TableSplitUnsupported {
                table: entry.table().to_string(),
            });
        }
    }

    if !entries.iter().any(ShardEntry::is_catalog) {
        return Err(MapError::TableUnmapped {
            table: CATALOG_TABLE.to_owned(),
        });
    }

    for entry in entries.iter() {
        let expected = table_bounds(entry.table());
        let lower = entry.bounds().lower();
        let upper = entry.bounds().upper();
        if lower < expected.lower() || upper > expected.upper() {
            return Err(MapError::Overlap {
                left: range_text(entry),
                right: format!("{}..{}", hex(expected.lower()), hex(expected.upper())),
            });
        }
        if lower > expected.lower() || upper < expected.upper() {
            return Err(MapError::Gap {
                shard_id: hex(entry.shard_id().as_bytes()),
                table: entry.table().to_string(),
            });
        }
    }

    entries.sort_by(|left, right| {
        left.bounds()
            .lower()
            .cmp(right.bounds().lower())
            .then_with(|| left.bounds().upper().cmp(right.bounds().upper()))
            .then_with(|| left.shard_id().cmp(right.shard_id()))
    });

    for pair in entries.windows(2) {
        if pair[1].bounds().lower() < pair[0].bounds().upper() {
            return Err(MapError::Overlap {
                left: range_text(&pair[0]),
                right: range_text(&pair[1]),
            });
        }
    }

    Ok(())
}

/// Returns a display form for one shard range.
fn range_text(entry: &ShardEntry) -> String {
    format!(
        "{}..{}",
        hex(entry.bounds().lower()),
        hex(entry.bounds().upper())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::version::PrefixBounds;

    fn table(name: &str) -> TableName {
        TableName::parse_user(name).expect("the table name is valid")
    }

    fn valid_map() -> ShardMap {
        ShardMap::from_assignments(
            MapVersion::new(1),
            ShardId::new(vec![0]),
            [
                (ShardId::new(vec![1]), table("accounts")),
                (ShardId::new(vec![2]), table("ledger_entry")),
            ],
        )
        .expect("the map is valid")
    }

    #[test]
    fn a_valid_map_has_the_catalog_and_each_table() {
        let map = valid_map();
        assert_eq!(map.len(), 3);
        assert!(!map.is_empty());
        assert!(map.catalog_entry().is_catalog());
        assert!(map.entry_for_table(&table("accounts")).is_some());
        assert!(map.entry_for_table(&table("ledger_entry")).is_some());
        assert!(map.entry_for_table(&table("missing")).is_none());
    }

    #[test]
    fn the_entries_use_canonical_range_order() {
        let map = valid_map();
        let lowers: Vec<&[u8]> = map
            .entries()
            .iter()
            .map(|entry| entry.bounds().lower())
            .collect();
        let mut sorted = lowers.clone();
        sorted.sort_unstable();
        assert_eq!(lowers, sorted);
    }

    #[test]
    fn a_split_table_fails() {
        let entries = [
            ShardEntry::data(ShardId::new(vec![1]), table("accounts")).expect("the entry is valid"),
            ShardEntry::data(ShardId::new(vec![2]), table("accounts")).expect("the entry is valid"),
            ShardEntry::catalog(ShardId::new(vec![0])),
        ];
        assert_eq!(
            ShardMap::new(MapVersion::new(1), entries)
                .unwrap_err()
                .code(),
            "map.table_split_unsupported"
        );
    }

    #[test]
    fn an_overlapping_range_fails() {
        let bounds = table_bounds(&table("accounts"));
        let mut upper = bounds.upper().to_vec();
        upper.push(0x00);
        let overlapping = PrefixBounds::new(bounds.lower().to_vec(), upper);
        let entries = [
            ShardEntry::from_parts(ShardId::new(vec![1]), table("accounts"), overlapping),
            ShardEntry::catalog(ShardId::new(vec![0])),
        ];
        assert_eq!(
            ShardMap::new(MapVersion::new(1), entries)
                .unwrap_err()
                .code(),
            "map.overlap"
        );
    }

    #[test]
    fn a_gap_in_one_table_range_fails() {
        let bounds = table_bounds(&table("accounts"));
        let mut upper = bounds.lower().to_vec();
        upper.extend_from_slice(b"\x10a");
        let gap = PrefixBounds::new(bounds.lower().to_vec(), upper);
        let entries = [
            ShardEntry::from_parts(ShardId::new(vec![1]), table("accounts"), gap),
            ShardEntry::catalog(ShardId::new(vec![0])),
        ];
        assert_eq!(
            ShardMap::new(MapVersion::new(1), entries)
                .unwrap_err()
                .code(),
            "map.gap"
        );
    }

    #[test]
    fn a_duplicate_shard_id_fails() {
        let entries = [
            ShardEntry::data(ShardId::new(vec![1]), table("accounts")).expect("the entry is valid"),
            ShardEntry::data(ShardId::new(vec![1]), table("ledger_entry"))
                .expect("the entry is valid"),
            ShardEntry::catalog(ShardId::new(vec![0])),
        ];
        assert_eq!(
            ShardMap::new(MapVersion::new(1), entries)
                .unwrap_err()
                .code(),
            "map.duplicate_id"
        );
    }

    #[test]
    fn an_unmapped_table_fails() {
        let map = valid_map();
        assert_eq!(
            map.validate_tables([&table("accounts"), &table("missing")])
                .unwrap_err()
                .code(),
            "map.table_unmapped"
        );
        assert_eq!(
            map.shard_for_table(&table("missing")).unwrap_err().code(),
            "map.table_unmapped"
        );
    }

    #[test]
    fn an_empty_range_fails() {
        let bounds = PrefixBounds::new(b"\x01accounts\x01".to_vec(), b"\x01accounts\x00".to_vec());
        let entries = [
            ShardEntry::from_parts(ShardId::new(vec![1]), table("accounts"), bounds),
            ShardEntry::catalog(ShardId::new(vec![0])),
        ];
        assert_eq!(
            ShardMap::new(MapVersion::new(1), entries)
                .unwrap_err()
                .code(),
            "map.empty_range"
        );
    }

    #[test]
    fn a_map_without_a_catalog_fails() {
        let entries =
            [ShardEntry::data(ShardId::new(vec![1]), table("accounts"))
                .expect("the entry is valid")];
        assert_eq!(
            ShardMap::new(MapVersion::new(1), entries)
                .unwrap_err()
                .code(),
            "map.table_unmapped"
        );
    }

    #[test]
    fn the_map_routes_every_valid_key_to_one_shard() {
        let map = valid_map();
        let component_suffixes: [&[u8]; 7] = [
            b"\x10a\x00\x00",
            b"\x10\x00\xff\x00\x00",
            b"\x11\x00\xff\x00\x00",
            b"\x20\x00\x00\x00\x00\x00\x00\x00\x01",
            b"\x21\x80\x00\x00\x00\x00\x00\x00\x00",
            b"\x30\x01",
            b"\xff\x01\x00",
        ];
        for entry in map.entries() {
            let lower = entry.bounds().lower();
            for suffix in component_suffixes {
                let mut key = lower.to_vec();
                key.extend_from_slice(suffix);
                let routed = map.route(&key).expect("the key is mapped");
                assert_eq!(routed.shard_id(), entry.shard_id());
                assert_eq!(routed.table(), entry.table());
            }
        }
    }

    #[test]
    fn the_map_routes_the_same_key_to_the_same_shard() {
        let map = valid_map();
        let mut key = table_bounds(&table("ledger_entry")).lower().to_vec();
        key.extend_from_slice(b"\x10x\x00\x00");
        let first = map.route_shard(&key).expect("the key is mapped");
        for _ in 0..8 {
            assert_eq!(map.route_shard(&key).expect("the key is mapped"), first);
        }
        assert_eq!(first, &ShardId::new(vec![2]));
    }

    #[test]
    fn an_unknown_key_version_fails() {
        let map = valid_map();
        assert_eq!(
            map.route(&[0x02, b'a', 0x00]).unwrap_err().code(),
            "key.unknown_version"
        );
        assert_eq!(map.route(&[]).unwrap_err().code(), "key.unknown_version");
    }

    #[test]
    fn a_key_without_a_table_delimiter_fails() {
        let map = valid_map();
        assert_eq!(
            map.route(&[0x01, b'a']).unwrap_err().code(),
            "key.malformed"
        );
        assert_eq!(
            map.route(&[0x01, b'A', 0x00]).unwrap_err().code(),
            "key.malformed"
        );
    }

    #[test]
    fn a_key_for_an_unmapped_table_fails() {
        let map = valid_map();
        assert_eq!(
            map.route(&[0x01, b'm', b'i', b's', b's', 0x00])
                .unwrap_err()
                .code(),
            "map.table_unmapped"
        );
    }

    #[test]
    fn a_scope_shard_set_is_sorted_and_unique() {
        let map = valid_map();
        let entries = map
            .shards_for_tables([
                &table("ledger_entry"),
                &table("accounts"),
                &table("accounts"),
            ])
            .expect("every table is mapped");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].shard_id() < entries[1].shard_id());
    }
}
