//! The cost cursor and its expiry rules.
//!
//! A [`oxkv_contracts::CostCursor`] holds one frontier per shard and the pinned
//! version set. The extension trait adds accounting and expiry. Expiry returns
//! `query.budget_exceeded` or `search.pin_expired` as appropriate.

use std::collections::BTreeMap;

use oxkv_contracts::{CommitVersion, ScopeVersionVector, ShardId};
pub use oxkv_contracts::{CostCursor, CostFrontier};

use crate::budget::CostBudget;
use crate::error::{CostCode, CostError};
use crate::util::len_to_u64;

/// The reason that a cost cursor can no longer serve.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CostCursorExpiry {
    /// The cursor has no query budget.
    #[error("The cost cursor has no query budget. Remaining {remaining_budget}.")]
    BudgetExhausted {
        /// The remaining budget in milliunits.
        remaining_budget: u64,
    },
    /// One pinned shard version is below the oldest readable version.
    #[error(
        "The shard {shard_id:?} expired its pin. Requested {requested}, oldest readable {oldest_readable}."
    )]
    PinExpired {
        /// The shard with the expired pin.
        shard_id: ShardId,
        /// The pinned commit version.
        requested: CommitVersion,
        /// The oldest readable commit version.
        oldest_readable: CommitVersion,
    },
}

impl CostCursorExpiry {
    /// Returns the frozen error code.
    #[must_use]
    pub const fn code(&self) -> CostCode {
        match self {
            Self::BudgetExhausted { .. } => CostCode::QueryBudgetExceeded,
            Self::PinExpired { .. } => CostCode::SearchPinExpired,
        }
    }
}

/// Cost accounting and expiry for one cursor.
pub trait CostCursorExt {
    /// Creates one cursor from a pin and a budget.
    #[must_use]
    fn from_budget(pin: &ScopeVersionVector, budget: &CostBudget) -> Self;

    /// Returns one shard frontier. An absent shard has a zero frontier.
    #[must_use]
    fn frontier(&self, shard_id: &ShardId) -> CostFrontier;

    /// Returns the cursor expiry for one oldest readable vector.
    ///
    /// Budget exhaustion has precedence over pin expiry.
    ///
    /// # Errors
    ///
    /// Returns [`CostCursorExpiry::BudgetExhausted`] when no budget remains.
    /// Returns [`CostCursorExpiry::PinExpired`] when one pinned version is
    /// below the oldest readable version.
    fn expiry(&self, oldest_readable: &ScopeVersionVector) -> Result<(), CostCursorExpiry>;

    /// Spends query budget.
    ///
    /// # Errors
    ///
    /// Returns [`CostError::BudgetExceeded`] when the cost is above the
    /// remaining budget.
    fn spend(&mut self, needed_milli: u64) -> Result<(), CostError>;

    /// Adds examined keys to one shard frontier.
    ///
    /// # Errors
    ///
    /// Returns [`CostError::CounterOverflow`] when the frontier overflows.
    fn advance_frontier(
        &mut self,
        shard_id: ShardId,
        additional_keys: u64,
    ) -> Result<(), CostError>;

    /// Returns a new cursor after one page.
    ///
    /// The method adds the page frontiers and spends `1000 * shards + keys`.
    ///
    /// # Errors
    ///
    /// Returns [`CostError::BudgetExceeded`] when the page is above the
    /// remaining budget. Returns [`CostError::CounterOverflow`] when a
    /// frontier overflows.
    fn after_page(
        &self,
        frontiers: &BTreeMap<ShardId, CostFrontier>,
    ) -> Result<CostCursor, CostError>;
}

impl CostCursorExt for CostCursor {
    fn from_budget(pin: &ScopeVersionVector, budget: &CostBudget) -> Self {
        Self::new(BTreeMap::new(), pin.clone(), budget.max_cost_milli)
    }

    fn frontier(&self, shard_id: &ShardId) -> CostFrontier {
        self.frontiers
            .get(shard_id)
            .copied()
            .unwrap_or_else(|| CostFrontier::new(0))
    }

    fn expiry(&self, oldest_readable: &ScopeVersionVector) -> Result<(), CostCursorExpiry> {
        if self.remaining_budget == 0 {
            return Err(CostCursorExpiry::BudgetExhausted {
                remaining_budget: self.remaining_budget,
            });
        }
        for (shard_id, requested) in self.versions.as_vector().iter() {
            let Some(oldest) = oldest_readable.get(shard_id) else {
                continue;
            };
            if requested < oldest {
                return Err(CostCursorExpiry::PinExpired {
                    shard_id: shard_id.clone(),
                    requested,
                    oldest_readable: oldest,
                });
            }
        }
        Ok(())
    }

    fn spend(&mut self, needed_milli: u64) -> Result<(), CostError> {
        if needed_milli > self.remaining_budget {
            return Err(CostError::BudgetExceeded {
                needed_milli,
                budget_milli: self.remaining_budget,
            });
        }
        self.remaining_budget = self.remaining_budget.saturating_sub(needed_milli);
        Ok(())
    }

