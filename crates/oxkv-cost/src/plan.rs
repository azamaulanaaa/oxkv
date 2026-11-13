//! The query plan and its rule-based cost.
//!
//! A plan has one entry per targeted shard. The cost unit combines machine
//! count and key count. One shard costs 1000 milliunits. One examined key costs
//! 1 milliunit. The rule-based plan never under-reports either number.

use core::fmt;

use oxkv_contracts::{ManifestRevision, MapVersion, PolicyVersion, ShardId, StatisticsVersion};

use crate::error::CostError;
use crate::util::{len_to_u32, len_to_u64};

/// The cost and explain contract version.
pub const COST_CONTRACT_VERSION: &str = "cost/v1";

/// The shard fan-out shape of one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergeShape {
    /// One shard answers the query.
    SingleShard,
    /// Shards merge under the search order.
    Search,
    /// Shards merge under the general query order.
    General,
}

impl MergeShape {
    /// Every merge shape.
    pub const ALL: &'static [Self] = &[Self::SingleShard, Self::Search, Self::General];

    /// Returns the stable contract text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SingleShard => "single_shard",
            Self::Search => "search",
            Self::General => "general",
        }
    }
}

impl fmt::Display for MergeShape {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The versions that produced one cost estimate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CostInputs {
    /// The manifest revision.
    pub manifest_revision: ManifestRevision,
    /// The policy version.
    pub policy_version: PolicyVersion,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The search statistics version. It is optional in 1.0.
    pub statistics_version: Option<StatisticsVersion>,
}

impl CostInputs {
    /// Creates one cost input tuple.
    #[must_use]
    pub const fn new(
        manifest_revision: ManifestRevision,
        policy_version: PolicyVersion,
        map_version: MapVersion,
        statistics_version: Option<StatisticsVersion>,
    ) -> Self {
        Self {
            manifest_revision,
            policy_version,
            map_version,
            statistics_version,
        }
    }
}

/// The estimated work of one shard.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShardPlan {
    /// The shard identifier.
    pub shard_id: ShardId,
    /// The number of term probes.
    pub term_probes: u64,
    /// The number of posting keys that the probes may read.
    pub posting_keys: u64,
    /// The estimated row count.
    pub estimated_rows: u64,
    /// The number of query tokens.
    pub token_count: u64,
}

impl ShardPlan {
    /// Creates one shard plan.
    #[must_use]
    pub const fn new(
        shard_id: ShardId,
        term_probes: u64,
        posting_keys: u64,
        estimated_rows: u64,
        token_count: u64,
    ) -> Self {
        Self {
            shard_id,
            term_probes,
            posting_keys,
            estimated_rows,
            token_count,
        }
    }

    /// Returns the worst-case key count. A term probe costs one key.
    #[must_use]
    pub const fn keys_examined(&self) -> u64 {
        self.term_probes.saturating_add(self.posting_keys)
    }

    /// Returns the shard cost in milliunits.
    #[must_use]
    pub const fn cost_milli(&self) -> u64 {
        1000_u64.saturating_add(self.keys_examined())
    }
}

/// A canonical query plan.
///
/// The constructor sorts the shard entries by shard identifier. A duplicate
/// shard entry is an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPlan {
    inputs: CostInputs,
    policy_normalization: String,
    merge_shape: MergeShape,
    shards: Vec<ShardPlan>,
    token_budget: u64,
}

impl QueryPlan {
    /// Creates one canonical query plan.
    ///
    /// # Errors
    ///
    /// Returns [`CostError::DuplicateShard`] when one shard has two entries.
    pub fn new(
        inputs: CostInputs,
        policy_normalization: impl Into<String>,
        merge_shape: MergeShape,
        mut shards: Vec<ShardPlan>,
        token_budget: u64,
    ) -> Result<Self, CostError> {
        shards.sort_by(|left, right| left.shard_id.cmp(&right.shard_id));
        if let Some(pair) = shards
            .windows(2)
            .find(|pair| pair[0].shard_id == pair[1].shard_id)
        {
            return Err(CostError::DuplicateShard {
                shard_id: pair[1].shard_id.clone(),
            });
        }
        Ok(Self {
            inputs,
            policy_normalization: policy_normalization.into(),
            merge_shape,
            shards,
            token_budget,
        })
    }

    /// Returns the cost inputs.
    #[must_use]
    pub const fn inputs(&self) -> &CostInputs {
        &self.inputs
    }

