//! Named search errors.
//!
//! Every error maps to one code in the canonical 1.0 error registry. A caller
//! branches on [`SearchError::code`]. A caller does not parse the message.

use core::fmt;

use oxkv_contracts::version::{ContentDigest, ScopeVersionVector, ShardId, TokenizerVersion};

use crate::coverage::CoverageDescriptor;

/// The reason that a search cursor is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorInvalidReason {
    /// The token is malformed or tampered.
    Malformed,
    /// The token belongs to another cursor type.
    TypeMismatch,
    /// The token version is not supported.
    UnsupportedVersion,
    /// The cursor pin differs from the request pin.
    PinMismatch,
    /// The cursor query fingerprint differs from the request fingerprint.
    QueryMismatch,
    /// The cursor sort tuple does not match the request order.
    SortTupleMismatch,
    /// The request uses coverage mode with an exact cursor.
    CoverageCursor,
    /// The cursor is past its pin lifetime.
    Expired,
}

impl CursorInvalidReason {
    /// Returns the canonical reason name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Malformed => "malformed token",
            Self::TypeMismatch => "cursor type mismatch",
            Self::UnsupportedVersion => "unsupported token version",
            Self::PinMismatch => "pin mismatch",
            Self::QueryMismatch => "query mismatch",
            Self::SortTupleMismatch => "sort tuple mismatch",
            Self::CoverageCursor => "coverage cursor",
            Self::Expired => "expired cursor",
        }
    }
}

impl fmt::Display for CursorInvalidReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A named search error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    /// The search pin is outside the readable range.
    #[error(
        "The search pin expired. Requested {requested:?}. Oldest readable {oldest_readable:?}."
    )]
    PinExpired {
        /// The requested version vector.
        requested: ScopeVersionVector,
        /// The oldest readable version vector.
        oldest_readable: ScopeVersionVector,
    },
    /// The pin has no entry for one targeted shard.
    #[error("The search pin has no version for the shard {shard_id:?}.")]
    PinMissingShard {
        /// The targeted shard.
        shard_id: ShardId,
    },
    /// The search cursor does not match the request.
    #[error("The search cursor is invalid. Reason: {reason}.")]
    CursorInvalid {
        /// The invalid reason.
        reason: CursorInvalidReason,
    },
    /// The query targets more shards than the fan-out limit.
    #[error("The search fan-out exceeded the limit. Requested {requested}, limit {limit}.")]
    FanoutExceeded {
        /// The number of targeted shards.
        requested: usize,
        /// The fan-out limit.
        limit: usize,
    },
    /// One targeted shard did not answer.
    #[error("The shard {shard_id:?} is unavailable.")]
    ShardUnavailable {
        /// The unavailable shard.
        shard_id: ShardId,
    },
    /// The shard index digest does not match the expected digest.
    #[error("The index digest does not match on shard {shard_id:?}.")]
    IndexMismatch {
        /// The shard.
        shard_id: ShardId,
        /// The expected digest.
        expected: Option<ContentDigest>,
        /// The found digest.
        found: Option<ContentDigest>,
    },
    /// A field tokenizer version does not match the expected version.
    #[error(
        "The tokenizer version does not match for field {field}. Expected {expected}, found {found}."
    )]
    TokenizerMismatch {
        /// The field name.
        field: String,
        /// The expected tokenizer version.
        expected: TokenizerVersion,
        /// The found tokenizer version.
        found: TokenizerVersion,
    },
    /// The shard does not declare a tokenizer version for one field.
    #[error("The tokenizer version is unknown for field {field}.")]
    TokenizerUnknown {
        /// The field name.
        field: String,
    },
    /// The token count exceeds the token budget.
    #[error("The token budget is exhausted. Required {required}, limit {limit}.")]
    TokenBudgetExceeded {
        /// The required token count.
        required: u64,
        /// The token budget.
        limit: u64,
    },
    /// The query budget is exhausted.
    #[error("The query budget is exhausted. Required {required}, limit {limit}.")]
    QueryBudgetExceeded {
        /// The required cost.
        required: u64,
        /// The query budget.
        limit: u64,
        /// The coverage descriptor in coverage mode.
        coverage: Option<CoverageDescriptor>,
    },
    /// One shard stream is not in comparator order.
    #[error("The search stream is not ordered on shard {shard_id:?}.")]
    UnorderedShard {
        /// The unordered shard.
        shard_id: ShardId,
    },
    /// Two shards returned the same canonical key at one pin.
    #[error("The search result repeats the key {key:?}.")]
    DuplicateHit {
        /// The repeated canonical key.
        key: Vec<u8>,
    },
}

impl SearchError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PinExpired { .. } => "search.pin_expired",
            Self::PinMissingShard { .. } => "search.pin_missing_shard",
            Self::CursorInvalid { .. } => "search.cursor_invalid",
            Self::FanoutExceeded { .. } => "search.fanout_exceeded",
            Self::ShardUnavailable { .. } => "scope.shard_unreachable",
            Self::IndexMismatch { .. } => "search.index_mismatch",
            Self::UnorderedShard { .. } => "search.unordered_shard",
            Self::DuplicateHit { .. } => "search.duplicate_hit",
            Self::TokenizerMismatch { .. } => "search.tokenizer_mismatch",
            Self::TokenizerUnknown { .. } => "search.tokenizer_unknown",
            Self::TokenBudgetExceeded { .. } => "search.token_budget_exceeded",
            Self::QueryBudgetExceeded { .. } => "query.budget_exceeded",
        }
    }

    /// Returns the coverage descriptor when the error carries one.
    #[must_use]
    pub const fn coverage(&self) -> Option<&CoverageDescriptor> {
        match self {
            Self::QueryBudgetExceeded { coverage, .. } => coverage.as_ref(),
            _ => None,
        }
    }
}
