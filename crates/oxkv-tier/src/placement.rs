//! Deterministic placement policy.
//!
//! A policy is versioned data. It reads no clock, no randomness, and no arrival
//! order. The same facts and the same policy produce the same placement on
//! native and wasm32.
//!
//! A plan that contradicts the policy returns
//! [`TierError::PlacementViolation`].

use oxkv_contracts::version::ShardId;

use crate::error::TierError;
use crate::tier::{ArtifactId, Tier};

/// The durability class of one placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DurabilityClass {
    /// The tier can lose the bytes on restart.
    Volatile,
    /// The local disk holds the bytes.
    DurableLocal,
    /// The object store holds the bytes.
    DurableObject,
}

impl DurabilityClass {
    /// Returns the lowercase durability name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Volatile => "volatile",
            Self::DurableLocal => "durable-local",
            Self::DurableObject => "durable-object",
        }
    }
}

/// The facts that a placement rule reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementFacts {
    /// The shard identity.
    pub shard_id: ShardId,
    /// The segment version.
    pub segment_version: u64,
    /// The current shard commit version.
    pub shard_version: u64,
    /// The declared size class of the segment.
    pub size_class: u8,
    /// The observed access count bucket.
    pub access_bucket: u8,
    /// True when a reader pinned the segment.
    pub pinned: bool,
}

impl PlacementFacts {
    /// Creates placement facts.
    #[must_use]
    pub const fn new(
        shard_id: ShardId,
        segment_version: u64,
        shard_version: u64,
        size_class: u8,
        access_bucket: u8,
        pinned: bool,
    ) -> Self {
        Self {
            shard_id,
            segment_version,
            shard_version,
            size_class,
            access_bucket,
            pinned,
        }
    }

    /// Returns the distance between the segment version and the shard version.
    #[must_use]
    pub const fn version_distance(&self) -> u64 {
        self.shard_version.saturating_sub(self.segment_version)
    }
}

/// One rule in a placement policy.
///
/// A rule matches when every declared bound matches. The policy evaluates rules
/// in declaration order and uses the first match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementRule {
    tier: Tier,
    durability: DurabilityClass,
    max_version_distance: Option<u64>,
    min_access_bucket: Option<u8>,
    max_size_class: Option<u8>,
    pinned: Option<bool>,
}

impl PlacementRule {
    /// Creates an unbounded rule for one tier.
    #[must_use]
    pub const fn new(tier: Tier, durability: DurabilityClass) -> Self {
        Self {
            tier,
            durability,
            max_version_distance: None,
            min_access_bucket: None,
            max_size_class: None,
            pinned: None,
        }
    }

    /// Sets the maximum version distance.
    #[must_use]
    pub const fn with_max_version_distance(mut self, distance: u64) -> Self {
        self.max_version_distance = Some(distance);
        self
    }

    /// Sets the minimum access count bucket.
    #[must_use]
    pub const fn with_min_access_bucket(mut self, bucket: u8) -> Self {
        self.min_access_bucket = Some(bucket);
        self
    }

    /// Sets the maximum size class.
    #[must_use]
    pub const fn with_max_size_class(mut self, size_class: u8) -> Self {
        self.max_size_class = Some(size_class);
        self
    }

    /// Sets the required pin state.
    #[must_use]
    pub const fn with_pinned(mut self, pinned: bool) -> Self {
        self.pinned = Some(pinned);
        self
    }

    /// Returns the target tier.
    #[must_use]
    pub const fn tier(self) -> Tier {
        self.tier
    }

    /// Returns the durability class.
    #[must_use]
    pub const fn durability(self) -> DurabilityClass {
        self.durability
    }

    /// Returns true when every declared bound matches the facts.
    #[must_use]
    pub fn matches(self, facts: &PlacementFacts) -> bool {
        self.max_version_distance
            .is_none_or(|bound| facts.version_distance() <= bound)
            && self
                .min_access_bucket
                .is_none_or(|bound| facts.access_bucket >= bound)
            && self
                .max_size_class
                .is_none_or(|bound| facts.size_class <= bound)
            && self.pinned.is_none_or(|bound| facts.pinned == bound)
    }
}

/// The result of a placement decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The chosen tier.
    pub tier: Tier,
    /// The chosen durability class.
    pub durability: DurabilityClass,
}

