//! Half-open table bounds from key encoding v1.
//!
//! A table with prefix `P` owns `[P, successor(P))`. The prefix is
//! `0x01 || table-name || 0x00`. [`successor`] replaces the final `0x00` with
//! `0x01`.
//!
//! # Boundary
//!
//! The key encoding crate is not in this worktree. This module implements the
//! frozen successor rule from key encoding v1. A later crate can replace this
//! module. The rule, the bytes, and the order stay the same.

use oxkv_contracts::TableName;
use oxkv_contracts::keyspace::KEY_ENCODING_VERSION;
use oxkv_contracts::version::PrefixBounds;

/// Returns the table prefix `0x01 || table-name || 0x00`.
#[must_use]
pub fn table_prefix(table: &TableName) -> Vec<u8> {
    let name = table.as_str().as_bytes();
    let mut prefix = Vec::with_capacity(name.len() + 2);
    prefix.push(KEY_ENCODING_VERSION);
    prefix.extend_from_slice(name);
    prefix.push(0x00);
    prefix
}

/// Returns the exclusive upper bound for a table prefix.
///
/// The function replaces the final `0x00` with `0x01`. It returns `None` when
/// the prefix is empty or does not end with `0x00`.
#[must_use]
pub fn successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let (&last, rest) = prefix.split_last()?;
    if last != 0x00 {
        return None;
    }
    let mut next = Vec::with_capacity(rest.len() + 1);
    next.extend_from_slice(rest);
    next.push(0x01);
    Some(next)
}

/// Returns the half-open bounds for one table.
///
/// The lower bound is the table prefix. The upper bound is
/// `successor(prefix)`.
#[must_use]
pub fn table_bounds(table: &TableName) -> PrefixBounds {
    let lower = table_prefix(table);
    let upper = successor(&lower).unwrap_or_default();
    PrefixBounds::new(lower, upper)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str) -> TableName {
        TableName::parse_user(name).expect("the table name is valid")
    }

    #[test]
    fn the_successor_replaces_the_final_zero() {
        assert_eq!(
            successor(b"\x01accounts\x00"),
            Some(b"\x01accounts\x01".to_vec())
        );
        assert_eq!(successor(b"\x01\x00"), Some(b"\x01\x01".to_vec()));
        assert_eq!(successor(b""), None);
        assert_eq!(successor(b"\x01a\x01"), None);
    }

    #[test]
    fn a_table_prefix_has_the_key_encoding_version() {
        assert_eq!(table_prefix(&table("accounts")), b"\x01accounts\x00");
        assert_eq!(table_prefix(&TableName::catalog()), b"\x01_catalog\x00");
    }

    #[test]
    fn table_bounds_are_half_open() {
        let bounds = table_bounds(&table("accounts"));
        assert_eq!(bounds.lower(), b"\x01accounts\x00");
        assert_eq!(bounds.upper(), b"\x01accounts\x01");
        assert!(!bounds.is_empty());
        let key: &[u8] = b"\x01accounts\x00\x10a\x00\x00";
        assert!(bounds.lower() <= key);
        assert!(key < bounds.upper());
        assert!(bounds.upper() <= b"\x01accounts\x01");
    }

    #[test]
    fn the_catalog_table_has_canonical_bounds() {
        let bounds = table_bounds(&TableName::catalog());
        assert_eq!(bounds.lower(), b"\x01_catalog\x00");
        assert_eq!(bounds.upper(), b"\x01_catalog\x01");
    }

    #[test]
    fn table_ranges_do_not_overlap() {
        let left = table_bounds(&table("accounts"));
        let right = table_bounds(&table("ledger_entry"));
        assert!(left.upper() <= right.lower());
        assert!(right.upper() <= table_bounds(&table("z")).lower());
    }
}
