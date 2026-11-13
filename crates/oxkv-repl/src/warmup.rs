//! Member warmup budgets.
//!
//! A new member catches up before it counts toward a quorum. The catch-up has
//! a budget. A member that exceeds the budget returns
//! `replication.warmup_budget_exceeded`. The caller then continues with a
//! cursor.

use crate::error::ReplError;

/// The warmup budget of one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WarmupBudget(u64);

impl WarmupBudget {
    /// Creates a warmup budget in caller-defined units.
    #[must_use]
    pub const fn new(units: u64) -> Self {
        Self(units)
    }

    /// Returns the budget in caller-defined units.
    #[must_use]
    pub const fn limit(self) -> u64 {
        self.0
    }

    /// Checks the used units against the budget.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::WarmupBudgetExceeded`] when `used` is above the
    /// budget.
    pub const fn check(self, used: u64) -> Result<(), ReplError> {
        if used > self.0 {
            return Err(ReplError::WarmupBudgetExceeded {
                budget: self.0,
                used,
            });
        }
        Ok(())
    }
}
