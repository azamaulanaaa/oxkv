//! Projection invalidation.
//!
//! A fact-set change, a schema change, an ownership change, or a revocation
//! invalidates a projection. A policy predicate change that reads the same
//! facts with the same schema does not invalidate it.

use crate::epoch::ProjectionEpochState;
use crate::fact::FactSet;

/// The cause of one projection invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InvalidationCause {
    /// The declared fact set changed.
    FactSetChanged,
    /// The canonical type of a fact changed.
    SchemaChanged,
    /// The owner of a fact changed.
    OwnershipChanged,
    /// A fact was revoked.
    Revocation,
    /// A watermark moved backward.
    WatermarkRegression,
    /// A checksum did not match.
    ChecksumFailed,
}

impl InvalidationCause {
    /// Returns the canonical cause name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::FactSetChanged => "fact set changed",
            Self::SchemaChanged => "schema changed",
            Self::OwnershipChanged => "ownership changed",
            Self::Revocation => "revocation",
            Self::WatermarkRegression => "watermark regression",
            Self::ChecksumFailed => "checksum failed",
        }
    }
}

/// One declared policy change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PolicyChange {
    /// No policy change.
    None,
    /// A predicate changed. The fact set and schema did not change.
    PredicateChanged,
    /// The fact set changed.
    FactSetChanged,
    /// A fact schema changed.
    SchemaChanged,
    /// A fact owner changed.
    OwnershipChanged,
    /// A fact was revoked.
    Revocation,
}

impl PolicyChange {
    /// Returns the invalidation cause that this change declares.
    #[must_use]
    pub const fn cause(self) -> Option<InvalidationCause> {
        match self {
            Self::None | Self::PredicateChanged => None,
            Self::FactSetChanged => Some(InvalidationCause::FactSetChanged),
            Self::SchemaChanged => Some(InvalidationCause::SchemaChanged),
            Self::OwnershipChanged => Some(InvalidationCause::OwnershipChanged),
            Self::Revocation => Some(InvalidationCause::Revocation),
        }
    }
}

/// The result of one invalidation check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InvalidationDecision {
    cause: Option<InvalidationCause>,
}

impl InvalidationDecision {
    /// Creates a decision that keeps the projection.
    #[must_use]
    pub const fn keep() -> Self {
        Self { cause: None }
    }

    /// Creates a decision that invalidates the projection.
    #[must_use]
    pub const fn invalidate(cause: InvalidationCause) -> Self {
        Self { cause: Some(cause) }
    }

    /// Returns true when the projection must be invalidated.
    #[must_use]
    pub const fn invalidates(self) -> bool {
        self.cause.is_some()
    }

    /// Returns the cause when the decision invalidates.
    #[must_use]
    pub const fn cause(self) -> Option<InvalidationCause> {
        self.cause
    }
}

/// Compares two fact sets.
///
/// The function returns `None` when the fact set keeps every identity, type,
/// owner, and revocation flag.
#[must_use]
pub fn classify_fact_set_change(before: &FactSet, after: &FactSet) -> Option<InvalidationCause> {
    if before == after {
        return None;
    }
    if before.len() != after.len()
        || before
            .iter()
            .any(|(fact_id, _)| after.get(fact_id).is_none())
    {
        return Some(InvalidationCause::FactSetChanged);
    }
    let mut cause = None;
    for (fact_id, previous) in before.iter() {
        let Some(next) = after.get(fact_id) else {
            return Some(InvalidationCause::FactSetChanged);
        };
        if previous.is_revoked() != next.is_revoked() {
            return Some(InvalidationCause::Revocation);
        }
        if previous.canonical_type() != next.canonical_type() {
            return Some(InvalidationCause::SchemaChanged);
        }
        if previous.owner() != next.owner() {
            return Some(InvalidationCause::OwnershipChanged);
        }
        if previous != next {
            cause = Some(InvalidationCause::FactSetChanged);
        }
    }
    cause
}

/// Combines a fact-set comparison with one declared policy change.
///
/// A policy predicate change that keeps the same facts and schema returns
/// [`InvalidationDecision::keep`].
#[must_use]
pub fn evaluate_invalidation(
    before: &FactSet,
    after: &FactSet,
    policy_change: PolicyChange,
) -> InvalidationDecision {
    if let Some(cause) = classify_fact_set_change(before, after) {
        return InvalidationDecision::invalidate(cause);
    }
    match policy_change.cause() {
        Some(cause) => InvalidationDecision::invalidate(cause),
        None => InvalidationDecision::keep(),
    }
}

/// The record of one invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidationRecord {
    /// The invalidation cause.
    pub cause: InvalidationCause,
    /// The epoch before the invalidation.
    pub previous_epoch: ProjectionEpochState,
    /// The epoch after the invalidation.
    pub new_epoch: ProjectionEpochState,
}
