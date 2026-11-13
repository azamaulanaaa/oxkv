//! Named replica errors.
//!
//! Every variant maps to one code in the 1.0 error registry. A caller can show
//! [`ReplicaError::code`] to a person or match on the variant. The type carries
//! no receipt. A replica never writes.

use oxkv_contracts::version::{PageId, SnapshotPageVersion, TokenizerVersion};

/// A named error from one replica operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaError {
    /// The replica has no published generation.
    #[error(
        "The replica is not hydrated. Verified {progress} bytes of {target} bytes. Retry after hydration."
    )]
    NotHydrated {
        /// The verified byte count.
        progress: u64,
        /// The target byte count.
        target: u64,
    },
    /// The replica cannot meet the read staleness bound.
    #[error(
        "The read is stale. The bound is {bound} versions. The measured lag is {lag} versions."
    )]
    Stale {
        /// The requested version bound.
        bound: u64,
        /// The measured version lag.
        lag: u64,
    },
    /// The requested pin is above the published version.
    #[error(
        "The replica is behind the requested version. The published version is {applied}. The requested version is {requested}."
    )]
    BehindCommit {
        /// The published version.
        applied: u64,
        /// The requested version.
        requested: u64,
    },
    /// The requested pin is below the oldest retained version.
    #[error("The read pin expired. The oldest readable version is {oldest}.")]
    PinExpired {
        /// The oldest readable version.
        oldest: u64,
    },
    /// The key has no value at the served pin.
    #[error("The key is not present at the served pin.")]
    NotFound,
    /// The index tokenizer differs from the expected tokenizer.
    #[error("The search tokenizer does not match. Expected {expected}. Found {found}.")]
    TokenizerMismatch {
        /// The expected tokenizer version.
        expected: TokenizerVersion,
        /// The tokenizer version in the index.
        found: TokenizerVersion,
    },
    /// The index digest differs from the index bytes.
    #[error("The replica index digest does not match its bytes. Rebuild the index.")]
    IndexMismatch,
    /// A page digest differs from the page bytes.
    #[error("The page {page_id} failed its checksum at offset {offset}. Refetch the page.")]
    CorruptPage {
        /// The page identity.
        page_id: PageId,
        /// The first byte offset that fails.
        offset: u64,
    },
    /// A page record is outside the declared scope.
    #[error("The page {page_id} holds a record outside the declared scope. Reject the page.")]
    ScopeViolation {
        /// The page identity.
        page_id: PageId,
    },
    /// The page provenance does not match the scope pin.
    #[error("The page provenance does not match the scope pin. Reject the page.")]
    ScopePinMismatch,
    /// The page key range is outside the shard bounds.
    #[error("The page key range is outside the shard bounds. Reject the page.")]
    PageOutOfBounds,
    /// The page format major version is not supported.
    #[error("The page format version {found} is not supported. This reader supports {supported}.")]
    UnsupportedVersion {
        /// The supported version.
        supported: SnapshotPageVersion,
        /// The version in the page.
        found: SnapshotPageVersion,
    },
    /// The scope does not fit the device budget at rest.
    #[error("The scope needs {required} resident bytes. The device budget is {budget} bytes.")]
    DeviceBudgetExceeded {
        /// The required byte count.
        required: u64,
        /// The device budget.
        budget: u64,
    },
    /// The hydration peak does not fit the device budget.
    #[error(
        "Hydration needs {required} peak bytes. The hydration budget is {budget} bytes. Use a smaller scope."
    )]
    HydrationBudgetExceeded {
        /// The required peak byte count.
        required: u64,
        /// The hydration budget.
        budget: u64,
    },
    /// The resident bytes exceed the ceiling and eviction cannot help.
    #[error(
        "The replica cannot hold {required} resident bytes under the ceiling of {ceiling} bytes."
    )]
    ResourceExhausted {
        /// The required byte count.
        required: u64,
        /// The ceiling byte count.
        ceiling: u64,
    },
    /// The mirror rejects a local mutation.
    #[error("The replica mirror is read-only. A local mutation is rejected.")]
    LocalMutationRejected,
    /// A version vector would decrease.
    #[error("The applied version vector would decrease. A replica never moves backward.")]
    VersionRegression,
    /// Another tab holds the local tab lease.
    #[error("Another tab holds the local tab lease. Read through the leader tab.")]
    TabLeaseHeld,
    /// The local tab lease epoch is stale.
    #[error("The local tab lease epoch is stale. Expected {expected}. Found {found}.")]
    StaleTabLease {
        /// The current epoch.
        expected: u64,
        /// The epoch in the guard.
        found: u64,
    },
    /// A hydration is already in progress.
    #[error("A hydration is already in progress. Finish or abort it first.")]
    HydrationInProgress,
    /// No hydration is in progress.
    #[error("No hydration is in progress. Start a hydration first.")]
    HydrationNotStarted,
    /// The outbox already holds the idempotency key with another payload.
    #[error("The idempotency key is already in use with another payload.")]
    IdempotencyKeyReuse,
}