impl Placement {
    /// Creates a placement decision.
    #[must_use]
    pub const fn new(tier: Tier, durability: DurabilityClass) -> Self {
        Self { tier, durability }
    }
}

/// A proposed placement for one artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementPlan {
    /// The artifact identity.
    pub artifact: ArtifactId,
    /// The proposed tier.
    pub tier: Tier,
}

impl PlacementPlan {
    /// Creates a placement plan.
    #[must_use]
    pub const fn new(artifact: ArtifactId, tier: Tier) -> Self {
        Self { artifact, tier }
    }
}

/// A versioned placement policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementPolicy {
    policy_version: u64,
    rules: Vec<PlacementRule>,
}

impl PlacementPolicy {
    /// Creates a placement policy.
    #[must_use]
    pub const fn new(policy_version: u64, rules: Vec<PlacementRule>) -> Self {
        Self {
            policy_version,
            rules,
        }
    }

    /// Returns the policy version.
    #[must_use]
    pub const fn policy_version(&self) -> u64 {
        self.policy_version
    }

    /// Returns the rules in declaration order.
    #[must_use]
    pub fn rules(&self) -> &[PlacementRule] {
        &self.rules
    }

    /// Returns the first matching placement.
    #[must_use]
    pub fn evaluate(&self, facts: &PlacementFacts) -> Option<Placement> {
        self.rules
            .iter()
            .copied()
            .find(|rule| rule.matches(facts))
            .map(|rule| Placement::new(rule.tier(), rule.durability()))
    }

    /// Returns the placement or a named violation.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::PlacementViolation`] when no rule matches.
    pub fn evaluate_required(&self, facts: &PlacementFacts) -> Result<Placement, TierError> {
        self.evaluate(facts).ok_or(TierError::PlacementViolation)
    }

    /// Checks a plan against the policy.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::PlacementViolation`] when the plan tier differs
    /// from the policy verdict.
    pub fn validate_plan(
        &self,
        facts: &PlacementFacts,
        plan: &PlacementPlan,
    ) -> Result<(), TierError> {
        let placement = self.evaluate_required(facts)?;
        if placement.tier == plan.tier {
            Ok(())
        } else {
            Err(TierError::PlacementViolation)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DurabilityClass, PlacementFacts, PlacementPlan, PlacementPolicy, PlacementRule, ShardId,
        Tier, TierError,
    };

    fn facts() -> PlacementFacts {
        PlacementFacts::new(ShardId::new(vec![1]), 7, 9, 1, 5, false)
    }

    fn policy() -> PlacementPolicy {
        PlacementPolicy::new(
            3,
            vec![
                PlacementRule::new(Tier::Ram, DurabilityClass::Volatile)
                    .with_max_version_distance(2)
                    .with_min_access_bucket(4),
                PlacementRule::new(Tier::Disk, DurabilityClass::DurableLocal)
                    .with_max_size_class(3),
                PlacementRule::new(Tier::Object, DurabilityClass::DurableObject),
            ],
        )
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let placement = policy()
            .evaluate(&facts())
            .expect("a rule matches the facts");
        assert_eq!(placement.tier, Tier::Ram);
        assert_eq!(placement.durability, DurabilityClass::Volatile);
    }

    #[test]
    fn a_pinned_rule_matches_only_a_pinned_segment() {
        let policy = PlacementPolicy::new(
            1,
            vec![
                PlacementRule::new(Tier::Ram, DurabilityClass::Volatile).with_pinned(true),
                PlacementRule::new(Tier::Object, DurabilityClass::DurableObject),
            ],
        );
        assert_eq!(
            policy.evaluate(&facts()).map(|placement| placement.tier),
            Some(Tier::Object)
        );
    }

    #[test]
    fn a_contradicting_plan_is_a_named_violation() {
        let facts = facts();
        let plan = PlacementPlan::new(
            crate::tier::ArtifactId::new(
                facts.shard_id.clone(),
                facts.segment_version,
                oxkv_contracts::version::PageSequence::new(1),
            ),
            Tier::Object,
        );
        assert_eq!(
            policy().validate_plan(&facts, &plan),
            Err(TierError::PlacementViolation)
        );
    }

    #[test]
    fn a_policy_with_no_match_is_a_named_violation() {
        let policy = PlacementPolicy::new(1, vec![]);
        assert_eq!(
            policy.evaluate_required(&facts()),
            Err(TierError::PlacementViolation)
        );
    }
}
