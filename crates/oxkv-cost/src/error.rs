//! The cost error set and its frozen error codes.
//!
//! The codes come from section 8.3 of the 1.0 specification. A cost error is a
//! named failure. The model never returns a silent subset.

use core::fmt;

use oxkv_contracts::{CommitVersion, ShardId};

/// One frozen cost error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CostCode {
    /// The query cost is above the active budget.
    QueryBudgetExceeded,
    /// The query fan-out is above the fan-out limit.
    SearchFanoutExceeded,
    /// The query token count is above the token budget.
    SearchTokenBudgetExceeded,
    /// A shard can no longer serve the pinned version.
    SearchPinExpired,
    /// A targeted shard cannot answer.
    ScopeShardUnreachable,
    /// A reply names a shard outside the pin.
    ScopeShardNotPinned,
    /// One shard appears more than one time.
    QueryDuplicateShard,
    /// One canonical key appears with different content.
    QueryDuplicateKey,
    /// A counter overflowed.
    QueryCounterOverflow,
    /// An internal invariant failed.
    InternalInvariantViolation,
}

impl CostCode {
    /// Every cost code.
    pub const ALL: &'static [Self] = &[
        Self::QueryBudgetExceeded,
        Self::SearchFanoutExceeded,
        Self::SearchTokenBudgetExceeded,
        Self::SearchPinExpired,
        Self::ScopeShardUnreachable,
        Self::ScopeShardNotPinned,
        Self::QueryDuplicateShard,
        Self::QueryDuplicateKey,
        Self::QueryCounterOverflow,
        Self::InternalInvariantViolation,
    ];

    /// Returns the canonical error code text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryBudgetExceeded => "query.budget_exceeded",
            Self::SearchFanoutExceeded => "search.fanout_exceeded",
            Self::SearchTokenBudgetExceeded => "search.token_budget_exceeded",
            Self::SearchPinExpired => "search.pin_expired",
            Self::ScopeShardUnreachable => "scope.shard_unreachable",
            Self::ScopeShardNotPinned => "scope.shard_not_pinned",
            Self::QueryDuplicateShard => "query.duplicate_shard",
            Self::QueryDuplicateKey => "query.duplicate_key",
            Self::QueryCounterOverflow => "query.counter_overflow",
            Self::InternalInvariantViolation => "internal.invariant_violation",
        }
    }
}

impl fmt::Display for CostCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A named cost failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CostError {
    /// The query cost is above the active budget.
    #[error("The query needs {needed_milli} cost milliunits. The budget is {budget_milli}.")]
    BudgetExceeded {
        /// The cost that the query needs.
        needed_milli: u64,
        /// The cost that is available.
        budget_milli: u64,
    },
    /// The query fan-out is above the limit.
    #[error("The query fan-out is {requested}. The limit is {limit}.")]
    FanoutExceeded {
        /// The planned shard count.
        requested: u32,
        /// The shard limit.
        limit: u32,
    },
    /// The query token count is above the budget.
    #[error("The query needs {needed} tokens. The budget is {budget}.")]
    TokenBudgetExceeded {
        /// The token count that the query needs.
        needed: u64,
        /// The token count that is available.
        budget: u64,
    },
    /// A shard served a version other than the pin.
    #[error(
        "The shard {shard_id:?} did not serve the pin. Requested {requested}, served {served}."
    )]
    PinExpired {
        /// The shard that did not serve the pin.
        shard_id: ShardId,
        /// The requested commit version.
        requested: CommitVersion,
        /// The served commit version.
        served: CommitVersion,
    },
    /// A targeted shard cannot answer.
    #[error("The shard {shard_id:?} is unavailable.")]
    ShardUnavailable {
        /// The unavailable shard.
        shard_id: ShardId,
    },
    /// A reply names a shard outside the pin.
    #[error("The shard {shard_id:?} is not in the pin.")]
    ShardNotPinned {
        /// The shard outside the pin.
        shard_id: ShardId,
    },
    /// One shard appears more than one time.
    #[error("The shard {shard_id:?} appears more than one time.")]
    DuplicateShard {
        /// The repeated shard.
        shard_id: ShardId,
    },
    /// One canonical key appears with different content.
    #[error("The canonical key {key:?} has more than one content.")]
    DuplicateKey {
        /// The canonical key with conflicting content.
        key: Vec<u8>,
    },
    /// A counter overflowed.
    #[error("The counter {field} overflowed.")]
    CounterOverflow {
        /// The name of the counter.
        field: &'static str,
    },
}

impl CostError {
    /// Returns the frozen error code.
    #[must_use]
    pub const fn code(&self) -> CostCode {
        match self {
            Self::BudgetExceeded { .. } => CostCode::QueryBudgetExceeded,
            Self::FanoutExceeded { .. } => CostCode::SearchFanoutExceeded,
            Self::TokenBudgetExceeded { .. } => CostCode::SearchTokenBudgetExceeded,
            Self::PinExpired { .. } => CostCode::SearchPinExpired,
            Self::ShardUnavailable { .. } => CostCode::ScopeShardUnreachable,
            Self::ShardNotPinned { .. } => CostCode::ScopeShardNotPinned,
            Self::DuplicateShard { .. } => CostCode::QueryDuplicateShard,
            Self::DuplicateKey { .. } => CostCode::QueryDuplicateKey,
            Self::CounterOverflow { .. } => CostCode::QueryCounterOverflow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_the_registered_text() {
        assert_eq!(
            CostCode::QueryBudgetExceeded.as_str(),
            "query.budget_exceeded"
        );
        assert_eq!(
            CostCode::SearchFanoutExceeded.as_str(),
            "search.fanout_exceeded"
        );
        assert_eq!(
            CostCode::SearchTokenBudgetExceeded.as_str(),
            "search.token_budget_exceeded"
        );
        assert_eq!(CostCode::SearchPinExpired.as_str(), "search.pin_expired");
        assert_eq!(
            CostCode::ScopeShardUnreachable.as_str(),
            "scope.shard_unreachable"
        );
        assert_eq!(
            CostCode::InternalInvariantViolation.as_str(),
            "internal.invariant_violation"
        );
        assert_eq!(CostCode::ALL.len(), 10);
    }

    #[test]
    fn each_error_maps_to_one_code() {
        let error = CostError::BudgetExceeded {
            needed_milli: 10,
            budget_milli: 5,
        };
        assert_eq!(error.code(), CostCode::QueryBudgetExceeded);
        let error = CostError::FanoutExceeded {
            requested: 4,
            limit: 2,
        };
        assert_eq!(error.code(), CostCode::SearchFanoutExceeded);
        let error = CostError::TokenBudgetExceeded {
            needed: 9,
            budget: 4,
        };
        assert_eq!(error.code(), CostCode::SearchTokenBudgetExceeded);
    }
}
