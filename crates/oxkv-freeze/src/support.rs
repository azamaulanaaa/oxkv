//! The support policy, the format removal rule, and the no-downgrade guard.
//!
//! The 1.x line receives fixes for 24 months after the next major. The last
//! minor receives security fixes for 12 months. Two minors stay supported.
//!
//! A format is removed only at a major, after 12 months of notice, and only
//! with an automated migration. A store cannot downgrade after the first
//! write under a new format.

use core::fmt;

use oxkv_contracts::version::SnapshotPageVersion;

/// The number of months that the 1.x line receives fixes after the next major.
pub const FIXES_AFTER_NEXT_MAJOR_MONTHS: u16 = 24;

/// The number of months that the last minor receives security fixes.
pub const SECURITY_FIXES_LAST_MINOR_MONTHS: u16 = 12;

/// The number of supported minors.
pub const SUPPORTED_MINORS: u16 = 2;

/// The number of notice months before a format removal.
pub const REMOVAL_NOTICE_MONTHS: u16 = 12;

/// The published support policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportPolicy {
    /// The months of fixes after the next major.
    pub fixes_after_next_major_months: u16,
    /// The months of security fixes for the last minor.
    pub security_fixes_last_minor_months: u16,
    /// The number of supported minors.
    pub supported_minors: u16,
    /// The months of notice before a format removal.
    pub removal_notice_months: u16,
    /// True when a format removal needs an automated migration.
    pub requires_automated_migration: bool,
    /// True when the policy allows a downgrade.
    pub allows_downgrade: bool,
}

impl SupportPolicy {
    /// The path of the published support policy.
    pub const PUBLISHED_IN: &'static str = "docs/specs/freeze.md";

    /// Returns the published 1.0 policy.
    #[must_use]
    pub const fn published() -> Self {
        Self {
            fixes_after_next_major_months: FIXES_AFTER_NEXT_MAJOR_MONTHS,
            security_fixes_last_minor_months: SECURITY_FIXES_LAST_MINOR_MONTHS,
            supported_minors: SUPPORTED_MINORS,
            removal_notice_months: REMOVAL_NOTICE_MONTHS,
            requires_automated_migration: true,
            allows_downgrade: false,
        }
    }

    /// Checks the policy for a zero or weak value.
    ///
    /// # Errors
    ///
    /// Returns [`SupportError::InvalidPolicy`] when a window is zero, when
    /// fewer than two minors are supported, or when the policy allows a
    /// downgrade or a removal without an automated migration.
    pub fn validate(&self) -> Result<(), SupportError> {
        if self.fixes_after_next_major_months == 0 {
            return Err(SupportError::InvalidPolicy {
                field: "fixes_after_next_major_months",
            });
        }
        if self.security_fixes_last_minor_months == 0 {
            return Err(SupportError::InvalidPolicy {
                field: "security_fixes_last_minor_months",
            });
        }
        if self.removal_notice_months == 0 {
            return Err(SupportError::InvalidPolicy {
                field: "removal_notice_months",
            });
        }
        if self.supported_minors < SUPPORTED_MINORS {
            return Err(SupportError::InvalidPolicy {
                field: "supported_minors",
            });
        }
        if !self.requires_automated_migration {
            return Err(SupportError::InvalidPolicy {
                field: "requires_automated_migration",
            });
        }
        if self.allows_downgrade {
            return Err(SupportError::InvalidPolicy {
                field: "allows_downgrade",
            });
        }
        Ok(())
    }

    /// Checks one format removal plan against the policy.
    ///
    /// # Errors
    ///
    /// Returns [`SupportError::RemovalNotAtMajor`] when the plan removes a
    /// format outside a major release. Returns [`SupportError::NoticeTooShort`]
    /// when the notice is shorter than the policy allows. Returns
    /// [`SupportError::MigrationRequired`] when the plan has no automated
    /// migration.
    pub fn can_remove_format(&self, plan: RemovalPlan) -> Result<(), SupportError> {
        if !plan.at_major {
            return Err(SupportError::RemovalNotAtMajor);
        }
        if plan.notice_months < self.removal_notice_months {
            return Err(SupportError::NoticeTooShort {
                required: self.removal_notice_months,
                found: plan.notice_months,
            });
        }
        if self.requires_automated_migration && !plan.automated_migration {
            return Err(SupportError::MigrationRequired);
        }
        Ok(())
    }
}

impl Default for SupportPolicy {
    fn default() -> Self {
        Self::published()
    }
}

/// One plan to remove a format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovalPlan {
    /// True when the removal happens at a major release.
    pub at_major: bool,
    /// The months of notice before the removal.
    pub notice_months: u16,
    /// True when the plan provides an automated migration.
    pub automated_migration: bool,
}

/// An error from the support policy or the downgrade guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportError {
    /// A policy field has a weak value.
    InvalidPolicy {
        /// The field name.
        field: &'static str,
    },
    /// A format removal is outside a major release.
    RemovalNotAtMajor,
    /// A format removal has too little notice.
    NoticeTooShort {
        /// The required months of notice.
        required: u16,
        /// The given months of notice.
        found: u16,
    },
    /// A format removal has no automated migration.
    MigrationRequired,
    /// A store cannot open an older format after a newer write.
    DowngradeNotAllowed {
        /// The format of the first write at the newest format.
        written: SnapshotPageVersion,
        /// The format that the caller asks to open.
        target: SnapshotPageVersion,
    },
}

