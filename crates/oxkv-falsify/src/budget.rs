//! Scope budget, ranking cost, and coverage records.
//!
//! The scope budget uses the declared device budget. The type has no host
//! memory input. The ranking cost counts document-local scoring only. The
//! result mode marks a coverage result.

use core::fmt;

use oxkv_contracts::version::{CommitVersion, ShardId};

/// The storage budget of one scope.
///
/// The measurement uses the declared device budget. It never reads host
/// memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ScopeBudget {
    declared_device_budget: u64,
    scope: u64,
    peak_hydration: u64,
}

impl ScopeBudget {
    /// Creates a scope budget measurement.
    #[must_use]
    pub const fn new(
        declared_device_budget_bytes: u64,
        scope_bytes: u64,
        peak_hydration_bytes: u64,
    ) -> Self {
        Self {
            declared_device_budget: declared_device_budget_bytes,
            scope: scope_bytes,
            peak_hydration: peak_hydration_bytes,
        }
    }

    /// Returns an empty measurement.
    #[must_use]
    pub const fn none() -> Self {
        Self::new(0, 0, 0)
    }

    /// Returns the declared device budget.
    #[must_use]
    pub const fn declared_device_budget_bytes(self) -> u64 {
        self.declared_device_budget
    }

    /// Returns the live scope byte count.
    #[must_use]
    pub const fn scope_bytes(self) -> u64 {
        self.scope
    }

    /// Returns the peak hydration byte count.
    #[must_use]
    pub const fn peak_hydration_bytes(self) -> u64 {
        self.peak_hydration
    }

    /// Returns the scope bytes plus the peak hydration bytes.
    #[must_use]
    pub const fn total_bytes(self) -> u64 {
        self.scope.saturating_add(self.peak_hydration)
    }

    /// Returns the remaining declared budget.
    #[must_use]
    pub const fn headroom_bytes(self) -> u64 {
        self.declared_device_budget
            .saturating_sub(self.total_bytes())
    }

    /// Returns true when the scope and its hydration fit the declared budget.
    #[must_use]
    pub const fn fits(self) -> bool {
        self.declared_device_budget > 0 && self.total_bytes() <= self.declared_device_budget
    }
}

/// The document-local cost of one search page.
///
/// The type accepts document-local features only. It has no field for a
/// global statistic such as a collection-wide term frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RankingCost {
    scored_documents: u64,
    document_local_terms: u64,
}

impl RankingCost {
    /// Creates a document-local ranking cost.
    #[must_use]
    pub const fn document_local(scored_documents: u64, document_local_terms: u64) -> Self {
        Self {
            scored_documents,
            document_local_terms,
        }
    }

    /// Returns an empty cost.
    #[must_use]
    pub const fn none() -> Self {
        Self::document_local(0, 0)
    }

    /// Returns the scored document count.
    #[must_use]
    pub const fn scored_documents(self) -> u64 {
        self.scored_documents
    }

    /// Returns the document-local term count per document.
    #[must_use]
    pub const fn document_local_terms(self) -> u64 {
        self.document_local_terms
    }

    /// Returns the total term visits.
    #[must_use]
    pub const fn total_term_visits(self) -> u64 {
        self.scored_documents
            .saturating_mul(self.document_local_terms)
    }
}

/// One coverage descriptor.
///
/// The descriptor lists the answered shards, the missing shards, and the
/// served versions. A coverage result carries this descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoverageDescriptor {
    answered_shards: Vec<ShardId>,
    missing_shards: Vec<ShardId>,
    served_versions: Vec<(ShardId, CommitVersion)>,
}

impl CoverageDescriptor {
    /// Creates a coverage descriptor.
    ///
    /// The constructor sorts every list by shard identifier. The text form is
    /// therefore stable.
    #[must_use]
    pub fn new(
        mut answered_shards: Vec<ShardId>,
        mut missing_shards: Vec<ShardId>,
        mut served_versions: Vec<(ShardId, CommitVersion)>,
    ) -> Self {
        answered_shards.sort();
        missing_shards.sort();
        served_versions.sort_by(|left, right| left.0.cmp(&right.0));
        Self {
            answered_shards,
            missing_shards,
            served_versions,
        }
    }

    /// Returns the answered shards.
    #[must_use]
    pub fn answered_shards(&self) -> &[ShardId] {
        &self.answered_shards
    }

    /// Returns the missing shards.
    #[must_use]
    pub fn missing_shards(&self) -> &[ShardId] {
        &self.missing_shards
    }

    /// Returns the served versions.
    #[must_use]
    pub fn served_versions(&self) -> &[(ShardId, CommitVersion)] {
        &self.served_versions
    }
}

/// The result mode of one run.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResultMode {
    /// Every targeted shard served the pin.
    Exact,
    /// One or more targeted shards did not serve the pin.
    Coverage(CoverageDescriptor),
}

impl ResultMode {
    /// Returns true when the mode is coverage.
    #[must_use]
    pub const fn is_coverage(&self) -> bool {
        matches!(self, Self::Coverage(_))
    }

    /// Returns the stable mode name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Coverage(_) => "coverage",
        }
    }
}

impl fmt::Display for ResultMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scope_budget_uses_the_declared_device_budget() {
        let fits = ScopeBudget::new(1_000, 900, 50);
        assert_eq!(fits.total_bytes(), 950);
        assert_eq!(fits.headroom_bytes(), 50);
        assert!(fits.fits());

        let over = ScopeBudget::new(1_000, 900, 101);
        assert_eq!(over.total_bytes(), 1_001);
        assert_eq!(over.headroom_bytes(), 0);
        assert!(!over.fits());
        assert!(!ScopeBudget::none().fits());
    }

    #[test]
    fn the_ranking_cost_counts_document_local_terms() {
        let cost = RankingCost::document_local(100, 50);
        assert_eq!(cost.scored_documents(), 100);
        assert_eq!(cost.document_local_terms(), 50);
        assert_eq!(cost.total_term_visits(), 5_000);
        assert_eq!(RankingCost::none().total_term_visits(), 0);
    }

    #[test]
    fn a_coverage_descriptor_sorts_every_list() {
        let descriptor = CoverageDescriptor::new(
            vec![ShardId::new(vec![2]), ShardId::new(vec![1])],
            vec![ShardId::new(vec![4]), ShardId::new(vec![3])],
            vec![
                (ShardId::new(vec![2]), CommitVersion::new(12)),
                (ShardId::new(vec![1]), CommitVersion::new(11)),
            ],
        );
        assert_eq!(
            descriptor.answered_shards(),
            &[ShardId::new(vec![1]), ShardId::new(vec![2])]
        );
        assert_eq!(
            descriptor.missing_shards(),
            &[ShardId::new(vec![3]), ShardId::new(vec![4])]
        );
        assert_eq!(descriptor.served_versions()[0].0, ShardId::new(vec![1]));
        assert!(ResultMode::Coverage(descriptor).is_coverage());
        assert!(!ResultMode::Exact.is_coverage());
        assert_eq!(ResultMode::Exact.name(), "exact");
    }
}
