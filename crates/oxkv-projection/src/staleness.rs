//! Staleness bounds and authorization decisions.
//!
//! The authorization bound is a version distance per fact source. Time is
//! monitoring data only. Time does not enter a policy verdict.
//!
//! In the default mode, a write over the bound stays `Queued` until the queue
//! deadline. At the deadline the authority rejects the write. In strict mode,
//! a write over the bound is rejected at once.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::{FactSourceId, FactVersion};

use crate::error::{AUTHORIZATION_STALE_FACTS, PROJECTION_LAGGING, WRITE_QUEUE_TIMEOUT};
use crate::fact::FactVersionDistance;

/// The current version of each fact source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactSourceVersions {
    versions: BTreeMap<FactSourceId, FactVersion>,
}

impl FactSourceVersions {
    /// Creates an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets one source version.
    pub fn set(&mut self, source: FactSourceId, version: FactVersion) {
        self.versions.insert(source, version);
    }

    /// Returns one source version.
    #[must_use]
    pub fn get(&self, source: &FactSourceId) -> Option<FactVersion> {
        self.versions.get(source).copied()
    }

    /// Returns the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    /// Returns true when the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// Iterates over the entries in canonical source order.
    pub fn iter(&self) -> impl Iterator<Item = (&FactSourceId, FactVersion)> {
        self.versions
            .iter()
            .map(|(source, version)| (source, *version))
    }
}

/// One measured version distance for one fact source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalenessReport {
    source: FactSourceId,
    watermark: FactVersion,
    observed: Option<FactVersion>,
    bound: FactVersionDistance,
    distance: Option<FactVersionDistance>,
}

impl StalenessReport {
    /// Creates a staleness report.
    #[must_use]
    pub const fn new(
        source: FactSourceId,
        watermark: FactVersion,
        observed: Option<FactVersion>,
        bound: FactVersionDistance,
    ) -> Self {
        let distance = match observed {
            Some(observed) => FactVersionDistance::between(watermark, observed),
            None => None,
        };
        Self {
            source,
            watermark,
            observed,
            bound,
            distance,
        }
    }

    /// Returns the fact source.
    #[must_use]
    pub const fn source(&self) -> &FactSourceId {
        &self.source
    }

    /// Returns the projection watermark.
    #[must_use]
    pub const fn watermark(&self) -> FactVersion {
        self.watermark
    }

    /// Returns the observed source version.
    #[must_use]
    pub const fn observed(&self) -> Option<FactVersion> {
        self.observed
    }

    /// Returns the declared bound.
    #[must_use]
    pub const fn bound(&self) -> FactVersionDistance {
        self.bound
    }

    /// Returns the measured distance.
    ///
    /// The result is `None` when the source version is unknown or behind the
    /// projection watermark. An unknown distance fails closed.
    #[must_use]
    pub const fn distance(&self) -> Option<FactVersionDistance> {
        self.distance
    }

    /// Returns true when the distance is known and within the bound.
    #[must_use]
    pub fn within_bound(&self) -> bool {
        self.distance
            .is_some_and(|distance| distance.is_within(self.bound))
    }

    /// Returns one monitoring observation.
    ///
    /// The elapsed tick count is monitoring data only. It does not change the
    /// authorization decision.
    #[must_use]
    pub fn to_monitoring(&self, elapsed_ticks: u64) -> LagObservation {
        LagObservation {
            version_distance: self.distance,
            elapsed_ticks,
        }
    }
}

/// One lag observation for monitoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LagObservation {
    /// The measured version distance.
    pub version_distance: Option<FactVersionDistance>,
    /// The elapsed monitoring ticks.
    pub elapsed_ticks: u64,
}

/// The authorization mode of one rule or table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorizationMode {
    /// Keep a lagging write `Queued` until the deadline.
    Default,
    /// Reject a lagging write at once.
    Strict,
}

impl AuthorizationMode {
    /// Returns the canonical mode name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Strict => "strict",
        }
    }
}

/// The injected queue deadline state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueueDeadline {
    /// The deadline has not arrived.
    Pending,
    /// The deadline has arrived.
    Reached,
}

impl QueueDeadline {
    /// Returns the canonical deadline name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Reached => "reached",
        }
    }

    /// Returns true when the deadline has arrived.
    #[must_use]
    pub const fn is_reached(self) -> bool {
        matches!(self, Self::Reached)
    }
}

/// The decision for one authorization attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorizationDecision {
    /// The projection is inside every bound.
    Allow,
    /// The write stays live and waits for the projection.
    Queue {
        /// The canonical reason code.
        reason: &'static str,
    },
    /// The authority refuses the write.
    Reject {
        /// The canonical reason code.
        reason: &'static str,
    },
}

impl AuthorizationDecision {
    /// Returns the receipt state, or `None` for an allow decision.
    #[must_use]
    pub const fn receipt_state(self) -> Option<ReceiptState> {
        match self {
            Self::Allow => None,
            Self::Queue { .. } => Some(ReceiptState::Queued),
            Self::Reject { .. } => Some(ReceiptState::Rejected),
        }
    }

    /// Returns the reason code.
    #[must_use]
    pub const fn reason(self) -> Option<&'static str> {
        match self {
            Self::Allow => None,
            Self::Queue { reason } | Self::Reject { reason } => Some(reason),
        }
    }

    /// Returns true when the attempt may proceed.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }

    /// Returns true when the attempt is live.
    #[must_use]
    pub const fn is_queued(self) -> bool {
        matches!(self, Self::Queue { .. })
    }
}

/// The decision and the measured reports for one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationReport {
    /// The authorization decision.
    pub decision: AuthorizationDecision,
    /// The per-source staleness reports.
    pub staleness: Vec<StalenessReport>,
}

impl AuthorizationReport {
    /// Creates a report from per-source staleness reports.
    #[must_use]
    pub fn new(
        staleness: Vec<StalenessReport>,
        mode: AuthorizationMode,
        deadline: QueueDeadline,
    ) -> Self {
        let decision = authorize(&staleness, mode, deadline);
        Self {
            decision,
            staleness,
        }
    }

    /// Returns true when one source is outside its bound.
    #[must_use]
    pub fn is_over_bound(&self) -> bool {
        self.staleness.iter().any(|report| !report.within_bound())
    }
}

/// Returns the decision for a set of staleness reports.
#[must_use]
pub fn authorize(
    staleness: &[StalenessReport],
    mode: AuthorizationMode,
    deadline: QueueDeadline,
) -> AuthorizationDecision {
    if staleness.iter().all(StalenessReport::within_bound) {
        return AuthorizationDecision::Allow;
    }
    match mode {
        AuthorizationMode::Strict => AuthorizationDecision::Reject {
            reason: AUTHORIZATION_STALE_FACTS,
        },
        AuthorizationMode::Default if deadline.is_reached() => AuthorizationDecision::Reject {
            reason: WRITE_QUEUE_TIMEOUT,
        },
        AuthorizationMode::Default => AuthorizationDecision::Queue {
            reason: PROJECTION_LAGGING,
        },
    }
}
