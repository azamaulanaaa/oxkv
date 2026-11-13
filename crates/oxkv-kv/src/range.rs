//! Inclusive key ranges over unsigned bytes.

use crate::error::KvError;

/// An inclusive key range.
///
/// A `None` bound is unbounded. A `Some` bound is inclusive. The range uses
/// lexicographic order over unsigned bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRange {
    lower: Option<Vec<u8>>,
    upper: Option<Vec<u8>>,
}

impl KeyRange {
    /// Creates a range with two inclusive bounds.
    ///
    /// # Errors
    ///
    /// Returns [`KvError::MalformedRange`] when the lower bound is greater
    /// than the upper bound.
    pub fn new(lower: Vec<u8>, upper: Vec<u8>) -> Result<Self, KvError> {
        Self::from_options(Some(lower), Some(upper))
    }

    /// Creates a range from optional inclusive bounds.
    ///
    /// # Errors
    ///
    /// Returns [`KvError::MalformedRange`] when both bounds are present and
    /// the lower bound is greater than the upper bound.
    pub fn from_options(lower: Option<Vec<u8>>, upper: Option<Vec<u8>>) -> Result<Self, KvError> {
        if let (Some(lower), Some(upper)) = (&lower, &upper)
            && lower > upper
        {
            return Err(KvError::MalformedRange);
        }
        Ok(Self { lower, upper })
    }

    /// Creates a range with no bounds.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            lower: None,
            upper: None,
        }
    }

    /// Returns the inclusive lower bound.
    #[must_use]
    pub fn lower(&self) -> Option<&[u8]> {
        self.lower.as_deref()
    }

    /// Returns the inclusive upper bound.
    #[must_use]
    pub fn upper(&self) -> Option<&[u8]> {
        self.upper.as_deref()
    }

    /// Returns true when the key is inside the range.
    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        let above_lower = self.lower.as_ref().is_none_or(|lower| key >= lower);
        let below_upper = self.upper.as_ref().is_none_or(|upper| key <= upper);
        above_lower && below_upper
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_bounds_are_inclusive() {
        let range = KeyRange::new(vec![b'b'], vec![b'd']).expect("the range is valid");
        assert!(!range.contains(b"a"));
        assert!(range.contains(b"b"));
        assert!(range.contains(b"c"));
        assert!(range.contains(b"d"));
        assert!(!range.contains(b"e"));
    }

    #[test]
    fn an_inverted_range_fails() {
        assert_eq!(
            KeyRange::new(vec![b'd'], vec![b'b']),
            Err(KvError::MalformedRange)
        );
        assert_eq!(
            KeyRange::new(vec![b'd'], vec![b'b'])
                .expect_err("the range is invalid")
                .code(),
            "kv.malformed_range"
        );
    }

    #[test]
    fn an_unbounded_range_matches_every_key() {
        let range = KeyRange::all();
        assert!(range.contains(b""));
        assert!(range.contains(&[0xFF]));
    }
}
