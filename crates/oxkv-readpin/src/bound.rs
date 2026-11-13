//! Staleness bounds and the measured staleness of a read.
//!
//! Staleness is the worse of two values: the version lag in commits and the
//! replication silence in injected milliseconds. Admission fails when either
//! value exceeds its budget.

use crate::time::DurationMillis;

/// A staleness budget.
///
/// `None` on an axis means unbounded on that axis. A scope supplies the
/// default. A read may narrow it or widen it with an explicit override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StalenessBound {
    max_version_lag: Option<u64>,
    max_silence: Option<DurationMillis>,
}

impl StalenessBound {
    /// An unbounded budget. The response reports the measured staleness.
    pub const UNBOUNDED: Self = Self {
        max_version_lag: None,
        max_silence: None,
    };

    /// Creates a bounded budget.
    #[must_use]
    pub const fn new(max_version_lag: u64, max_silence: DurationMillis) -> Self {
        Self {
            max_version_lag: Some(max_version_lag),
            max_silence: Some(max_silence),
        }
    }

    /// Returns the version lag budget.
    #[must_use]
    pub const fn max_version_lag(self) -> Option<u64> {
        self.max_version_lag
    }

    /// Returns the replication silence budget.
    #[must_use]
    pub const fn max_silence(self) -> Option<DurationMillis> {
        self.max_silence
    }

    /// Returns true when both axes are unbounded.
    #[must_use]
    pub const fn is_unbounded(self) -> bool {
        self.max_version_lag.is_none() && self.max_silence.is_none()
    }

    /// Returns the tighter budget on each axis.
    ///
    /// An unbounded axis loses to a bounded axis. This operation never widens
    /// the budget.
    #[must_use]
    pub fn stricter(self, other: Self) -> Self {
        Self {
            max_version_lag: tighter(self.max_version_lag, other.max_version_lag),
            max_silence: tighter(self.max_silence, other.max_silence),
        }
    }

    /// Returns true when the measured staleness is inside the budget.
    #[must_use]
    pub fn permits(self, measured: StalenessMeasure) -> bool {
        let lag_ok = self
            .max_version_lag
            .is_none_or(|budget| measured.version_lag() <= budget);
        let silence_ok = self
            .max_silence
            .is_none_or(|budget| measured.silence() <= budget);
        lag_ok && silence_ok
    }
}

fn tighter<T: Ord>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

/// How a read selects its budget from the scope default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoundRequest {
    /// Use the scope default.
    #[default]
    ScopeDefault,
    /// Narrow the scope default. The result is never wider than the default.
    Narrow(StalenessBound),
    /// Replace the scope default. This is the explicit widening path.
    Override(StalenessBound),
    /// Declare no bound for offline use. The response reports data age.
    Unbounded,
}

impl BoundRequest {
    /// Resolves the request against the scope default.
    #[must_use]
    pub fn resolve(self, scope_default: StalenessBound) -> StalenessBound {
        match self {
            Self::ScopeDefault => scope_default,
            Self::Narrow(bound) => scope_default.stricter(bound),
            Self::Override(bound) => bound,
            Self::Unbounded => StalenessBound::UNBOUNDED,
        }
    }
}

/// The measured staleness of one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StalenessMeasure {
    version_lag: u64,
    silence: DurationMillis,
}

impl StalenessMeasure {
    /// Creates a staleness measure.
    #[must_use]
    pub const fn new(version_lag: u64, silence: DurationMillis) -> Self {
        Self {
            version_lag,
            silence,
        }
    }

    /// Returns the version lag in commits.
    #[must_use]
    pub const fn version_lag(self) -> u64 {
        self.version_lag
    }

    /// Returns the replication silence.
    #[must_use]
    pub const fn silence(self) -> DurationMillis {
        self.silence
    }

    /// Returns true when the measure is inside the budget.
    #[must_use]
    pub fn within(self, bound: StalenessBound) -> bool {
        bound.permits(self)
    }
}
