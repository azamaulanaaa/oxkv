//! Size limits for keys, values, batches, and pages.

/// The size limits of one engine.
///
/// The limits are immutable after engine construction. A batch over a limit
/// fails with a named error and changes no state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest key length in bytes.
    pub key_bytes: usize,
    /// The largest value length in bytes.
    pub value_bytes: usize,
    /// The largest operation count in one batch.
    pub batch_operations: usize,
    /// The largest encoded page length in bytes.
    pub page_bytes: usize,
}

impl Limits {
    /// The default key budget in bytes.
    pub const DEFAULT_KEY_BYTES: usize = 1024;
    /// The default value budget in bytes.
    pub const DEFAULT_VALUE_BYTES: usize = 1024 * 1024;
    /// The default batch budget in operations.
    pub const DEFAULT_BATCH_OPERATIONS: usize = 1024;
    /// The default page budget in bytes.
    pub const DEFAULT_PAGE_BYTES: usize = 1024 * 1024;

    /// Creates a limit set.
    #[must_use]
    pub const fn new(
        key_bytes: usize,
        value_bytes: usize,
        batch_operations: usize,
        page_bytes: usize,
    ) -> Self {
        Self {
            key_bytes,
            value_bytes,
            batch_operations,
            page_bytes,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_KEY_BYTES,
            Self::DEFAULT_VALUE_BYTES,
            Self::DEFAULT_BATCH_OPERATIONS,
            Self::DEFAULT_PAGE_BYTES,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_limits_match_the_registry_names() {
        let limits = Limits::default();
        assert_eq!(limits.key_bytes, 1024);
        assert_eq!(limits.value_bytes, 1024 * 1024);
        assert_eq!(limits.batch_operations, 1024);
        assert_eq!(limits.page_bytes, 1024 * 1024);
    }
}
