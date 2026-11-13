//! Metrics and traces as pure records.
//!
//! Telemetry comes from protocol events. The records use integer values only.
//! The crate never reads a clock and never opens a network connection.

use std::collections::BTreeMap;

use oxkv_contracts::version::Hash32;

/// One metric signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MetricName {
    /// The active manifest version.
    ManifestVersion,
    /// The number of write conflicts.
    ConflictCount,
    /// The number of committed writes.
    CommitCount,
    /// The number of checksum failures.
    ChecksumFailures,
    /// The number of restore failures.
    RestoreFailures,
    /// The sync lag in milliseconds.
    SyncLagMillis,
    /// The cursor age in milliseconds.
    CursorAgeMillis,
    /// The number of resumes.
    ResumeCount,
    /// The number of full resyncs.
    FullResyncCount,
    /// The policy evaluation time in nanoseconds.
    PolicyTimeNanos,
    /// The number of budget rejects.
    BudgetRejects,
    /// The number of `Committed` receipts.
    ReceiptCommitted,
    /// The number of `Rejected` receipts.
    ReceiptRejected,
    /// The number of `Conflict` receipts.
    ReceiptConflict,
    /// The number of `Queued` receipts.
    ReceiptQueued,
    /// The age of the oldest `Queued` receipt in milliseconds.
    OldestQueuedAgeMillis,
    /// The number of errors.
    ErrorCount,
}

impl MetricName {
    /// Every metric signal.
    pub const ALL: [Self; 17] = [
        Self::ManifestVersion,
        Self::ConflictCount,
        Self::CommitCount,
        Self::ChecksumFailures,
        Self::RestoreFailures,
        Self::SyncLagMillis,
        Self::CursorAgeMillis,
        Self::ResumeCount,
        Self::FullResyncCount,
        Self::PolicyTimeNanos,
        Self::BudgetRejects,
        Self::ReceiptCommitted,
        Self::ReceiptRejected,
        Self::ReceiptConflict,
        Self::ReceiptQueued,
        Self::OldestQueuedAgeMillis,
        Self::ErrorCount,
    ];

    /// Returns the canonical metric name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ManifestVersion => "manifest_version",
            Self::ConflictCount => "conflict_count",
            Self::CommitCount => "commit_count",
            Self::ChecksumFailures => "checksum_failures",
            Self::RestoreFailures => "restore_failures",
            Self::SyncLagMillis => "sync_lag_millis",
            Self::CursorAgeMillis => "cursor_age_millis",
            Self::ResumeCount => "resume_count",
            Self::FullResyncCount => "full_resync_count",
            Self::PolicyTimeNanos => "policy_time_nanos",
            Self::BudgetRejects => "budget_rejects",
            Self::ReceiptCommitted => "receipt_committed",
            Self::ReceiptRejected => "receipt_rejected",
            Self::ReceiptConflict => "receipt_conflict",
            Self::ReceiptQueued => "receipt_queued",
            Self::OldestQueuedAgeMillis => "oldest_queued_age_millis",
            Self::ErrorCount => "error_count",
        }
    }
}

impl core::fmt::Display for MetricName {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One metric value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetricValue {
    /// A monotonic counter.
    Counter(u64),
    /// A current value.
    Gauge(u64),
    /// An integer ratio. The denominator can be zero.
    Ratio {
        /// The numerator.
        numerator: u64,
        /// The denominator.
        denominator: u64,
    },
}

impl MetricValue {
    /// Returns the integer value of a counter or a gauge.
    #[must_use]
    pub const fn get(self) -> Option<u64> {
        match self {
            Self::Counter(value) | Self::Gauge(value) => Some(value),
            Self::Ratio { .. } => None,
        }
    }
}

/// One metric sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MetricSample {
    /// The metric name.
    pub name: MetricName,
    /// The metric value.
    pub value: MetricValue,
}

impl MetricSample {
    /// Creates a counter sample.
    #[must_use]
    pub const fn counter(name: MetricName, value: u64) -> Self {
        Self {
            name,
            value: MetricValue::Counter(value),
        }
    }

    /// Creates a gauge sample.
    #[must_use]
    pub const fn gauge(name: MetricName, value: u64) -> Self {
        Self {
            name,
            value: MetricValue::Gauge(value),
        }
    }

