//! Shard entries with stable identifiers and derived table bounds.

use oxkv_contracts::TableName;
use oxkv_contracts::version::{PrefixBounds, ShardId};

use crate::bounds::table_bounds;
use crate::error::MapError;

/// The role of one shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ShardRole {
    /// A data shard. It contains exactly one user table.
    Data,
    /// The catalog shard. It contains the `_catalog` table.
    Catalog,
}

impl ShardRole {
    /// Returns the canonical role text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Catalog => "catalog",
        }
    }

    /// Returns the canonical wire tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Data => 0x00,
            Self::Catalog => 0x01,
        }
    }

    /// Reads a canonical wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            0x00 => Some(Self::Data),
            0x01 => Some(Self::Catalog),
            _ => None,
        }
    }
}

/// One shard entry.
///
/// A data shard entry has one user table. The catalog shard entry has the
/// `_catalog` table. The bounds are half-open.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShardEntry {
    shard_id: ShardId,
    table: TableName,
    bounds: PrefixBounds,
    role: ShardRole,
}

impl ShardEntry {
    /// Creates a data shard entry with derived bounds.
    ///
    /// # Errors
    ///
    /// Returns [`MapError::TableSplitUnsupported`] when the table is
    /// `_catalog`. Use [`ShardEntry::catalog`] for the catalog shard.
    pub fn data(shard_id: ShardId, table: TableName) -> Result<Self, MapError> {
        if table.is_catalog() {
            return Err(MapError::TableSplitUnsupported {
                table: table.to_string(),
            });
        }
        let bounds = table_bounds(&table);
        Ok(Self {
            shard_id,
            table,
            bounds,
            role: ShardRole::Data,
        })
    }

    /// Creates the catalog shard entry with derived bounds.
    #[must_use]
    pub fn catalog(shard_id: ShardId) -> Self {
        let table = TableName::catalog();
        let bounds = table_bounds(&table);
        Self {
            shard_id,
            table,
            bounds,
            role: ShardRole::Catalog,
        }
    }

    /// Creates an entry from declared parts.
    ///
    /// [`crate::ShardMap::new`] validates the bounds. An entry alone does not
    /// prove coverage.
    #[must_use]
    pub fn from_parts(shard_id: ShardId, table: TableName, bounds: PrefixBounds) -> Self {
        let role = if table.is_catalog() {
            ShardRole::Catalog
        } else {
            ShardRole::Data
        };
        Self {
            shard_id,
            table,
            bounds,
            role,
        }
    }

    /// Returns the stable opaque shard identifier.
    #[must_use]
    pub fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the table that the shard contains.
    #[must_use]
    pub fn table(&self) -> &TableName {
        &self.table
    }

    /// Returns the half-open prefix bounds.
    #[must_use]
    pub fn bounds(&self) -> &PrefixBounds {
        &self.bounds
    }

    /// Returns the shard role.
    #[must_use]
    pub const fn role(&self) -> ShardRole {
        self.role
    }

    /// Returns true when the entry is the catalog shard.
    #[must_use]
    pub fn is_catalog(&self) -> bool {
        self.role == ShardRole::Catalog
    }

    /// Returns true when the key is inside the half-open shard range.
    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        self.bounds.lower() <= key && key < self.bounds.upper()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str) -> TableName {
        TableName::parse_user(name).expect("the table name is valid")
    }

    #[test]
    fn a_data_entry_derives_its_bounds() {
        let entry =
            ShardEntry::data(ShardId::new(vec![1]), table("accounts")).expect("the entry is valid");
        assert_eq!(entry.bounds().lower(), b"\x01accounts\x00");
        assert_eq!(entry.bounds().upper(), b"\x01accounts\x01");
        assert_eq!(entry.role(), ShardRole::Data);
        assert!(!entry.is_catalog());
        assert!(entry.contains(b"\x01accounts\x00\x10a\x00\x00"));
    }

    #[test]
    fn a_catalog_entry_uses_the_catalog_table() {
        let entry = ShardEntry::catalog(ShardId::new(vec![0]));
        assert_eq!(entry.table().as_str(), "_catalog");
        assert_eq!(entry.bounds().lower(), b"\x01_catalog\x00");
        assert_eq!(entry.bounds().upper(), b"\x01_catalog\x01");
        assert_eq!(entry.role(), ShardRole::Catalog);
        assert!(entry.is_catalog());
    }

    #[test]
    fn a_data_entry_rejects_the_catalog_table() {
        assert_eq!(
            ShardEntry::data(ShardId::new(vec![1]), TableName::catalog()),
            Err(MapError::TableSplitUnsupported {
                table: "_catalog".to_owned()
            })
        );
    }

    #[test]
    fn the_role_wire_tags_round_trip() {
        for role in [ShardRole::Data, ShardRole::Catalog] {
            assert_eq!(ShardRole::from_wire_tag(role.wire_tag()), Some(role));
        }
        assert_eq!(ShardRole::from_wire_tag(0x02), None);
    }
}
