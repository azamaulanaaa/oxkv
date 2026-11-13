//! Per-row policy evaluation and audit decisions.
//!
//! One production evaluator serves read scope and write validation. This crate
//! consumes that evaluator through [`PolicyEvaluator`]. It does not implement
//! a second evaluator.
//!
//! The evaluator is total. Every row returns an allow, a reject, or a named
//! budget failure.

use core::fmt;

use crate::projection::FactProjection;
use crate::request::{RowKey, RowMutation};
use crate::state::StoredRow;

/// The identity of one policy rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleId(String);

impl RuleId {
    /// Creates a rule identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the rule identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the system authorization rule.
    #[must_use]
    pub fn authorization() -> Self {
        Self::new("authorization")
    }

    /// Returns the system version rule.
    #[must_use]
    pub fn version() -> Self {
        Self::new("version")
    }

    /// Returns the system shard rule.
    #[must_use]
    pub fn shard() -> Self {
        Self::new("shard")
    }

    /// Returns the system batch rule.
    #[must_use]
    pub fn batch() -> Self {
        Self::new("batch")
    }

    /// Returns the system budget rule.
    #[must_use]
    pub fn budget() -> Self {
        Self::new("budget")
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A stable reason code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReasonCode(&'static str);

impl ReasonCode {
    /// The row passed policy.
    pub const ALLOWED: Self = Self::new("policy.allowed");
    /// The evaluator used its step, depth, or memory budget.
    pub const BUDGET_EXHAUSTED: Self = Self::new("policy.budget_exhausted");
    /// Strict authorization rejected stale facts.
    pub const AUTHORIZATION_STALE_FACTS: Self = Self::new("authorization.stale_facts");
    /// The projection is behind its bound.
    pub const PROJECTION_LAGGING: Self = Self::new("projection.lagging");
    /// The queued write reached its deadline.
    pub const QUEUE_TIMEOUT: Self = Self::new("write.queue_timeout");
    /// The projection checksum failed.
    pub const PROJECTION_CHECKSUM_FAILED: Self = Self::new("projection.checksum_failed");
    /// The projection watermark went backwards.
    pub const PROJECTION_WATERMARK_REGRESSION: Self = Self::new("projection.watermark_regression");
    /// A client attempted a projection write.
    pub const PROJECTION_CLIENT_WRITE: Self = Self::new("projection.client_write_rejected");
    /// The batch names more than one shard.
    pub const CROSS_MUTATION: Self = Self::new("shard.cross_mutation_rejected");
    /// The base version is ahead of the current key version.
    pub const STALE_BASE_VERSION: Self = Self::new("write.stale_base_version");
    /// The base version does not match the current key version.
    pub const VERSION_CONFLICT: Self = Self::new("write.version_conflict");
    /// The request has no base version.
    pub const MISSING_BASE_VERSION: Self = Self::new("write.missing_base_version");
    /// The batch is larger than the decision list bound.
    pub const BATCH_TOO_LARGE: Self = Self::new("write.batch_too_large");
    /// The audit append failed.
    pub const AUDIT_UNAVAILABLE: Self = Self::new("audit.unavailable");
    /// A rule denied the row.
    pub const FORBIDDEN: Self = Self::new("auth.forbidden");

    /// Creates a reason code from a stable string.
    #[must_use]
    pub const fn new(code: &'static str) -> Self {
        Self(code)
    }

    /// Returns the stable code text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ReasonCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

/// The verdict of one row decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionVerdict {
    /// The row passed policy.
    Allow,
    /// The row failed policy.
    Reject,
}

impl DecisionVerdict {
    /// Returns the canonical decision name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Reject => "reject",
        }
    }
}

impl fmt::Display for DecisionVerdict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One row decision in the audit decision list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// The row key.
    pub row_key: RowKey,
    /// The rule identity.
    pub rule: RuleId,
    /// The row verdict.
    pub verdict: DecisionVerdict,
    /// The stable reason code.
    pub reason: ReasonCode,
}

impl Decision {
    /// Creates an allow decision.
    #[must_use]
    pub fn allow(row_key: RowKey, rule: RuleId) -> Self {
        Self {
            row_key,
            rule,
            verdict: DecisionVerdict::Allow,
            reason: ReasonCode::ALLOWED,
        }
    }

    /// Creates a reject decision.
    #[must_use]
    pub fn reject(row_key: RowKey, rule: RuleId, reason: ReasonCode) -> Self {
        Self {
            row_key,
            rule,
            verdict: DecisionVerdict::Reject,
            reason,
        }
    }
}

/// The context of one row evaluation.
#[derive(Debug, Clone, Copy)]
pub struct RowContext<'a> {
    /// The stored value before the write.
    pub before: Option<&'a StoredRow>,
    /// The proposed row change.
    pub after: &'a RowMutation,
    /// The pinned fact projection.
    pub projection: &'a FactProjection,
}

/// The result of one row evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evaluation {
    /// The row passed the rule.
    Allow {
        /// The rule identity.
        rule: RuleId,
    },
    /// The row failed the rule.
    Reject {
        /// The rule identity.
        rule: RuleId,
        /// The stable reason code.
        reason: ReasonCode,
    },
    /// The evaluation used its budget.
    BudgetExhausted {
        /// The rule identity.
        rule: RuleId,
    },
}

/// Evaluates one row against the pinned projection.
///
/// The authority supplies the production evaluator. A test-only oracle is
/// allowed outside the production path.
pub trait PolicyEvaluator {
    /// Evaluates one row.
    fn evaluate(&self, context: &RowContext<'_>) -> Evaluation;
}
