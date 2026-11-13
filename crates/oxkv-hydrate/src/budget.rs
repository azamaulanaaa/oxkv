//! The hydration peak budget.
//!
//! Hydration holds the served generation and the new generation at the same
//! time. The peak is part of the device budget. A scope that cannot fit the
//! peak fails before the replica transfers a page.

use crate::error::HydrationError;
use crate::snapshot::SnapshotDescriptor;

/// One fixed journal overhead in bytes.
const JOURNAL_BASE_BYTES: u64 = 4096;

/// One directory entry overhead in bytes.
const JOURNAL_ENTRY_BYTES: u64 = 128;

/// The device budget for one replica and the hydration overhead limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeBudget {
    /// The total device bytes.
    pub device_budget_bytes: u64,
    /// The allowed hydration overhead bytes.
    pub max_hydration_overhead_bytes: u64,
}

impl ScopeBudget {
    /// Creates one scope budget.
    #[must_use]
    pub const fn new(device_budget_bytes: u64, max_hydration_overhead_bytes: u64) -> Self {
        Self {
            device_budget_bytes,
            max_hydration_overhead_bytes,
        }
    }

    /// Creates a budget with no limit.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            device_budget_bytes: u64::MAX,
            max_hydration_overhead_bytes: u64::MAX,
        }
    }

    /// Plans one hydration and checks the peak.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::HydrationBudgetExceeded`] when the peak
    /// exceeds the device budget or the hydration overhead limit.
    pub fn plan(
        self,
        descriptor: &SnapshotDescriptor,
        served_bytes: u64,
    ) -> Result<BudgetPlan, HydrationError> {
        let journal_bytes = JOURNAL_BASE_BYTES
            .saturating_add(descriptor.page_count.saturating_mul(JOURNAL_ENTRY_BYTES));
        let overhead_bytes = descriptor.snapshot_bytes.saturating_add(journal_bytes);
        let planned_peak_bytes = served_bytes.saturating_add(overhead_bytes);
        if planned_peak_bytes > self.device_budget_bytes {
            return Err(HydrationError::HydrationBudgetExceeded {
                planned_peak_bytes,
                budget_bytes: self.device_budget_bytes,
            });
        }
        if overhead_bytes > self.max_hydration_overhead_bytes {
            return Err(HydrationError::HydrationBudgetExceeded {
                planned_peak_bytes: overhead_bytes,
                budget_bytes: self.max_hydration_overhead_bytes,
            });
        }
        Ok(BudgetPlan {
            served_bytes,
            snapshot_bytes: descriptor.snapshot_bytes,
            journal_bytes,
            overhead_bytes,
            planned_peak_bytes,
        })
    }
}

impl Default for ScopeBudget {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// One accepted budget plan.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetPlan {
    /// The bytes of the served generation.
    pub served_bytes: u64,
    /// The logical snapshot bytes.
    pub snapshot_bytes: u64,
    /// The journal bytes.
    pub journal_bytes: u64,
    /// The hydration overhead bytes.
    pub overhead_bytes: u64,
    /// The planned peak bytes.
    pub planned_peak_bytes: u64,
}
