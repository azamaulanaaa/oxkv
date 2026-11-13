//! Half-open table bounds and resume helpers.
//!
//! A table with prefix `P` owns the interval `[P, successor(P))`. The
//! `successor` function replaces the final `0x00` with `0x01`.

use oxkv_contracts::version::PrefixBounds;

use crate::table::Table;

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

/// Returns the smallest byte string that is strictly greater than `last`.
///
/// Use the result as an inclusive lower bound. It resumes a scan after the
/// last key with no duplicate and no skip. The result may not be a valid key
/// for the table.
#[must_use]
pub fn after_key(last: &[u8]) -> Vec<u8> {
    let mut next = Vec::with_capacity(last.len() + 1);
    next.extend_from_slice(last);
    next.push(0x00);
    next
}

/// The half-open byte bounds of one table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableBounds {
    lower: Vec<u8>,
    upper: Vec<u8>,
}

impl TableBounds {
    /// Creates the bounds for one table.
    #[must_use]
    pub fn new(table: &Table) -> Self {
        let lower = table.prefix();
        let upper = successor(&lower).unwrap_or_default();
        Self { lower, upper }
    }

    /// Returns the inclusive lower bound `P`.
    #[must_use]
    pub fn lower(&self) -> &[u8] {
        &self.lower
    }

    /// Returns the exclusive upper bound `successor(P)`.
    #[must_use]
    pub fn upper(&self) -> &[u8] {
        &self.upper
    }

    /// Returns true when the key is in `[P, successor(P))`.
    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        self.lower.as_slice() <= key && key < self.upper.as_slice()
    }

    /// Returns the resume lower bound for a last key.
    ///
    /// The function returns `None` when the resume bound leaves the table.
    #[must_use]
    pub fn resume_after(&self, last: &[u8]) -> Option<Vec<u8>> {
        let lower = after_key(last);
        if lower >= self.upper {
            None
        } else {
            Some(lower)
        }
    }

    /// Returns the shared prefix bounds type.
    #[must_use]
    pub fn as_prefix_bounds(&self) -> PrefixBounds {
        PrefixBounds::new(self.lower.clone(), self.upper.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str) -> Table {
        Table::parse_user(name).expect("the table name is valid")
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
    fn the_after_key_is_strictly_greater() {
        for last in [b"".as_slice(), b"a", b"\xff\xff"] {
            let next = after_key(last);
            assert!(next.as_slice() > last);
            assert_eq!(next, [last, b"\x00"].concat());
        }
    }

    #[test]
    fn the_bounds_are_half_open() {
        let bounds = TableBounds::new(&table("accounts"));
        assert_eq!(bounds.lower(), b"\x01accounts\x00");
        assert_eq!(bounds.upper(), b"\x01accounts\x01");
        assert!(bounds.contains(b"\x01accounts\x00"));
        assert!(bounds.contains(b"\x01accounts\x00\x10a\x00\x00"));
        assert!(!bounds.contains(b"\x01accounts\x01"));
        assert!(!bounds.contains(b"\x01accounts"));
    }

    #[test]
    fn a_resume_bound_does_not_leave_the_table() {
        let bounds = TableBounds::new(&table("accounts"));
        let last = b"\x01accounts\x00\x10a\x00\x00";
        assert_eq!(
            bounds.resume_after(last),
            Some(b"\x01accounts\x00\x10a\x00\x00\x00".to_vec())
        );
        assert_eq!(bounds.resume_after(b"\x01accounts\x01"), None);
    }

    #[test]
    fn the_prefix_bounds_type_is_shared() {
        let bounds = TableBounds::new(&table("accounts"));
        let shared = bounds.as_prefix_bounds();
        assert_eq!(shared.lower(), bounds.lower());
        assert_eq!(shared.upper(), bounds.upper());
        assert!(!shared.is_empty());
    }
}
