//! Named replication errors.
//!
//! Each value maps to one code in the 1.0 error registry. A code with a
//! [`ReceiptState::Rejected`] state closes the write with a terminal receipt.
//! A code with no receipt ends the call and writes no receipt.

use core::fmt;

use oxkv_contracts::error::{ErrorSpec, lookup};
use oxkv_contracts::receipt::ReceiptState;

/// A replication failure.
///
/// The `code` method returns the canonical registry code. A caller must use
/// that code in a receipt and in an audit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplError {
    /// The writer presented a term that is not the owner term.
    StaleEpoch {
        /// The presented term.
        presented: u64,
        /// The current owner term.
        current: u64,
    },
    /// The authority lease is no longer valid.
    LeaseExpired {
        /// The lease term that expired.
        term: u64,
    },
    /// Another holder owns the authority lease.
    LeaseHeld {
        /// The held lease term.
        term: u64,
    },
    /// A replication tier is not available.
    TierUnavailable {
        /// The tier name.
        tier: &'static str,
    },
    /// A member did not catch up inside the warmup budget.
    WarmupBudgetExceeded {
        /// The allowed number of ticks or bytes.
        budget: u64,
        /// The used number of ticks or bytes.
        used: u64,
    },
    /// A member set or a placement violates the shard map.
    PlacementViolation {
        /// The reason text.
        reason: &'static str,
    },
    /// The driver reached a state that its invariants forbid.
    InvariantViolation {
        /// The reason text.
        reason: &'static str,
    },
    /// A read asked for an index that the replica did not apply.
    ReadBehindCommit {
        /// The requested log index.
        requested: u64,
        /// The applied log index.
        applied: u64,
    },
    /// A read would serve data older than its declared bound.
    ReadStale {
        /// The oldest served version.
        served: u64,
        /// The bound version.
        bound: u64,
    },
}

impl ReplError {
    /// Returns the canonical registry code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::StaleEpoch { .. } => "replication.stale_epoch",
            Self::LeaseExpired { .. } => "replication.lease_expired",
            Self::LeaseHeld { .. } => "replication.lease_held",
            Self::TierUnavailable { .. } => "replication.tier_unavailable",
            Self::WarmupBudgetExceeded { .. } => "replication.warmup_budget_exceeded",
            Self::PlacementViolation { .. } => "replication.placement_violation",
            Self::InvariantViolation { .. } => "internal.invariant_violation",
            Self::ReadBehindCommit { .. } => "read.behind_commit",
            Self::ReadStale { .. } => "read.stale",
        }
    }

    /// Returns the registry row for this error.
    ///
    /// Returns `None` only when the copy of `oxkv-contracts` is older than
    /// this crate. A test pins the row for every code.
    #[must_use]
    pub fn registry_spec(self) -> Option<&'static ErrorSpec> {
        lookup(self.code())
    }

    /// Returns the receipt state for this error.
    ///
    /// A value of `None` means that the error writes no receipt.
    #[must_use]
    pub const fn receipt_state(self) -> Option<ReceiptState> {
        match self {
            Self::StaleEpoch { .. }
            | Self::LeaseExpired { .. }
            | Self::PlacementViolation { .. } => Some(ReceiptState::Rejected),
            Self::LeaseHeld { .. }
            | Self::TierUnavailable { .. }
            | Self::WarmupBudgetExceeded { .. }
            | Self::InvariantViolation { .. }
            | Self::ReadBehindCommit { .. }
            | Self::ReadStale { .. } => None,
        }
    }
}

impl fmt::Display for ReplError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleEpoch { presented, current } => write!(
                formatter,
                "The presented lease term {presented} is stale. The owner term is {current}. Acquire leadership again."
            ),
            Self::LeaseExpired { term } => {
                write!(
                    formatter,
                    "The lease for term {term} expired. Acquire leadership again."
                )
            }
            Self::LeaseHeld { term } => {
                write!(
                    formatter,
                    "Another holder owns the lease for term {term}. Request a hint."
                )
            }
            Self::TierUnavailable { tier } => {
                write!(
                    formatter,
                    "The replication tier {tier} is not available. Retry after a backoff."
                )
            }
            Self::WarmupBudgetExceeded { budget, used } => write!(
                formatter,
                "The member used {used} of {budget} warmup units. Continue with a cursor."
            ),
            Self::PlacementViolation { reason } => {
                write!(formatter, "The placement is not valid. {reason}")
            }
            Self::InvariantViolation { reason } => {
                write!(formatter, "The driver invariant is false. {reason}")
            }
            Self::ReadBehindCommit { requested, applied } => write!(
                formatter,
                "The read asked for index {requested}. The replica applied index {applied}. Retry after the apply."
            ),
            Self::ReadStale { served, bound } => write!(
                formatter,
                "The read would serve version {served}. The bound requires version {bound}."
            ),
        }
    }
}

impl std::error::Error for ReplError {}
