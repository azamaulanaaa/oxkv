//! The stable identity of one hook rule.

use core::fmt;

/// The stable identity of one hook rule.
///
/// The manifest declares the rule id. The pipeline uses the id for ordering,
/// duplicate detection, and error context. One pipeline rejects an empty id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleId(String);

impl RuleId {
    /// Creates a rule id.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// Returns the rule id text.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns true when the rule id is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<&str> for RuleId {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for RuleId {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}
