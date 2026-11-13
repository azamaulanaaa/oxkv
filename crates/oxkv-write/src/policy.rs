//! Pre-write policy hooks.
//!
//! Policy runs after the cross-shard check and after the base-version check.
//! Policy is reject-only. It cannot rewrite the batch.
//!
//! A policy implementation must be pure. It must not read a clock, draw
//! randomness, perform I/O, or keep state.

use core::fmt;

use oxkv_contracts::version::KeyVersion;

use crate::record::KeyRecord;
use crate::request::{Key, Mutation, WriteRequest};

/// A rule identity.
///
/// The audit record carries one rule identity for the evaluation and one rule
/// identity for each row decision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleIdentity(String);

impl RuleIdentity {
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

    /// Returns true when the identity is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for RuleIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One request row that a policy can inspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyRow<'a> {
    mutation: &'a Mutation,
    current: Option<&'a KeyRecord>,
}

impl<'a> PolicyRow<'a> {
    /// Creates a policy row.
    pub(crate) const fn new(mutation: &'a Mutation, current: Option<&'a KeyRecord>) -> Self {
        Self { mutation, current }
    }

    /// Returns the request mutation.
    #[must_use]
    pub const fn mutation(&self) -> &'a Mutation {
        self.mutation
    }

    /// Returns the current record. The value is `None` when the key is absent.
    #[must_use]
    pub const fn current(&self) -> Option<&'a KeyRecord> {
        self.current
    }

    /// Returns the key.
    #[must_use]
    pub const fn key(&self) -> &'a Key {
        self.mutation.key()
    }

    /// Returns the current key version. An absent key has version zero.
    #[must_use]
    pub fn current_version(&self) -> KeyVersion {
        self.current
            .map_or(KeyVersion::new(0), KeyRecord::key_version)
    }

    /// Returns the current live value. A tombstone returns `None`.
    #[must_use]
    pub fn current_value(&self) -> Option<&'a [u8]> {
        self.current.and_then(KeyRecord::as_value)
    }
}

/// The verdict of one policy evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyVerdict {
    /// The request may commit.
    Allow,
    /// The authority refuses the request for the named reason code.
    Reject {
        /// A registered error code that maps to a `Rejected` receipt.
        reason: &'static str,
    },
}

impl PolicyVerdict {
    /// Returns true when the verdict allows the request.
    #[must_use]
    pub const fn is_allow(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// The verdict of one row decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionVerdict {
    /// The row passed the rule.
    Allow,
    /// The row failed the rule.
    Reject,
}

impl DecisionVerdict {
    /// Returns the canonical decision name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Allow => "Allow",
            Self::Reject => "Reject",
        }
    }

    /// Returns the stable wire tag.
    ///
    /// The tag does not depend on the Rust enum discriminant.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Allow => 1,
            Self::Reject => 2,
        }
    }
}

/// One row decision in the audit decision list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    key: Key,
    rule: RuleIdentity,
    verdict: DecisionVerdict,
    reason: Option<&'static str>,
}

impl PolicyDecision {
    /// Creates an allow decision.
    ///
    /// An allow decision has no reason code.
    #[must_use]
    pub fn allow(key: Key, rule: RuleIdentity) -> Self {
        Self {
            key,
            rule,
            verdict: DecisionVerdict::Allow,
            reason: None,
        }
    }

    /// Creates a reject decision.
    #[must_use]
    pub fn reject(key: Key, rule: RuleIdentity, reason: &'static str) -> Self {
        Self {
            key,
            rule,
            verdict: DecisionVerdict::Reject,
            reason: Some(reason),
        }
    }

    /// Returns the row key.
    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the rule identity.
    #[must_use]
    pub const fn rule(&self) -> &RuleIdentity {
        &self.rule
    }

    /// Returns the row verdict.
    #[must_use]
    pub const fn verdict(&self) -> DecisionVerdict {
        self.verdict
    }

    /// Returns the reason code. An allow decision has no reason code.
    #[must_use]
    pub const fn reason(&self) -> Option<&'static str> {
        self.reason
    }
}

/// The result of one policy evaluation.
///
/// The evaluation names the rule identity and one bounded decision for each
/// row. The authority copies both values into the audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEvaluation {
    verdict: PolicyVerdict,
    rule_identity: RuleIdentity,
    decisions: Vec<PolicyDecision>,
}

impl PolicyEvaluation {
    /// Creates an allow evaluation.
    #[must_use]
    pub fn allow(rule_identity: RuleIdentity, decisions: Vec<PolicyDecision>) -> Self {
        Self {
            verdict: PolicyVerdict::Allow,
            rule_identity,
            decisions,
        }
    }

    /// Creates a reject evaluation.
    #[must_use]
    pub fn reject(
        rule_identity: RuleIdentity,
        reason: &'static str,
        decisions: Vec<PolicyDecision>,
    ) -> Self {
        Self {
            verdict: PolicyVerdict::Reject { reason },
            rule_identity,
            decisions,
        }
    }

    /// Returns the overall verdict.
    #[must_use]
    pub const fn verdict(&self) -> PolicyVerdict {
        self.verdict
    }

    /// Returns true when the evaluation allows the request.
    #[must_use]
    pub const fn is_allow(&self) -> bool {
        self.verdict.is_allow()
    }

    /// Returns the reject reason. An allow evaluation has no reason.
    #[must_use]
    pub const fn reason(&self) -> Option<&'static str> {
        match self.verdict {
            PolicyVerdict::Allow => None,
            PolicyVerdict::Reject { reason } => Some(reason),
        }
    }

    /// Returns the rule identity.
    #[must_use]
    pub const fn rule_identity(&self) -> &RuleIdentity {
        &self.rule_identity
    }

    /// Returns the row decisions in request order.
    #[must_use]
    pub fn decisions(&self) -> &[PolicyDecision] {
        &self.decisions
    }
}

/// A pure pre-write policy.
pub trait Policy {
    /// Evaluates one request against the current key records.
    fn evaluate(&self, request: &WriteRequest, rows: &[PolicyRow<'_>]) -> PolicyEvaluation;
}

/// A policy that allows every request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AllowAll;

impl Policy for AllowAll {
    fn evaluate(&self, _request: &WriteRequest, rows: &[PolicyRow<'_>]) -> PolicyEvaluation {
        let rule = RuleIdentity::new("allow-all");
        let decisions = rows
            .iter()
            .map(|row| PolicyDecision::allow(row.key().clone(), rule.clone()))
            .collect();
        PolicyEvaluation::allow(rule, decisions)
    }
}
