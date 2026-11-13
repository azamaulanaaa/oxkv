//! The step budget and its bounds.
//!
//! A budget is a count. It is not a deadline. One call inspects at most
//! `max_positions` queue positions and `max_edges` dependency edges. An
//! exhausted budget returns a partial decision.

use crate::state::SchedulerConfig;

/// The default maximum number of positions for one decision.
pub const DEFAULT_MAX_POSITIONS: usize = 8;

/// The default maximum number of dependency edges for one decision.
pub const DEFAULT_MAX_EDGES: usize = 64;

/// The work limits for one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SchedulerBudget {
    /// The maximum number of ready positions to inspect.
    pub max_positions: usize,
    /// The maximum number of dependency edges to inspect.
    pub max_edges: usize,
}

impl SchedulerBudget {
    /// Creates a budget.
    #[must_use]
    pub const fn new(max_positions: usize, max_edges: usize) -> Self {
        Self {
            max_positions,
            max_edges,
        }
    }

    /// Returns true when the budget can select every default fan-in in one
    /// full batch.
    #[must_use]
    pub const fn is_adequate_for(&self, config: SchedulerConfig) -> bool {
        self.max_positions > 0
            && self.max_edges >= self.max_positions.saturating_mul(config.max_fan_in)
    }
}

impl Default for SchedulerBudget {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_POSITIONS, DEFAULT_MAX_EDGES)
    }
}

/// The work that one decision used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BudgetUsage {
    /// The number of ready positions inspected.
    pub positions: usize,
    /// The number of dependency edges inspected.
    pub edges: usize,
}

/// Returns the starvation bound for a finite ready set.
///
/// The bound assumes an adequate edge budget, a finite ready set, and no new
/// arrivals. One call selects at most `max_positions` ready requests. A ready
/// request waits at most this many calls.
///
/// The function returns [`usize::MAX`] when `max_positions` is zero. A zero
/// position budget selects no request, so no finite bound exists.
#[must_use]
pub const fn starvation_bound(ready_count: usize, max_positions: usize) -> usize {
    if max_positions == 0 {
        usize::MAX
    } else {
        ready_count.div_ceil(max_positions)
    }
}
