//! Query budgets and per-shard frontiers.
//!
//! A query charges one cost per examined hit. A charge above the budget fails
//! with [`SearchError::QueryBudgetExceeded`]. The budget keeps one frontier per
//! shard for the cost crate.

use std::collections::BTreeMap;

use oxkv_contracts::version::ShardId;

use crate::error::SearchError;

/// A query budget with one frontier per shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryBudget {
    limit: u64,
    used: u64,
    frontiers: BTreeMap<ShardId, u64>,
}

impl QueryBudget {
    /// Creates a query budget.
    #[must_use]
    pub const fn new(limit: u64) -> Self {
        Self {
            limit,
            used: 0,
            frontiers: BTreeMap::new(),
        }
    }

    /// Creates a query budget with no practical limit.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self::new(u64::MAX)
    }

    /// Returns the query limit.
    #[must_use]
    pub const fn limit(&self) -> u64 {
        self.limit
    }

    /// Returns the used cost.
    #[must_use]
    pub const fn used(&self) -> u64 {
        self.used
    }

    /// Returns the remaining cost.
    #[must_use]
    pub const fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    /// Returns the frontier for one shard.
    #[must_use]
    pub fn frontier(&self, shard_id: &ShardId) -> Option<u64> {
        self.frontiers.get(shard_id).copied()
    }

    /// Iterates over the per-shard frontiers in shard identifier order.
    pub fn frontiers(&self) -> impl Iterator<Item = (&ShardId, u64)> {
        self.frontiers
            .iter()
            .map(|(shard_id, keys_examined)| (shard_id, *keys_examined))
    }

    /// Charges a cost to one shard frontier.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::QueryBudgetExceeded`] when the charge passes the
    /// limit. The budget does not change.
    pub fn charge(&mut self, shard_id: &ShardId, cost: u64) -> Result<(), SearchError> {
        let required = self.used.saturating_add(cost);
        if required > self.limit {
            return Err(SearchError::QueryBudgetExceeded {
                required,
                limit: self.limit,
                coverage: None,
            });
        }
        self.used = required;
        let frontier = self.frontiers.entry(shard_id.clone()).or_insert(0);
        *frontier = frontier.saturating_add(cost);
        Ok(())
    }
}