    /// Creates a ratio sample.
    #[must_use]
    pub const fn ratio(name: MetricName, numerator: u64, denominator: u64) -> Self {
        Self {
            name,
            value: MetricValue::Ratio {
                numerator,
                denominator,
            },
        }
    }
}

/// One trace family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraceKind {
    /// One write from acceptance to event.
    Write,
    /// One query with a span per shard.
    Query,
    /// One sync from handshake to applied diff.
    Sync,
}

impl TraceKind {
    /// Every trace family.
    pub const ALL: [Self; 3] = [Self::Write, Self::Query, Self::Sync];

    /// Returns the canonical trace name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Query => "query",
            Self::Sync => "sync",
        }
    }
}

impl core::fmt::Display for TraceKind {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One trace span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceSpan {
    /// The span name.
    pub name: String,
    /// The fixed-token attributes.
    pub attributes: BTreeMap<String, String>,
}

impl TraceSpan {
    /// Creates a trace span.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: BTreeMap::new(),
        }
    }

    /// Adds one attribute.
    #[must_use]
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

/// One trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace {
    /// The trace identity.
    pub id: Hash32,
    /// The trace family.
    pub kind: TraceKind,
    /// The spans in start order.
    pub spans: Vec<TraceSpan>,
}

impl Trace {
    /// Creates a trace.
    #[must_use]
    pub fn new(id: Hash32, kind: TraceKind, spans: Vec<TraceSpan>) -> Self {
        Self { id, kind, spans }
    }
}

/// One telemetry batch from the operational feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TelemetryBatch {
    /// The metric samples.
    pub metrics: Vec<MetricSample>,
    /// The traces.
    pub traces: Vec<Trace>,
}

impl TelemetryBatch {
    /// Creates an empty telemetry batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one metric sample.
    pub fn push_metric(&mut self, sample: MetricSample) {
        self.metrics.push(sample);
    }

    /// Adds one trace.
    pub fn push_trace(&mut self, trace: Trace) {
        self.traces.push(trace);
    }

    /// Returns the sample for one metric name.
    #[must_use]
    pub fn sample(&self, name: MetricName) -> Option<&MetricSample> {
        self.metrics.iter().find(|sample| sample.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_required_metric_has_a_name() {
        let names = MetricName::ALL.map(MetricName::name);
        for required in [
            "manifest_version",
            "conflict_count",
            "checksum_failures",
            "restore_failures",
            "sync_lag_millis",
            "cursor_age_millis",
            "resume_count",
            "full_resync_count",
            "policy_time_nanos",
            "budget_rejects",
            "receipt_committed",
            "receipt_rejected",
            "receipt_conflict",
            "receipt_queued",
            "oldest_queued_age_millis",
            "error_count",
        ] {
            assert!(names.contains(&required), "the metric {required} is absent");
        }
    }

    #[test]
    fn every_trace_family_has_a_record() {
        for kind in TraceKind::ALL {
            let trace = Trace::new(
                Hash32::from_bytes([1; 32]),
                kind,
                vec![TraceSpan::new("span").with_attribute("shard", "1")],
            );
            assert_eq!(trace.kind, kind);
            assert_eq!(trace.spans.len(), 1);
        }
        assert_eq!(
            TraceKind::ALL.map(TraceKind::name),
            ["write", "query", "sync"]
        );
    }

    #[test]
    fn a_ratio_has_no_scalar_value() {
        let sample = MetricSample::ratio(MetricName::ConflictCount, 3, 10);
        assert_eq!(sample.value.get(), None);
        assert_eq!(
            MetricSample::counter(MetricName::CommitCount, 9)
                .value
                .get(),
            Some(9)
        );
    }

    #[test]
    fn a_batch_returns_a_named_sample() {
        let mut batch = TelemetryBatch::new();
        batch.push_metric(MetricSample::gauge(MetricName::ManifestVersion, 3));
        assert_eq!(
            batch.sample(MetricName::ManifestVersion),
            Some(&MetricSample::gauge(MetricName::ManifestVersion, 3))
        );
        assert_eq!(batch.sample(MetricName::ErrorCount), None);
    }
}