    /// Returns the normalized policy text.
    #[must_use]
    pub fn policy_normalization(&self) -> &str {
        &self.policy_normalization
    }

    /// Returns the merge shape.
    #[must_use]
    pub const fn merge_shape(&self) -> MergeShape {
        self.merge_shape
    }

    /// Returns the per-shard plans in shard identifier order.
    #[must_use]
    pub fn shards(&self) -> &[ShardPlan] {
        &self.shards
    }

    /// Returns the shard identifiers in shard identifier order.
    pub fn shard_ids(&self) -> impl Iterator<Item = &ShardId> {
        self.shards.iter().map(|shard| &shard.shard_id)
    }

    /// Returns the declared token budget.
    #[must_use]
    pub const fn token_budget(&self) -> u64 {
        self.token_budget
    }

    /// Returns the shard count.
    #[must_use]
    pub fn fanout(&self) -> u32 {
        len_to_u32(self.shards.len())
    }

    /// Returns the sum of the estimated row counts.
    #[must_use]
    pub fn estimated_rows(&self) -> u64 {
        self.shards.iter().fold(0_u64, |total, shard| {
            total.saturating_add(shard.estimated_rows)
        })
    }

    /// Returns the sum of the token counts.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.shards.iter().fold(0_u64, |total, shard| {
            total.saturating_add(shard.token_count)
        })
    }

    /// Returns the sum of the examined key counts.
    #[must_use]
    pub fn total_keys(&self) -> u64 {
        self.shards.iter().fold(0_u64, |total, shard| {
            total.saturating_add(shard.keys_examined())
        })
    }

    /// Returns the total cost in milliunits.
    ///
    /// The value is `1000 * shards + keys_examined`.
    #[must_use]
    pub fn total_cost_milli(&self) -> u64 {
        1000_u64
            .saturating_mul(u64::from(self.fanout()))
            .saturating_add(self.total_keys())
    }

    /// Returns the number of shard entries as `u64`.
    #[must_use]
    pub fn shard_count(&self) -> u64 {
        len_to_u64(self.shards.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::{ManifestRevision, MapVersion, PolicyVersion};

    fn inputs() -> CostInputs {
        CostInputs::new(
            ManifestRevision::new(7),
            PolicyVersion::from_bytes([1; 32]),
            MapVersion::new(3),
            Some(StatisticsVersion::new(2)),
        )
    }

    fn shard(id: u8, probes: u64, keys: u64) -> ShardPlan {
        ShardPlan::new(ShardId::new(vec![id]), probes, keys, 1, 2)
    }

    #[test]
    fn a_single_shard_point_lookup_costs_one_cost_unit() {
        let plan = QueryPlan::new(
            inputs(),
            "table == accounts",
            MergeShape::SingleShard,
            vec![ShardPlan::new(ShardId::new(vec![1]), 0, 0, 1, 1)],
            10,
        )
        .expect("the plan is valid");
        assert_eq!(plan.fanout(), 1);
        assert_eq!(plan.total_keys(), 0);
        assert_eq!(plan.total_cost_milli(), 1000);
    }

    #[test]
    fn a_three_shard_search_adds_shards_and_keys() {
        let plan = QueryPlan::new(
            inputs(),
            "term",
            MergeShape::Search,
            vec![shard(3, 0, 100), shard(1, 0, 100), shard(2, 0, 200)],
            10,
        )
        .expect("the plan is valid");
        assert_eq!(plan.fanout(), 3);
        assert_eq!(plan.total_keys(), 400);
        assert_eq!(plan.total_cost_milli(), 3400);
        let ids: Vec<&ShardId> = plan.shard_ids().collect();
        assert_eq!(ids[0], &ShardId::new(vec![1]));
        assert_eq!(ids[2], &ShardId::new(vec![3]));
    }

    #[test]
    fn a_term_probe_counts_as_one_examined_key() {
        let shard = ShardPlan::new(ShardId::new(vec![1]), 2, 5, 1, 1);
        assert_eq!(shard.keys_examined(), 7);
        assert_eq!(shard.cost_milli(), 1007);
    }

    #[test]
    fn a_duplicate_shard_entry_is_rejected() {
        let error = QueryPlan::new(
            inputs(),
            "term",
            MergeShape::Search,
            vec![shard(1, 0, 1), shard(1, 0, 2)],
            10,
        )
        .expect_err("the plan has a duplicate shard");
        assert_eq!(error.code().as_str(), "query.duplicate_shard");
    }
}
