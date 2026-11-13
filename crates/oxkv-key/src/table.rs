//! Table names and table prefixes.
//!
//! A user table name matches `[a-z][a-z0-9_]*`. A user table name does not
//! start with `_`. The catalog table `_catalog` is the only system table name
//! in 1.0.

use core::fmt;

use oxkv_contracts::keyspace::{CATALOG_TABLE, KEY_ENCODING_VERSION, TableName};

use crate::{KeyError, MAX_TABLE_NAME_BYTES};

/// A table name and its derived key prefix.
///
/// Use [`Table::parse`] for a user table or the catalog table. Use
/// [`Table::parse_user`] for a user table only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Table {
    name: TableName,
}

impl Table {
    /// Parses a table name.
    ///
    /// The only valid system table name is `_catalog`.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::Malformed`] when the name is empty, longer than 63
    /// bytes, or does not match the grammar.
    pub fn parse(name: &str) -> Result<Self, KeyError> {
        check_name_len(name)?;
        let name = TableName::parse(name).map_err(|_| KeyError::Malformed { offset: 0 })?;
        Ok(Self { name })
    }

    /// Parses a user table name.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::Malformed`] when the name is empty, longer than 63
    /// bytes, starts with `_`, or does not match the grammar.
    pub fn parse_user(name: &str) -> Result<Self, KeyError> {
        check_name_len(name)?;
        let name = TableName::parse_user(name).map_err(|_| KeyError::Malformed { offset: 0 })?;
        Ok(Self { name })
    }

    /// Creates the catalog table name.
    #[must_use]
    pub fn catalog() -> Self {
        Self {
            name: TableName::catalog(),
        }
    }

    /// Returns the table name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.name.as_str()
    }

    /// Returns true when the table is the catalog table.
    #[must_use]
    pub fn is_catalog(&self) -> bool {
        self.name.is_catalog()
    }

    /// Returns the table key prefix `0x01 || table-name || 0x00`.
    #[must_use]
    pub fn prefix(&self) -> Vec<u8> {
        let mut prefix = Vec::with_capacity(1 + self.as_str().len() + 1);
        prefix.push(KEY_ENCODING_VERSION);
        prefix.extend_from_slice(self.as_str().as_bytes());
        prefix.push(0x00);
        prefix
    }
}

impl fmt::Display for Table {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn check_name_len(name: &str) -> Result<(), KeyError> {
    if name.is_empty() || name.len() > MAX_TABLE_NAME_BYTES {
        return Err(KeyError::Malformed { offset: 0 });
    }
    Ok(())
}

/// Returns the only top-level system table name.
#[must_use]
pub const fn catalog_table_name() -> &'static str {
    CATALOG_TABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_table_name_uses_the_grammar() {
        assert!(Table::parse_user("accounts").is_ok());
        assert!(Table::parse_user("ledger_entry_2").is_ok());
        assert!(Table::parse_user("a").is_ok());
        assert!(Table::parse_user("").is_err());
        assert!(Table::parse_user("Accounts").is_err());
        assert!(Table::parse_user("1accounts").is_err());
        assert!(Table::parse_user("account-name").is_err());
    }

    #[test]
    fn a_user_table_name_does_not_start_with_underscore() {
        assert!(Table::parse_user("_catalog").is_err());
        assert!(Table::parse_user("_receipt").is_err());
        assert!(Table::parse("_catalog").is_ok());
        assert!(Table::parse("_receipt").is_err());
    }

    #[test]
    fn a_table_name_is_at_most_63_bytes() {
        let longest = "a".repeat(63);
        assert!(Table::parse_user(&longest).is_ok());
        let too_long = "a".repeat(64);
        assert_eq!(
            Table::parse_user(&too_long),
            Err(KeyError::Malformed { offset: 0 })
        );
    }

    #[test]
    fn the_catalog_table_is_the_system_table() {
        let table = Table::catalog();
        assert!(table.is_catalog());
        assert_eq!(table.as_str(), CATALOG_TABLE);
        assert_eq!(table.prefix(), b"\x01_catalog\x00");
    }

    #[test]
    fn a_table_prefix_has_the_version_and_delimiter() {
        let table = Table::parse_user("accounts").expect("the table name is valid");
        assert_eq!(table.prefix(), b"\x01accounts\x00");
    }
}
