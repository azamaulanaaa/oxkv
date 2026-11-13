//! Device budget, resident accounting, and eviction reports.
//!
//! The device budget covers the scope at rest and the hydration peak. A scope
//! that cannot fit at rest fails with `scope.device_budget_exceeded`. A scope
//! that fits at rest but cannot fit its hydration peak fails with
//! `scope.hydration_budget_exceeded`.
//!
//! Eviction removes only out-of-scope or superseded pages. It never removes a
//! live scope member.

use crate::error::ReplicaError;

/// The declared device budget of one replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceBudget {
    /// The maximum resident byte count.
    pub max_resident_bytes: u64,
    /// The maximum hydration overhead above the final resident bytes.
    pub max_hydration_overhead_bytes: u64,
}

impl DeviceBudget {
    /// Creates a device budget.
    #[must_use]
    pub const fn new(max_resident_bytes: u64, max_hydration_overhead_bytes: u64) -> Self {
        Self {
            max_resident_bytes,
            max_hydration_overhead_bytes,
        }
    }

    /// Checks one scope footprint at subscribe time.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::DeviceBudgetExceeded`] when the scope does not
    /// fit at rest. Returns [`ReplicaError::HydrationBudgetExceeded`] when the
    /// hydration peak does not fit.
    pub const fn admit(&self, footprint: ScopeFootprint) -> Result<(), ReplicaError> {
        if footprint.resident_bytes > self.max_resident_bytes {
            return Err(ReplicaError::DeviceBudgetExceeded {
                required: footprint.resident_bytes,
                budget: self.max_resident_bytes,
            });
        }
        let peak = footprint
            .resident_bytes
            .saturating_add(footprint.hydration_overhead_bytes);
        if peak > self.max_resident_bytes {
            return Err(ReplicaError::HydrationBudgetExceeded {
                required: peak,
                budget: self.max_resident_bytes,
            });
        }
        if footprint.hydration_overhead_bytes > self.max_hydration_overhead_bytes {
            return Err(ReplicaError::HydrationBudgetExceeded {
                required: peak,
                budget: self
                    .max_resident_bytes
                    .saturating_add(self.max_hydration_overhead_bytes),
            });
        }
        Ok(())
    }
}

/// The declared byte footprint of one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeFootprint {
    /// The resident byte count after hydration.
    pub resident_bytes: u64,
    /// The extra byte count that hydration needs beside the served generation.
    pub hydration_overhead_bytes: u64,
}

impl ScopeFootprint {
    /// Creates a scope footprint.
    #[must_use]
    pub const fn new(resident_bytes: u64, hydration_overhead_bytes: u64) -> Self {
        Self {
            resident_bytes,
            hydration_overhead_bytes,
        }
    }
}

/// The resident byte report of one replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidencyReport {
    /// The mirror page, row, fact, and tail bytes.
    pub mirror_bytes: u64,
    /// The local index bytes.
    pub index_bytes: u64,
    /// The outbox bytes.
    pub outbox_bytes: u64,
    /// The hydration staging bytes.
    pub hydration_bytes: u64,
    /// The total resident bytes.
    pub total_bytes: u64,
    /// The resident ceiling.
    pub ceiling_bytes: u64,
    /// The highest observed hydration peak.
    pub hydration_peak_bytes: u64,
    /// The page count that eviction removed.
    pub evicted_pages: u64,
    /// The byte count that eviction freed.
    pub evicted_bytes: u64,
}

/// The result of one eviction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EvictionReport {
    /// The page count that eviction removed.
    pub pages_removed: u64,
    /// The byte count that eviction freed.
    pub bytes_freed: u64,
}

impl EvictionReport {
    /// Adds one eviction result to this report.
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        Self {
            pages_removed: self.pages_removed.saturating_add(other.pages_removed),
            bytes_freed: self.bytes_freed.saturating_add(other.bytes_freed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceBudget, ScopeFootprint};

    #[test]
    fn a_scope_that_fits_is_admitted() {
        let budget = DeviceBudget::new(2_000, 2_000);
        assert!(budget.admit(ScopeFootprint::new(600, 600)).is_ok());
    }

    #[test]
    fn a_scope_over_the_device_budget_fails() {
        let budget = DeviceBudget::new(1_000, 0);
        assert_eq!(
            budget
                .admit(ScopeFootprint::new(1_001, 0))
                .expect_err("the scope is too large")
                .code(),
            "scope.device_budget_exceeded"
        );
    }

    #[test]
    fn a_hydration_peak_over_the_budget_fails() {
        let budget = DeviceBudget::new(1_000, 1_000);
        assert_eq!(
            budget
                .admit(ScopeFootprint::new(600, 500))
                .expect_err("the peak is too large")
                .code(),
            "scope.hydration_budget_exceeded"
        );
    }

    #[test]
    fn a_hydration_overhead_over_the_declared_limit_fails() {
        let budget = DeviceBudget::new(10_000, 100);
        assert_eq!(
            budget
                .admit(ScopeFootprint::new(600, 500))
                .expect_err("the overhead is too large")
                .code(),
            "scope.hydration_budget_exceeded"
        );
    }
}
