//! The release gate.
//!
//! A breach fails the gate. A skipped falsifier fails the gate. A missing
//! falsifier number delays the tag. A pass names the measured value and the
//! threshold.

use core::fmt::{self, Write as _};

use crate::falsifier::{Falsifier, Threshold};
use crate::fixed::Fixed;
use crate::measure::{FalsifierResult, MeasurementRecord, Verdict};

/// The outcome of one release gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateOutcome {
    /// Every falsifier passed.
    Pass,
    /// A falsifier breached its threshold or a falsifier was skipped.
    Fail,
    /// One falsifier number is missing.
    Delayed,
}

impl GateOutcome {
    /// Returns the stable outcome name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Delayed => "delayed",
        }
    }

    /// Returns true when the outcome allows a tag.
    #[must_use]
    pub const fn allows_tag(self) -> bool {
        matches!(self, Self::Pass)
    }
}

impl fmt::Display for GateOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The status of one falsifier in the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateStatus {
    /// The measured value passed.
    Passed,
    /// The measured value crossed the threshold.
    Breached,
    /// The run skipped the falsifier.
    Skipped,
    /// The run has no number for the falsifier.
    Missing,
}

impl GateStatus {
    /// Returns the stable status name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Passed => "pass",
            Self::Breached => "breach",
            Self::Skipped => "skip",
            Self::Missing => "missing",
        }
    }
}

impl fmt::Display for GateStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One gate entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateEntry {
    falsifier: Falsifier,
    status: GateStatus,
    measured: Option<Fixed>,
    threshold: Threshold,
    reason: Option<String>,
}

impl GateEntry {
    /// Returns the falsifier.
    #[must_use]
    pub const fn falsifier(&self) -> Falsifier {
        self.falsifier
    }

    /// Returns the status.
    #[must_use]
    pub const fn status(&self) -> GateStatus {
        self.status
    }

    /// Returns the measured value.
    #[must_use]
    pub const fn measured(&self) -> Option<Fixed> {
        self.measured
    }

    /// Returns the threshold.
    #[must_use]
    pub const fn threshold(&self) -> Threshold {
        self.threshold
    }

    /// Returns the skip reason.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Returns the deterministic detail text for the entry.
    #[must_use]
    pub fn detail(&self) -> String {
        let unit = self.threshold.unit();
        let threshold = self.threshold.describe();
        match self.status {
            GateStatus::Passed => {
                let measured = self.measured.unwrap_or_else(Fixed::zero);
                format!("pass: measured {measured} {unit}. threshold {threshold}.")
            }
            GateStatus::Breached => {
                let measured = self.measured.unwrap_or_else(Fixed::zero);
                format!(
                    "breach: measured {measured} {unit}. threshold {threshold}. action: {}.",
                    self.falsifier.action()
                )
            }
            GateStatus::Skipped => {
                let reason = self.reason.as_deref().unwrap_or("no reason");
                format!("skip: {reason} The gate fails.")
            }
            GateStatus::Missing => "missing: no measurement. The tag is delayed.".to_owned(),
        }
    }
}

/// The report of one release gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    outcome: GateOutcome,
    entries: Vec<GateEntry>,
}

impl GateReport {
    /// Returns the outcome.
    #[must_use]
    pub const fn outcome(&self) -> GateOutcome {
        self.outcome
    }

    /// Returns true when the outcome allows a tag.
    #[must_use]
    pub const fn allows_tag(&self) -> bool {
        self.outcome.allows_tag()
    }

    /// Returns the entries.
    #[must_use]
    pub fn entries(&self) -> &[GateEntry] {
        &self.entries
    }

    /// Returns the entry for one falsifier.
    #[must_use]
    pub fn entry(&self, falsifier: Falsifier) -> Option<&GateEntry> {
        self.entries
            .iter()
            .find(|entry| entry.falsifier() == falsifier)
    }

    /// Returns the deterministic text form.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        push_line(&mut text, format_args!("gate: {}", self.outcome.name()));
        for entry in &self.entries {
            push_line(
                &mut text,
                format_args!("{}: {}", entry.falsifier.name(), entry.detail()),
            );
        }
        text
    }
}

/// The release gate evaluator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReleaseGate;

