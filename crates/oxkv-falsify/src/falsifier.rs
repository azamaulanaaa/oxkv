//! The five falsifiers and their candidate thresholds.
//!
//! A falsifier is a measured benchmark with a threshold that can disprove a
//! claim. Each falsifier names one workload, one definition, one threshold,
//! and one action.
//!
//! The 1.0 specification fixes the shape of each falsifier. It does not fix a
//! numeric threshold. The values in this module are candidates. Mark a
//! threshold as frozen only after a committed baseline exists.

use core::fmt;

use crate::fixed::Fixed;

/// The unit of one threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ThresholdUnit {
    /// A relative change per decade.
    SlopePerDecade,
    /// A dimensionless ratio, such as `5x`.
    Ratio,
    /// A fraction of one, such as `5%`.
    Fraction,
}

impl ThresholdUnit {
    /// Returns the stable unit name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SlopePerDecade => "per_decade",
            Self::Ratio => "ratio",
            Self::Fraction => "fraction",
        }
    }
}

impl fmt::Display for ThresholdUnit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One candidate threshold.
///
/// A measured value above the limit breaches the threshold. A value equal to
/// the limit passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Threshold {
    limit: Fixed,
    unit: ThresholdUnit,
    candidate: bool,
}

impl Threshold {
    /// Creates a threshold.
    #[must_use]
    pub const fn new(limit: Fixed, unit: ThresholdUnit, candidate: bool) -> Self {
        Self {
            limit,
            unit,
            candidate,
        }
    }

    /// Returns the limit.
    #[must_use]
    pub const fn limit(self) -> Fixed {
        self.limit
    }

    /// Returns the unit.
    #[must_use]
    pub const fn unit(self) -> ThresholdUnit {
        self.unit
    }

    /// Returns true when the threshold is a candidate value.
    #[must_use]
    pub const fn is_candidate(self) -> bool {
        self.candidate
    }

    /// Returns the text form of the threshold.
    #[must_use]
    pub fn describe(self) -> String {
        let status = if self.candidate {
            "candidate"
        } else {
            "frozen"
        };
        format!("{} {} {status}", self.limit, self.unit)
    }
}

impl fmt::Display for Threshold {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.describe())
    }
}

/// One falsifier from the 1.0 qualification suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Falsifier {
    /// Sync amplification against scope size.
    SyncAmplification,
    /// Policy cost per write.
    PolicyCost,
    /// Conflict rate under realistic concurrency.
    ConflictRate,
    /// Cold start for one scope.
    ColdStart,
    /// Read p99 at a fixed scope.
    ReadP99,
}

impl Falsifier {
    /// Every falsifier in report order.
    pub const ALL: [Self; 5] = [
        Self::SyncAmplification,
        Self::PolicyCost,
        Self::ConflictRate,
        Self::ColdStart,
        Self::ReadP99,
    ];