impl SupportError {
    /// The error code for a refused downgrade.
    pub const DOWNGRADE_CODE: &'static str = "ops.downgrade_not_allowed";

    /// Returns the named error code, when the error has one.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::DowngradeNotAllowed { .. } => Some(Self::DOWNGRADE_CODE),
            Self::InvalidPolicy { .. }
            | Self::RemovalNotAtMajor
            | Self::NoticeTooShort { .. }
            | Self::MigrationRequired => None,
        }
    }
}

impl fmt::Display for SupportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy { field } => {
                write!(
                    formatter,
                    "The support policy field {field} has a weak value."
                )
            }
            Self::RemovalNotAtMajor => {
                formatter.write_str("A format removal is allowed only at a major release.")
            }
            Self::NoticeTooShort { required, found } => write!(
                formatter,
                "The format removal has {found} months of notice. It needs {required}."
            ),
            Self::MigrationRequired => {
                formatter.write_str("A format removal needs an automated migration.")
            }
            Self::DowngradeNotAllowed { written, target } => write!(
                formatter,
                "The store cannot open format version {target} after a write under format version {written}."
            ),
        }
    }
}

impl std::error::Error for SupportError {}

/// Tracks the newest format that the store wrote.
///
/// The guard refuses an open of an older format after a newer write. The data
/// that the newer format wrote cannot be read by the older format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FormatGuard {
    written: Option<SnapshotPageVersion>,
}

impl FormatGuard {
    /// Creates a guard with no writes.
    #[must_use]
    pub const fn new() -> Self {
        Self { written: None }
    }

    /// Records one write under a format.
    ///
    /// The guard keeps the newest format. The function does not clear an
    /// earlier write.
    pub fn record_write(&mut self, format: SnapshotPageVersion) {
        self.written = match self.written {
            Some(current) if current >= format => Some(current),
            _ => Some(format),
        };
    }

    /// Returns the newest format that the store wrote.
    #[must_use]
    pub const fn written(self) -> Option<SnapshotPageVersion> {
        self.written
    }

    /// Returns true when the store wrote under a format.
    #[must_use]
    pub const fn has_written(self) -> bool {
        self.written.is_some()
    }

    /// Checks one open request against the write record.
    ///
    /// # Errors
    ///
    /// Returns [`SupportError::DowngradeNotAllowed`] when the target format is
    /// older than the newest write. The error code is
    /// [`SupportError::DOWNGRADE_CODE`].
    pub fn check_open(self, target: SnapshotPageVersion) -> Result<(), SupportError> {
        let Some(written) = self.written else {
            return Ok(());
        };
        if target < written {
            return Err(SupportError::DowngradeNotAllowed { written, target });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_policy_has_the_1_0_windows() {
        let policy = SupportPolicy::published();
        assert_eq!(policy.fixes_after_next_major_months, 24);
        assert_eq!(policy.security_fixes_last_minor_months, 12);
        assert_eq!(policy.supported_minors, 2);
        assert_eq!(policy.removal_notice_months, 12);
        assert!(policy.requires_automated_migration);
        assert!(!policy.allows_downgrade);
        policy.validate().expect("the published policy is valid");
    }

    #[test]
    fn a_format_removal_needs_a_major_and_notice_and_migration() {
        let policy = SupportPolicy::published();
        let good = RemovalPlan {
            at_major: true,
            notice_months: 12,
            automated_migration: true,
        };
        assert_eq!(policy.can_remove_format(good), Ok(()));
        assert_eq!(
            policy.can_remove_format(RemovalPlan {
                at_major: false,
                ..good
            }),
            Err(SupportError::RemovalNotAtMajor)
        );
        assert_eq!(
            policy.can_remove_format(RemovalPlan {
                notice_months: 11,
                ..good
            }),
            Err(SupportError::NoticeTooShort {
                required: 12,
                found: 11
            })
        );
    }

    #[test]
    fn a_downgrade_after_a_new_format_write_is_refused() {
        let mut guard = FormatGuard::new();
        assert_eq!(guard.check_open(SnapshotPageVersion::new(1, 0)), Ok(()));
        guard.record_write(SnapshotPageVersion::new(1, 1));
        let error = guard
            .check_open(SnapshotPageVersion::new(1, 0))
            .expect_err("the older format cannot read the newer write");
        assert_eq!(error.code(), Some("ops.downgrade_not_allowed"));
        assert_eq!(
            error,
            SupportError::DowngradeNotAllowed {
                written: SnapshotPageVersion::new(1, 1),
                target: SnapshotPageVersion::new(1, 0)
            }
        );
    }

    #[test]
    fn a_downgrade_before_the_first_write_is_allowed() {
        let guard = FormatGuard::new();
        assert_eq!(guard.check_open(SnapshotPageVersion::new(0, 12)), Ok(()));
        assert!(!guard.has_written());
    }

    #[test]
    fn an_upgrade_after_a_write_is_allowed() {
        let mut guard = FormatGuard::new();
        guard.record_write(SnapshotPageVersion::new(1, 0));
        assert_eq!(guard.check_open(SnapshotPageVersion::new(1, 1)), Ok(()));
    }
}
