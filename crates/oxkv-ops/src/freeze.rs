//! Support window and downgrade policy.
//!
//! A 1.x reader accepts the current format and one previous minor. A downgrade
//! is refused after the first new-format write. The rules are pure functions.

use oxkv_contracts::version::SnapshotPageVersion;

use crate::error::OpsError;

/// The number of previous minors that a reader accepts.
pub const READER_WINDOW: u16 = 1;

/// The support policy of one release line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SupportPolicy {
    /// The current major version.
    pub current_major: u16,
    /// The current minor version.
    pub current_minor: u16,
    /// The number of previous minors that the reader accepts.
    pub reader_back: u16,
}

impl SupportPolicy {
    /// Creates a support policy for one release.
    #[must_use]
    pub const fn new(current_major: u16, current_minor: u16) -> Self {
        Self {
            current_major,
            current_minor,
            reader_back: READER_WINDOW,
        }
    }

    /// Returns true when the reader accepts the format.
    #[must_use]
    pub const fn can_read(self, format: SnapshotPageVersion) -> bool {
        if format.major() != self.current_major || format.minor() > self.current_minor {
            return false;
        }
        self.current_minor.saturating_sub(format.minor()) <= self.reader_back
    }

    /// Checks one downgrade.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::DowngradeNotAllowed`] after the first new-format
    /// write.
    pub const fn check_downgrade(first_new_format_write: bool) -> Result<(), OpsError> {
        if first_new_format_write {
            return Err(OpsError::DowngradeNotAllowed);
        }
        Ok(())
    }
}

impl Default for SupportPolicy {
    fn default() -> Self {
        Self::new(1, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_accepts_the_current_format_and_one_back() {
        let policy = SupportPolicy::new(1, 2);
        assert!(policy.can_read(SnapshotPageVersion::new(1, 2)));
        assert!(policy.can_read(SnapshotPageVersion::new(1, 1)));
        assert!(!policy.can_read(SnapshotPageVersion::new(1, 0)));
        assert!(!policy.can_read(SnapshotPageVersion::new(1, 3)));
        assert!(!policy.can_read(SnapshotPageVersion::new(2, 2)));
    }

    #[test]
    fn a_downgrade_is_allowed_before_the_first_new_write() {
        assert_eq!(SupportPolicy::check_downgrade(false), Ok(()));
        assert_eq!(
            SupportPolicy::check_downgrade(true),
            Err(OpsError::DowngradeNotAllowed)
        );
    }
}