    fn advance_frontier(
        &mut self,
        shard_id: ShardId,
        additional_keys: u64,
    ) -> Result<(), CostError> {
        let frontier = self
            .frontiers
            .entry(shard_id)
            .or_insert_with(|| CostFrontier::new(0));
        frontier.keys_examined = frontier.keys_examined.checked_add(additional_keys).ok_or(
            CostError::CounterOverflow {
                field: "frontier.keys_examined",
            },
        )?;
        Ok(())
    }

    fn after_page(
        &self,
        frontiers: &BTreeMap<ShardId, CostFrontier>,
    ) -> Result<CostCursor, CostError> {
        let shards = len_to_u64(frontiers.len());
        let keys = frontiers.values().fold(0_u64, |total, frontier| {
            total.saturating_add(frontier.keys_examined)
        });
        let needed_milli = 1000_u64.saturating_mul(shards).saturating_add(keys);
        let mut next = self.clone();
        next.spend(needed_milli)?;
        for (shard_id, frontier) in frontiers {
            next.advance_frontier(shard_id.clone(), frontier.keys_examined)?;
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::VersionVector;

    fn vector(entries: &[(&[u8], u64)]) -> ScopeVersionVector {
        ScopeVersionVector::from_vector(VersionVector::from_entries(
            entries.iter().map(|(shard, version)| {
                (ShardId::new(shard.to_vec()), CommitVersion::new(*version))
            }),
        ))
    }

    fn budget() -> CostBudget {
        CostBudget::new(5000, 2, 10)
    }

    #[test]
    fn a_cursor_from_a_budget_starts_with_the_full_budget() {
        let cursor = CostCursor::from_budget(&vector(&[(&[1], 5)]), &budget());
        assert_eq!(cursor.remaining_budget, 5000);
        assert!(cursor.frontiers.is_empty());
        assert_eq!(
            cursor.versions.get(&ShardId::new(vec![1])),
            Some(CommitVersion::new(5))
        );
    }

    #[test]
    fn spend_reduces_the_remaining_budget() {
        let mut cursor = CostCursor::from_budget(&vector(&[]), &budget());
        cursor.spend(1200).expect("the budget covers the cost");
        assert_eq!(cursor.remaining_budget, 3800);
        let error = cursor.spend(4000).expect_err("the budget is too small");
        assert_eq!(error.code(), CostCode::QueryBudgetExceeded);
    }

    #[test]
    fn after_page_adds_frontiers_and_spends_the_page_cost() {
        let cursor = CostCursor::from_budget(&vector(&[(&[1], 5)]), &budget());
        let mut frontiers = BTreeMap::new();
        frontiers.insert(ShardId::new(vec![1]), CostFrontier::new(10));
        frontiers.insert(ShardId::new(vec![2]), CostFrontier::new(4));
        let next = cursor.after_page(&frontiers).expect("the page fits");
        assert_eq!(next.remaining_budget, 5000 - 2014);
        assert_eq!(next.frontier(&ShardId::new(vec![1])).keys_examined, 10);
        assert_eq!(next.frontier(&ShardId::new(vec![2])).keys_examined, 4);
    }

    #[test]
    fn budget_exhaustion_has_precedence_over_pin_expiry() {
        let mut cursor = CostCursor::from_budget(&vector(&[(&[1], 5)]), &budget());
        cursor.remaining_budget = 0;
        let expiry = cursor
            .expiry(&vector(&[(&[1], 6)]))
            .expect_err("the cursor has no budget");
        assert_eq!(expiry.code(), CostCode::QueryBudgetExceeded);
    }

    #[test]
    fn a_pinned_version_below_the_oldest_readable_version_expires() {
        let cursor = CostCursor::from_budget(&vector(&[(&[1], 5)]), &budget());
        let expiry = cursor
            .expiry(&vector(&[(&[1], 6)]))
            .expect_err("the pin is too old");
        assert_eq!(expiry.code(), CostCode::SearchPinExpired);
        match expiry {
            CostCursorExpiry::PinExpired {
                shard_id,
                requested,
                oldest_readable,
            } => {
                assert_eq!(shard_id, ShardId::new(vec![1]));
                assert_eq!(requested, CommitVersion::new(5));
                assert_eq!(oldest_readable, CommitVersion::new(6));
            }
            other @ CostCursorExpiry::BudgetExhausted { .. } => {
                panic!("wrong expiry: {other:?}")
            }
        }
    }

    #[test]
    fn a_readable_pin_and_a_nonzero_budget_do_not_expire() {
        let cursor = CostCursor::from_budget(&vector(&[(&[1], 6)]), &budget());
        assert!(cursor.expiry(&vector(&[(&[1], 6)])).is_ok());
        assert!(cursor.expiry(&vector(&[(&[1], 4)])).is_ok());
        assert!(cursor.expiry(&vector(&[(&[2], 99)])).is_ok());
    }

    #[test]
    fn a_frontier_overflow_is_rejected() {
        let mut cursor = CostCursor::from_budget(&vector(&[]), &budget());
        cursor
            .advance_frontier(ShardId::new(vec![1]), u64::MAX)
            .expect("the first keys fit");
        let error = cursor
            .advance_frontier(ShardId::new(vec![1]), 1)
            .expect_err("the frontier overflows");
        assert_eq!(error.code(), CostCode::InternalInvariantViolation);
    }
}