impl ReplicaError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotHydrated { .. } => "read.not_hydrated",
            Self::Stale { .. } => "read.stale",
            Self::BehindCommit { .. } => "read.behind_commit",
            Self::PinExpired { .. } => "read.pin_expired",
            Self::NotFound => "key.not_found",
            Self::TokenizerMismatch { .. } => "search.tokenizer_mismatch",
            Self::IndexMismatch => "search.index_mismatch",
            Self::CorruptPage { .. } => "format.corrupt_page",
            Self::ScopeViolation { .. } | Self::ScopePinMismatch => "scope.identity_mismatch",
            Self::PageOutOfBounds => "format.page_out_of_bounds",
            Self::UnsupportedVersion { .. } => "format.unsupported_version",
            Self::DeviceBudgetExceeded { .. } => "scope.device_budget_exceeded",
            Self::HydrationBudgetExceeded { .. } => "scope.hydration_budget_exceeded",
            Self::ResourceExhausted { .. } => "internal.resource_exhausted",
            Self::LocalMutationRejected => "projection.client_write_rejected",
            Self::VersionRegression | Self::HydrationInProgress | Self::HydrationNotStarted => {
                "internal.invariant_violation"
            }
            Self::TabLeaseHeld => "replication.lease_held",
            Self::StaleTabLease { .. } => "replication.stale_epoch",
            Self::IdempotencyKeyReuse => "write.idempotency_key_reuse",
        }
    }

    /// Returns true when a retry can succeed later.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::NotHydrated { .. }
                | Self::Stale { .. }
                | Self::BehindCommit { .. }
                | Self::CorruptPage { .. }
                | Self::ResourceExhausted { .. }
                | Self::TabLeaseHeld
        )
    }
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::lookup;
    use oxkv_contracts::version::{PageId, SnapshotPageVersion, TokenizerVersion};

    use super::ReplicaError;

    fn every_error() -> Vec<ReplicaError> {
        vec![
            ReplicaError::NotHydrated {
                progress: 1,
                target: 2,
            },
            ReplicaError::Stale { bound: 1, lag: 2 },
            ReplicaError::BehindCommit {
                applied: 1,
                requested: 2,
            },
            ReplicaError::PinExpired { oldest: 1 },
            ReplicaError::NotFound,
            ReplicaError::TokenizerMismatch {
                expected: TokenizerVersion::new(1),
                found: TokenizerVersion::new(2),
            },
            ReplicaError::IndexMismatch,
            ReplicaError::CorruptPage {
                page_id: PageId::from_bytes([1; 32]),
                offset: 0,
            },
            ReplicaError::ScopeViolation {
                page_id: PageId::from_bytes([1; 32]),
            },
            ReplicaError::ScopePinMismatch,
            ReplicaError::PageOutOfBounds,
            ReplicaError::UnsupportedVersion {
                supported: SnapshotPageVersion::new(1, 0),
                found: SnapshotPageVersion::new(2, 0),
            },
            ReplicaError::DeviceBudgetExceeded {
                required: 2,
                budget: 1,
            },
            ReplicaError::HydrationBudgetExceeded {
                required: 2,
                budget: 1,
            },
            ReplicaError::ResourceExhausted {
                required: 2,
                ceiling: 1,
            },
            ReplicaError::LocalMutationRejected,
            ReplicaError::VersionRegression,
            ReplicaError::TabLeaseHeld,
            ReplicaError::StaleTabLease {
                expected: 1,
                found: 2,
            },
            ReplicaError::HydrationInProgress,
            ReplicaError::HydrationNotStarted,
            ReplicaError::IdempotencyKeyReuse,
        ]
    }

    #[test]
    fn every_error_code_is_registered() {
        for error in every_error() {
            assert!(
                lookup(error.code()).is_some(),
                "the code {} is not registered",
                error.code()
            );
        }
    }

    #[test]
    fn every_error_displays_without_a_panic() {
        for error in every_error() {
            assert_ne!(error.to_string(), "");
        }
    }
}
