//! The bounded query cost budget.
//!
//! Admission checks the fan-out limit before the cost limit. The fan-out check
//! runs before the first shard request. A rejected plan carries the named
//! error codes in check order.

use core::fmt;

use crate::error::{CostCode, CostError};
use crate::plan::QueryPlan;
use crate::util::u32_to_usize;

/// The limits that apply to one query.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CostBudget {
    /// The maximum query cost in milliunits.
    pub max_cost_milli: u64,
    /// The maximum shard count.
    pub max_fanout: u32,
    /// The maximum token count.
    pub max_tokens: u64,
}

impl CostBudget {
    /// Creates one budget.
    #[must_use]
    pub const fn new(max_cost_milli: u64, max_fanout: u32, max_tokens: u64) -> Self {
        Self {
            max_cost_milli,
            max_fanout,
            max_tokens,
        }
    }

    /// Returns the fan-out limit as a `usize`.
    #[must_use]
    pub fn fanout_limit(&self) -> usize {
        u32_to_usize(self.max_fanout)
    }

    /// Evaluates one plan against the budget.
    ///
    /// The method returns every named error. The order is fan-out, token, then
    /// cost.
    #[must_use]
    pub fn evaluate(&self, plan: &QueryPlan) -> Admission {
        let mut errors = Vec::new();
        if plan.fanout() > self.max_fanout {
            errors.push(CostError::FanoutExceeded {
                requested: plan.fanout(),
                limit: self.max_fanout,
            });
        }
        let tokens = plan.total_tokens();
        if tokens > plan.token_budget() {
            errors.push(CostError::TokenBudgetExceeded {
                needed: tokens,
                budget: plan.token_budget(),
            });
        } else if plan.token_budget() > self.max_tokens {
            errors.push(CostError::TokenBudgetExceeded {
                needed: plan.token_budget(),
                budget: self.max_tokens,
            });
        } else if tokens > self.max_tokens {
            errors.push(CostError::TokenBudgetExceeded {
                needed: tokens,
                budget: self.max_tokens,
            });
        }
        let cost = plan.total_cost_milli();
        if cost > self.max_cost_milli {
            errors.push(CostError::BudgetExceeded {
                needed_milli: cost,
                budget_milli: self.max_cost_milli,
            });
        }
        let verdict = if errors.is_empty() {
            Verdict::Admitted
        } else {
            Verdict::Rejected
        };
        Admission {
            planned_cost_milli: cost,
            remaining_cost_milli: self.max_cost_milli.saturating_sub(cost),
            verdict,
            errors,
        }
    }

    /// Evaluates one plan and returns the first error.
    ///
    /// # Errors
    ///
    /// Returns the first error from [`CostBudget::evaluate`] when the plan is
    /// over a limit.
    pub fn admit(&self, plan: &QueryPlan) -> Result<Admission, CostError> {
        let admission = self.evaluate(plan);
        match admission.errors.first() {
            Some(error) => Err(error.clone()),
            None => Ok(admission),
        }
    }
}

/// The verdict of one budget check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// The query fits the budget.
    Admitted,
    /// The query is over one limit.
    Rejected,
}

impl Verdict {
    /// Returns the stable contract text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Rejected => "rejected",
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The result of one budget check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    /// The planned cost in milliunits.
    pub planned_cost_milli: u64,
    /// The cost that remains after the plan.
    pub remaining_cost_milli: u64,
    /// The verdict.
    pub verdict: Verdict,
    /// The named errors in check order.
    pub errors: Vec<CostError>,
}

impl Admission {
    /// Returns true when the plan is admitted.
    #[must_use]
    pub fn is_admitted(&self) -> bool {
        self.verdict == Verdict::Admitted
    }

    /// Returns the error codes in check order.
    pub fn error_codes(&self) -> impl Iterator<Item = CostCode> + '_ {
        self.errors.iter().map(CostError::code)
    }

    /// Returns the first error.
    #[must_use]
    pub fn first_error(&self) -> Option<&CostError> {
        self.errors.first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{CostInputs, MergeShape, ShardPlan};
    use oxkv_contracts::{ManifestRevision, MapVersion, PolicyVersion, ShardId, StatisticsVersion};

    fn plan(shards: Vec<ShardPlan>, token_budget: u64) -> QueryPlan {
        QueryPlan::new(
            CostInputs::new(
                ManifestRevision::new(1),
                PolicyVersion::from_bytes([0; 32]),
                MapVersion::new(1),
                Some(StatisticsVersion::new(1)),
            ),
            "term",
            MergeShape::Search,
            shards,
            token_budget,
        )
        .expect("the plan is valid")
    }

    fn shard(id: u8, keys: u64, tokens: u64) -> ShardPlan {
        ShardPlan::new(ShardId::new(vec![id]), 0, keys, 1, tokens)
    }

    #[test]
    fn a_plan_within_every_limit_is_admitted() {
        let plan = plan(vec![shard(1, 100, 10)], 20);
        let budget = CostBudget::new(2000, 2, 100);
        let admission = budget.evaluate(&plan);
        assert!(admission.is_admitted());
        assert_eq!(admission.planned_cost_milli, 1100);
        assert_eq!(admission.remaining_cost_milli, 900);
        assert_eq!(admission.errors, Vec::new());
        assert!(budget.admit(&plan).is_ok());
    }

    #[test]
    fn a_fanout_over_the_limit_is_rejected_before_cost() {
        let plan = plan(
            vec![shard(1, 5000, 1), shard(2, 5000, 1), shard(3, 1, 1)],
            10,
        );
        let budget = CostBudget::new(1, 2, 100);
        let error = budget
            .admit(&plan)
            .expect_err("the fan-out is over the limit");
        assert_eq!(error.code(), CostCode::SearchFanoutExceeded);
        let admission = budget.evaluate(&plan);
        assert_eq!(admission.verdict, Verdict::Rejected);
        assert_eq!(admission.errors[0].code(), CostCode::SearchFanoutExceeded);
    }

    #[test]
    fn a_cost_over_the_limit_is_rejected() {
        let plan = plan(vec![shard(1, 1000, 1)], 10);
        let budget = CostBudget::new(1500, 2, 100);
        let error = budget.admit(&plan).expect_err("the cost is over the limit");
        assert_eq!(error.code(), CostCode::QueryBudgetExceeded);
    }

    #[test]
    fn a_token_count_over_the_limit_is_rejected() {
        let plan = plan(vec![shard(1, 1, 50)], 20);
        let budget = CostBudget::new(10_000, 2, 100);
        let error = budget
            .admit(&plan)
            .expect_err("the token count is over the limit");
        assert_eq!(error.code(), CostCode::SearchTokenBudgetExceeded);
    }

    #[test]
    fn the_error_codes_follow_the_check_order() {
        let plan = plan(vec![shard(1, 5000, 50), shard(2, 1, 1), shard(3, 1, 1)], 20);
        let budget = CostBudget::new(100, 1, 10);
        let admission = budget.evaluate(&plan);
        assert!(!admission.is_admitted());
        let codes: Vec<CostCode> = admission.error_codes().collect();
        assert_eq!(
            codes,
            vec![
                CostCode::SearchFanoutExceeded,
                CostCode::SearchTokenBudgetExceeded,
                CostCode::QueryBudgetExceeded
            ]
        );
    }
}
