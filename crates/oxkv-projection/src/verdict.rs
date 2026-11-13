//! One enforcement verdict.
//!
//! A verdict names the policy version, projection epoch, per-source
//! watermarks, fact versions read, rule identity, and decision. A stored
//! verdict replays offline against the fact log.

use oxkv_contracts::version::{
    FactVersion, Hash32, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
};

use crate::epoch::ProjectionEpochState;
use crate::fact::FactId;

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
}

/// The decision of one rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decision {
    /// The rule allows the write.
    Allow,
    /// The rule denies the write.
    Deny,
}

impl Decision {
    /// Returns the canonical decision name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

/// One fact version that a rule read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FactRead {
    fact_id: FactId,
    fact_version: FactVersion,
}

impl FactRead {
    /// Creates a fact read.
    #[must_use]
    pub const fn new(fact_id: FactId, fact_version: FactVersion) -> Self {
        Self {
            fact_id,
            fact_version,
        }
    }

    /// Returns the fact identity.
    #[must_use]
    pub const fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    /// Returns the fact version.
    #[must_use]
    pub const fn fact_version(&self) -> FactVersion {
        self.fact_version
    }
}

/// One write enforcement verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    policy_version: PolicyVersion,
    projection_epoch: ProjectionEpoch,
    watermarks: ProjectionWatermarks,
    facts_read: Vec<FactRead>,
    rule: RuleId,
    decision: Decision,
    reason: Option<&'static str>,
    fact_set_digest: Hash32,
}

impl Verdict {
    /// Creates a verdict.
    #[must_use]
    pub fn new(
        epoch: ProjectionEpochState,
        watermarks: ProjectionWatermarks,
        facts_read: Vec<FactRead>,
        rule: RuleId,
        decision: Decision,
        reason: Option<&'static str>,
        fact_set_digest: Hash32,
    ) -> Self {
        Self {
            policy_version: epoch.policy_version(),
            projection_epoch: epoch.generation(),
            watermarks,
            facts_read,
            rule,
            decision,
            reason,
            fact_set_digest,
        }
    }

    /// Returns the policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the projection epoch generation.
    #[must_use]
    pub const fn projection_epoch(&self) -> ProjectionEpoch {
        self.projection_epoch
    }

    /// Returns the per-source watermarks.
    #[must_use]
    pub const fn watermarks(&self) -> &ProjectionWatermarks {
        &self.watermarks
    }

    /// Returns the fact versions read.
    #[must_use]
    pub fn facts_read(&self) -> &[FactRead] {
        &self.facts_read
    }

    /// Returns the rule identity.
    #[must_use]
    pub const fn rule(&self) -> &RuleId {
        &self.rule
    }

    /// Returns the decision.
    #[must_use]
    pub const fn decision(&self) -> Decision {
        self.decision
    }

    /// Returns the reason code.
    #[must_use]
    pub const fn reason(&self) -> Option<&'static str> {
        self.reason
    }

    /// Returns the fact set digest.
    #[must_use]
    pub const fn fact_set_digest(&self) -> Hash32 {
        self.fact_set_digest
    }
}
