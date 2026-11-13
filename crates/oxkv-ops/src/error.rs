//! Operation errors with canonical registry codes.
//!
//! Every error names one code from the 1.0 error registry. A denied write
//! carries a `Rejected` receipt state. A read or control error has no receipt.

use oxkv_contracts::error::{ErrorSpec, lookup};
use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::{ShardId, SnapshotPageVersion};

use crate::quota::{QuotaResource, RateLimitKey};
use crate::receipts::RebasePath;

/// One operation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpsError {
    /// The backup retention pin is absent or lost.
    #[error("The backup pin {pin} is lost.")]
    BackupPinLost {
        /// The lost pin identity.
        pin: String,
    },
    /// A restore object checksum does not match its manifest digest.
    #[error("The restore checksum does not match at object {object}.")]
    RestoreChecksumMismatch {
        /// The object key.
        object: String,
    },
    /// A migration step did not complete.
    #[error("The migration is incomplete at step {step}.")]
    MigrationIncomplete {
        /// The step that is incomplete.
        step: &'static str,
    },
    /// A downgrade is not allowed after the first new-format write.
    #[error("The store cannot downgrade after the first new-format write.")]
    DowngradeNotAllowed,
    /// A client local outbox entry is older than the authority.
    #[error("The client outbox entry is stale. Rebase at {rebase}.")]
    ClientOutboxStale {
        /// The rebase path that the client must follow.
        rebase: RebasePath,
    },
    /// A shard quota is exceeded at admission.
    #[error(
        "The {resource} quota is exceeded. The limit is {limit} and the request would use {used}."
    )]
    QuotaExceeded {
        /// The exceeded resource.
        resource: QuotaResource,
        /// The configured limit.
        limit: u64,
        /// The usage that the request would reach.
        used: u64,
    },
    /// A token bucket is empty.
    #[error("The rate limit {key} is exceeded.")]
    RateLimited {
        /// The exhausted bucket.
        key: RateLimitKey,
    },
    /// A shard is frozen for maintenance.
    #[error("The shard {shard_id:?} is frozen for maintenance.")]
    MaintenanceFrozen {
        /// The frozen shard.
        shard_id: ShardId,
    },
    /// The reader does not support the artifact format.
    #[error("The format version {version} is not supported.")]
    UnsupportedFormat {
        /// The unsupported format version.
        version: SnapshotPageVersion,
    },
}

impl OpsError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BackupPinLost { .. } => "ops.backup_pin_lost",
            Self::RestoreChecksumMismatch { .. } => "ops.restore_checksum_mismatch",
            Self::MigrationIncomplete { .. } => "ops.migration_incomplete",
            Self::DowngradeNotAllowed => "ops.downgrade_not_allowed",
            Self::ClientOutboxStale { .. } => "ops.client_outbox_stale",
            Self::QuotaExceeded { .. } => "ops.quota_exceeded",
            Self::RateLimited { .. } => "ops.rate_limited",
            Self::MaintenanceFrozen { .. } => "shard.frozen_for_maintenance",
            Self::UnsupportedFormat { .. } => "format.unsupported_version",
        }
    }

    /// Returns the registry row for this error.
    #[must_use]
    pub fn spec(&self) -> Option<&'static ErrorSpec> {
        lookup(self.code())
    }

    /// Returns the receipt state when this error is a write verdict.
    ///
    /// A quota rejection and a maintenance freeze are `Rejected` write
    /// verdicts. A read or control error has no receipt.
    #[must_use]
    pub const fn receipt_state(&self) -> Option<ReceiptState> {
        match self {
            Self::QuotaExceeded { .. } | Self::MaintenanceFrozen { .. } => {
                Some(ReceiptState::Rejected)
            }
            Self::BackupPinLost { .. }
            | Self::RestoreChecksumMismatch { .. }
            | Self::MigrationIncomplete { .. }
            | Self::DowngradeNotAllowed
            | Self::ClientOutboxStale { .. }
            | Self::RateLimited { .. }
            | Self::UnsupportedFormat { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_a_registered_code() {
        let sample = OpsError::MigrationIncomplete { step: "rewrite" };
        assert_eq!(sample.code(), "ops.migration_incomplete");
        for error in [
            OpsError::BackupPinLost {
                pin: "pin-1".to_owned(),
            },
            OpsError::RestoreChecksumMismatch {
                object: "snap".to_owned(),
            },
            OpsError::MigrationIncomplete { step: "rewrite" },
            OpsError::DowngradeNotAllowed,
            OpsError::ClientOutboxStale {
                rebase: RebasePath::new(
                    oxkv_contracts::version::ManifestDigest::from_bytes([1; 32]),
                    oxkv_contracts::version::ScopeVersionVector::new(),
                    crate::receipts::RebaseAction::RebaseOnLatest,
                ),
            },
            OpsError::QuotaExceeded {
                resource: QuotaResource::StoredBytes,
                limit: 10,
                used: 11,
            },
            OpsError::RateLimited {
                key: RateLimitKey::shard(
                    ShardId::new(vec![1]),
                    crate::quota::OperationClass::Write,
                ),
            },
            OpsError::MaintenanceFrozen {
                shard_id: ShardId::new(vec![1]),
            },
            OpsError::UnsupportedFormat {
                version: SnapshotPageVersion::new(9, 0),
            },
        ] {
            assert!(
                error.spec().is_some(),
                "the code {} is not registered",
                error.code()
            );
        }
    }

    #[test]
    fn a_quota_rejection_is_a_rejected_receipt() {
        let error = OpsError::QuotaExceeded {
            resource: QuotaResource::StoredBytes,
            limit: 10,
            used: 11,
        };
        assert_eq!(error.receipt_state(), Some(ReceiptState::Rejected));
    }

    #[test]
    fn a_control_error_has_no_receipt() {
        assert_eq!(OpsError::DowngradeNotAllowed.receipt_state(), None);
    }
}