    /// Returns the stable falsifier name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SyncAmplification => "sync_amplification",
            Self::PolicyCost => "policy_cost",
            Self::ConflictRate => "conflict_rate",
            Self::ColdStart => "cold_start",
            Self::ReadP99 => "read_p99",
        }
    }

    /// Returns the stable ordinal of the falsifier.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::SyncAmplification => 0,
            Self::PolicyCost => 1,
            Self::ConflictRate => 2,
            Self::ColdStart => 3,
            Self::ReadP99 => 4,
        }
    }

    /// Returns the one definition of the measured value.
    #[must_use]
    pub const fn definition(self) -> &'static str {
        match self {
            Self::SyncAmplification => {
                "Bytes one replica receives per committed batch, divided by the \
                 encoded bytes of the matching rows plus the index deltas."
            }
            Self::PolicyCost => {
                "p99 evaluation time and steps per write at a pinned policy \
                 version, plus the RPC count for a single-prefix write."
            }
            Self::ConflictRate => {
                "Conflict divided by the sum of committed, rejected, and \
                 conflict. Queued writes are excluded. A retry counts once."
            }
            Self::ColdStart => {
                "Seconds and bytes from an empty replica to a readable and \
                 searchable scope at a pinned version. Hydration and diff \
                 attach are included."
            }
            Self::ReadP99 => {
                "Replica latency at p50, p99, and p999 for a point read, one \
                 scope page, and one search page."
            }
        }
    }

    /// Returns the one workload of the falsifier.
    #[must_use]
    pub const fn workload(self) -> &'static str {
        match self {
            Self::SyncAmplification => "N = 1e5, 1e6, 1e7 at fixed s = 1%, 10%, 50%.",
            Self::PolicyCost => "r = 5 to 200, f = 1e2 to 1e5, cold and warm policy.",
            Self::ConflictRate => "C = 8, 64, 256 on Zipf and replay.",
            Self::ColdStart => "Scope 10 MB to 1 GB, native and wasm.",
            Self::ReadP99 => "N varies 10x at fixed scope, warm and cold.",
        }
    }

    /// Returns the candidate threshold of the falsifier.
    #[must_use]
    pub const fn threshold(self) -> Threshold {
        match self {
            Self::SyncAmplification => Threshold::new(
                Fixed::from_micros(50_000),
                ThresholdUnit::SlopePerDecade,
                true,
            ),
            Self::PolicyCost => {
                Threshold::new(Fixed::from_micros(5_000_000), ThresholdUnit::Ratio, true)
            }
            Self::ConflictRate => {
                Threshold::new(Fixed::from_micros(50_000), ThresholdUnit::Fraction, true)
            }
            Self::ColdStart => {
                Threshold::new(Fixed::from_micros(1_500_000), ThresholdUnit::Ratio, true)
            }
            Self::ReadP99 => Threshold::new(
                Fixed::from_micros(100_000),
                ThresholdUnit::SlopePerDecade,
                true,
            ),
        }
    }

    /// Returns the action that a breach forces.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::SyncAmplification => {
                "Add per-scope materialized views at the authority. Change \
                 items 13 and 14."
            }
            Self::PolicyCost => "Co-locate facts or widen the projection. Do not use 2PC.",
            Self::ConflictRate => {
                "Revisit the base-version check and the per-key FIFO \
                 guarantee. Publish the retry contract."
            }
            Self::ColdStart => "Add a per-scope snapshot path before the freeze.",
            Self::ReadP99 => "Find the dataset walk in the plan or the index. Remove it.",
        }
    }

    /// Returns true when the measured value crosses the threshold.
    #[must_use]
    pub fn breaches(self, measured: Fixed) -> bool {
        measured > self.threshold().limit()
    }

    /// Reads a falsifier from its stable name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|falsifier| falsifier.name() == name)
    }
}

impl fmt::Display for Falsifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_falsifier_names_its_contract_fields() {
        for falsifier in Falsifier::ALL {
            assert_ne!(falsifier.name(), "");
            assert_ne!(falsifier.definition(), "");
            assert_ne!(falsifier.workload(), "");
            assert_ne!(falsifier.action(), "");
            assert!(falsifier.threshold().is_candidate());
            assert!(falsifier.threshold().limit().micros() > 0);
        }
    }

    #[test]
    fn the_names_are_stable_and_unique() {
        let names: Vec<&str> = Falsifier::ALL.iter().map(|item| item.name()).collect();
        assert_eq!(names.len(), 5);
        for (index, name) in names.iter().enumerate() {
            assert_eq!(Falsifier::from_name(name), Some(Falsifier::ALL[index]));
            assert_eq!(Falsifier::ALL[index].index(), index);
            assert!(!names[..index].contains(name));
        }
        assert_eq!(Falsifier::from_name("unknown"), None);
    }

    #[test]
    fn a_value_equal_to_the_limit_passes() {
        let threshold = Falsifier::SyncAmplification.threshold();
        assert!(!Falsifier::SyncAmplification.breaches(threshold.limit()));
        assert!(
            Falsifier::SyncAmplification
                .breaches(Fixed::from_micros(threshold.limit().micros() + 1))
        );
    }

    #[test]
    fn the_threshold_text_names_the_unit_and_candidate_state() {
        assert_eq!(
            Falsifier::SyncAmplification.threshold().to_string(),
            "0.050000 per_decade candidate"
        );
        assert_eq!(
            Falsifier::PolicyCost.threshold().to_string(),
            "5.000000 ratio candidate"
        );
        assert_eq!(
            Falsifier::ConflictRate.threshold().to_string(),
            "0.050000 fraction candidate"
        );
    }
}
