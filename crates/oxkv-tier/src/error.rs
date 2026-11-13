//! Named errors for tier placement, authority leases, and sealed artifacts.
//!
//! Every variant maps to one code in the contract error registry. The
//! `ChecksumMismatch` variant maps to `format.corrupt_page`. The remaining
//! variants map to the `replication` and `internal` namespaces.

/// A named error from tier placement, leases, or artifact verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum TierError {
    /// The writer lease term is below the accepted fence.
    #[error("The writer lease term is below the accepted fence.")]
    StaleEpoch,
    /// The authority lease expired.
    #[error("The authority lease expired.")]
    LeaseExpired,
    /// Another writer holds a valid authority lease.
    #[error("Another writer holds the authority lease.")]
    LeaseHeld,
    /// The eviction would remove the only verified copy.
    #[error("The eviction would remove the only verified copy.")]
    OnlyCopyEviction,
    /// The tier did not answer.
    #[error("The tier is unavailable.")]
    TierUnavailable,
    /// The warm-up needs more budget than the caller allows.
    #[error("The warm-up needs more budget than the caller allows.")]
    WarmupBudgetExceeded,
    /// The placement plan contradicts the placement policy.
    #[error("The placement plan contradicts the placement policy.")]
    PlacementViolation,
    /// The content digest does not match the artifact bytes.
    #[error("The content digest does not match the artifact bytes.")]
    ChecksumMismatch,
    /// A replica tried to hold an authority write lease.
    #[error("A replica cannot hold an authority write lease.")]
    ReplicaAuthorityLease,
    /// No further lease term is available.
    #[error("No further lease term is available.")]
    TermExhausted,
    /// The artifact is absent from every available tier.
    #[error("The artifact is absent from every available tier.")]
    ArtifactMissing,
}

impl TierError {
    /// Returns the canonical contract code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::StaleEpoch => "replication.stale_epoch",
            Self::LeaseExpired => "replication.lease_expired",
            Self::LeaseHeld => "replication.lease_held",
            Self::OnlyCopyEviction => "replication.only_copy_eviction",
            Self::TierUnavailable | Self::ArtifactMissing => "replication.tier_unavailable",
            Self::WarmupBudgetExceeded => "replication.warmup_budget_exceeded",
            Self::PlacementViolation => "replication.placement_violation",
            Self::ChecksumMismatch => "format.corrupt_page",
            Self::ReplicaAuthorityLease | Self::TermExhausted => "internal.invariant_violation",
        }
    }
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::error::ErrorCode;

    use super::TierError;

    #[test]
    fn every_tier_error_maps_to_a_registered_code() {
        let errors = [
            TierError::StaleEpoch,
            TierError::LeaseExpired,
            TierError::LeaseHeld,
            TierError::OnlyCopyEviction,
            TierError::TierUnavailable,
            TierError::WarmupBudgetExceeded,
            TierError::PlacementViolation,
            TierError::ChecksumMismatch,
            TierError::ReplicaAuthorityLease,
            TierError::TermExhausted,
            TierError::ArtifactMissing,
        ];
        for error in errors {
            assert!(
                ErrorCode::parse(error.code()).is_some(),
                "{} is not registered",
                error.code()
            );
        }
    }

    #[test]
    fn the_stale_epoch_code_is_stable() {
        assert_eq!(TierError::StaleEpoch.code(), "replication.stale_epoch");
        assert_eq!(TierError::ChecksumMismatch.code(), "format.corrupt_page");
    }
}