impl ReleaseGate {
    /// Evaluates every falsifier against the result list.
    ///
    /// The gate takes the first result for each falsifier. It ignores a later
    /// duplicate result.
    #[must_use]
    pub fn evaluate(results: &[FalsifierResult]) -> GateReport {
        let mut entries = Vec::with_capacity(Falsifier::ALL.len());
        let mut fail = false;
        let mut delayed = false;
        for falsifier in Falsifier::ALL {
            let result = results
                .iter()
                .find(|result| result.falsifier() == falsifier);
            let entry = match result {
                Some(FalsifierResult::Measured(record)) => measured_entry(record),
                Some(FalsifierResult::Skipped { reason, .. }) => {
                    fail = true;
                    GateEntry {
                        falsifier,
                        status: GateStatus::Skipped,
                        measured: None,
                        threshold: falsifier.threshold(),
                        reason: Some(reason.clone()),
                    }
                }
                None => {
                    delayed = true;
                    GateEntry {
                        falsifier,
                        status: GateStatus::Missing,
                        measured: None,
                        threshold: falsifier.threshold(),
                        reason: None,
                    }
                }
            };
            if entry.status == GateStatus::Breached {
                fail = true;
            }
            entries.push(entry);
        }
        let outcome = if fail {
            GateOutcome::Fail
        } else if delayed {
            GateOutcome::Delayed
        } else {
            GateOutcome::Pass
        };
        GateReport { outcome, entries }
    }
}

fn measured_entry(record: &MeasurementRecord) -> GateEntry {
    let status = if record.verdict() == Verdict::Breach {
        GateStatus::Breached
    } else {
        GateStatus::Passed
    };
    GateEntry {
        falsifier: record.falsifier(),
        status,
        measured: Some(record.slope()),
        threshold: record.threshold(),
        reason: None,
    }
}

fn push_line(text: &mut String, arguments: fmt::Arguments<'_>) {
    text.write_fmt(arguments)
        .expect("writing to an in-memory string cannot fail");
    text.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::{HardwareClass, Interval, MeasurementParts};

    fn record(falsifier: Falsifier, micros: i64) -> MeasurementRecord {
        MeasurementRecord::new(MeasurementParts {
            falsifier,
            slope: Fixed::from_micros(micros),
            interval: Interval::new(
                Fixed::from_micros(micros - 1_000),
                Fixed::from_micros(micros + 1_000),
            )
            .expect("the interval is valid"),
            hardware_class: HardwareClass::Ci,
            toolchain_version: "rustc 1.99.1",
            commit: "0123456",
            sample_count: 30,
        })
        .expect("the record is valid")
    }

    fn passing_results() -> Vec<FalsifierResult> {
        vec![
            FalsifierResult::Measured(record(Falsifier::SyncAmplification, 20_000)),
            FalsifierResult::Measured(record(Falsifier::PolicyCost, 4_000_000)),
            FalsifierResult::Measured(record(Falsifier::ConflictRate, 20_000)),
            FalsifierResult::Measured(record(Falsifier::ColdStart, 1_200_000)),
            FalsifierResult::Measured(record(Falsifier::ReadP99, 50_000)),
        ]
    }

    #[test]
    fn a_full_pass_names_the_value_and_the_threshold() {
        let report = ReleaseGate::evaluate(&passing_results());
        assert_eq!(report.outcome(), GateOutcome::Pass);
        assert!(report.allows_tag());
        let entry = report
            .entry(Falsifier::SyncAmplification)
            .expect("the entry exists");
        assert_eq!(entry.status(), GateStatus::Passed);
        let detail = entry.detail();
        assert!(detail.contains("measured 0.020000"));
        assert!(detail.contains("threshold 0.050000 per_decade"));
    }

    #[test]
    fn a_crossed_threshold_fails_the_gate() {
        let mut results = passing_results();
        results[0] = FalsifierResult::Measured(record(Falsifier::SyncAmplification, 100_000));
        let report = ReleaseGate::evaluate(&results);
        assert_eq!(report.outcome(), GateOutcome::Fail);
        assert!(!report.allows_tag());
        let entry = report
            .entry(Falsifier::SyncAmplification)
            .expect("the entry exists");
        assert_eq!(entry.status(), GateStatus::Breached);
        assert!(entry.detail().contains("action:"));
    }

    #[test]
    fn a_missing_number_delays_the_tag() {
        let mut results = passing_results();
        results.pop();
        let report = ReleaseGate::evaluate(&results);
        assert_eq!(report.outcome(), GateOutcome::Delayed);
        assert!(!report.allows_tag());
        let entry = report.entry(Falsifier::ReadP99).expect("the entry exists");
        assert_eq!(entry.status(), GateStatus::Missing);
    }

    #[test]
    fn a_skipped_falsifier_fails_the_gate() {
        let mut results = passing_results();
        results[2] =
            FalsifierResult::skipped(Falsifier::ConflictRate, "The replay trace is absent.");
        let report = ReleaseGate::evaluate(&results);
        assert_eq!(report.outcome(), GateOutcome::Fail);
        let entry = report
            .entry(Falsifier::ConflictRate)
            .expect("the entry exists");
        assert_eq!(entry.status(), GateStatus::Skipped);
        assert!(entry.detail().contains("The gate fails."));
    }

    #[test]
    fn the_gate_text_is_stable() {
        let report = ReleaseGate::evaluate(&passing_results());
        assert_eq!(report.to_text(), report.to_text());
        let text = report.to_text();
        assert!(text.starts_with("gate: pass\n"), "{text:?}");
    }
}
