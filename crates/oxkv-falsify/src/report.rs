//! The deterministic report writer.
//!
//! [`ReportWriter`] turns one workload spec, one result list, and one gate
//! verdict into one text report. The report has a fixed field order. It uses
//! integer fixed-point values only.
//!
//! The report marks a coverage result. The report names the declared device
//! budget and the document-local ranking cost.

use core::fmt::{self, Write as _};

use oxkv_contracts::version::{CommitVersion, ShardId};

use crate::budget::{RankingCost, ResultMode, ScopeBudget};
use crate::falsifier::Falsifier;
use crate::gate::ReleaseGate;
use crate::measure::{FalsifierResult, HardwareClass, MeasurementRecord};
use crate::workload::WorkloadSpec;

/// The input of one report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportInput {
    /// The workload spec.
    pub spec: WorkloadSpec,
    /// The hardware class.
    pub hardware_class: HardwareClass,
    /// The toolchain version.
    pub toolchain_version: String,
    /// The source commit.
    pub commit: String,
    /// The falsifier results.
    pub results: Vec<FalsifierResult>,
    /// The scope budget.
    pub scope_budget: ScopeBudget,
    /// The ranking cost.
    pub ranking_cost: RankingCost,
    /// The result mode.
    pub result_mode: ResultMode,
}

impl ReportInput {
    /// Creates a report input with an empty budget and an exact result mode.
    #[must_use]
    pub fn new(
        spec: WorkloadSpec,
        hardware_class: HardwareClass,
        toolchain_version: &str,
        commit: &str,
        results: Vec<FalsifierResult>,
    ) -> Self {
        Self {
            spec,
            hardware_class,
            toolchain_version: toolchain_version.to_owned(),
            commit: commit.to_owned(),
            results,
            scope_budget: ScopeBudget::none(),
            ranking_cost: RankingCost::none(),
            result_mode: ResultMode::Exact,
        }
    }

    /// Sets the scope budget.
    #[must_use]
    pub fn with_scope_budget(mut self, scope_budget: ScopeBudget) -> Self {
        self.scope_budget = scope_budget;
        self
    }

    /// Sets the ranking cost.
    #[must_use]
    pub fn with_ranking_cost(mut self, ranking_cost: RankingCost) -> Self {
        self.ranking_cost = ranking_cost;
        self
    }

    /// Sets the result mode.
    #[must_use]
    pub fn with_result_mode(mut self, result_mode: ResultMode) -> Self {
        self.result_mode = result_mode;
        self
    }

    /// Returns the gate report for the results.
    #[must_use]
    pub fn gate(&self) -> crate::gate::GateReport {
        ReleaseGate::evaluate(&self.results)
    }

    /// Returns the deterministic text report.
    #[must_use]
    pub fn write(&self) -> String {
        ReportWriter::write(self)
    }
}

/// The deterministic report writer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReportWriter;

impl ReportWriter {
    /// Writes one deterministic text report.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn write(input: &ReportInput) -> String {
        let mut text = String::new();
        if write_report(input, &mut text).is_err() {
            text.clear();
        }
        text
    }
}

#[allow(clippy::too_many_lines)]
fn write_report(input: &ReportInput, text: &mut String) -> fmt::Result {
    let gate = input.gate();
    writeln!(text, "oxkv falsifier report v1")?;
    writeln!(text, "spec_hash: {}", input.spec.hash())?;
    writeln!(text, "hardware_class: {}", input.hardware_class.name())?;
    writeln!(text, "toolchain_version: {}", input.toolchain_version)?;
    writeln!(text, "commit: {}", input.commit)?;
    writeln!(text, "result_mode: {}", input.result_mode.name())?;
    write_coverage(text, &input.result_mode)?;
    writeln!(text, "gate: {}", gate.outcome().name())?;
    writeln!(text, "scope_budget:")?;
    writeln!(
        text,
        "  declared_device_budget_bytes: {}",
        input.scope_budget.declared_device_budget_bytes()
    )?;
    writeln!(text, "  scope_bytes: {}", input.scope_budget.scope_bytes())?;
    writeln!(
        text,
        "  peak_hydration_bytes: {}",
        input.scope_budget.peak_hydration_bytes()
    )?;
    writeln!(text, "  total_bytes: {}", input.scope_budget.total_bytes())?;
    writeln!(
        text,
        "  fits_declared_device_budget: {}",
        input.scope_budget.fits()
    )?;
    writeln!(text, "ranking_cost:")?;
    writeln!(
        text,
        "  scored_documents: {}",
        input.ranking_cost.scored_documents()
    )?;
    writeln!(
        text,
        "  document_local_terms: {}",
        input.ranking_cost.document_local_terms()
    )?;
    writeln!(
        text,
        "  total_term_visits: {}",
        input.ranking_cost.total_term_visits()
    )?;
    writeln!(text, "  document_local_only: true")?;
    writeln!(text, "falsifiers:")?;
    for falsifier in Falsifier::ALL {
        let result = input
            .results
            .iter()
            .find(|result| result.falsifier() == falsifier);
        match result {
            Some(FalsifierResult::Measured(record)) => {
                write_measurement(text, record)?;
            }
            Some(FalsifierResult::Skipped { reason, .. }) => {
                writeln!(text, "  [skip] {}", falsifier.name())?;
                writeln!(text, "    reason: {reason}")?;
            }
            None => {
                writeln!(text, "  [missing] {}", falsifier.name())?;
                writeln!(text, "    no measurement")?;
            }
        }
    }
    writeln!(text, "gate_detail:")?;
    for line in gate.to_text().lines() {
        writeln!(text, "  {line}")?;
    }
    Ok(())
}

fn write_measurement(text: &mut String, record: &MeasurementRecord) -> fmt::Result {
    let falsifier = record.falsifier();
    let unit = record.threshold().unit();
    writeln!(text, "  [{}] {}", record.verdict().name(), falsifier.name())?;
    writeln!(text, "    workload: {}", falsifier.workload())?;
    writeln!(text, "    definition: {}", falsifier.definition())?;
    writeln!(text, "    action: {}", falsifier.action())?;
    writeln!(text, "    slope: {} {unit}", record.slope())?;
    writeln!(
        text,
        "    interval: {}..{} {unit}",
        record.interval().low(),
        record.interval().high()
    )?;
    writeln!(text, "    verdict: {}", record.verdict().name())?;
    writeln!(
        text,
        "    hardware_class: {}",
        record.hardware_class().name()
    )?;
    writeln!(
        text,
        "    toolchain_version: {}",
        record.toolchain_version()
    )?;
    writeln!(text, "    commit: {}", record.commit())?;
    writeln!(text, "    sample_count: {}", record.sample_count())?;
    writeln!(text, "    threshold: {}", record.threshold().describe())?;
    Ok(())
}

fn write_coverage(text: &mut String, result_mode: &ResultMode) -> fmt::Result {
    match result_mode {
        ResultMode::Exact => writeln!(text, "coverage: none"),
        ResultMode::Coverage(descriptor) => {
            writeln!(text, "coverage: marked")?;
            writeln!(
                text,
                "  answered_shards: {}",
                shard_list(descriptor.answered_shards())
            )?;
            writeln!(
                text,
                "  missing_shards: {}",
                shard_list(descriptor.missing_shards())
            )?;
            writeln!(
                text,
                "  served_versions: {}",
                served_version_list(descriptor.served_versions())
            )
        }
    }
}

fn shard_list(shards: &[ShardId]) -> String {
    if shards.is_empty() {
        return "none".to_owned();
    }
    let mut text = String::new();
    for (index, shard) in shards.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&shard_label(shard));
    }
    text
}

fn served_version_list(entries: &[(ShardId, CommitVersion)]) -> String {
    if entries.is_empty() {
        return "none".to_owned();
    }
    let mut text = String::new();
    for (index, (shard, version)) in entries.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&shard_label(shard));
        text.push('=');
        text.push_str(&version.to_string());
    }
    text
}

fn shard_label(shard: &ShardId) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(shard.as_bytes().len() * 2);
    for &byte in shard.as_bytes() {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::CoverageDescriptor;
    use crate::fixed::Fixed;
    use crate::gate::GateOutcome;
    use crate::measure::{Interval, MeasurementParts, Verdict};

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

    fn results() -> Vec<FalsifierResult> {
        vec![
            FalsifierResult::Measured(record(Falsifier::SyncAmplification, 20_000)),
            FalsifierResult::Measured(record(Falsifier::PolicyCost, 4_000_000)),
            FalsifierResult::Measured(record(Falsifier::ConflictRate, 20_000)),
            FalsifierResult::Measured(record(Falsifier::ColdStart, 1_200_000)),
            FalsifierResult::Measured(record(Falsifier::ReadP99, 50_000)),
        ]
    }

    fn input() -> ReportInput {
        ReportInput::new(
            WorkloadSpec::candidate(),
            HardwareClass::Ci,
            "rustc 1.99.1",
            "0123456",
            results(),
        )
        .with_scope_budget(ScopeBudget::new(1_000, 900, 50))
        .with_ranking_cost(RankingCost::document_local(100, 50))
    }

    #[test]
    fn one_report_is_stable_and_names_every_field() {
        let first = input().write();
        let second = input().write();
        assert_eq!(first, second);
        assert!(first.contains("oxkv falsifier report v1"));
        assert!(first.contains("result_mode: exact"));
        assert!(first.contains("gate: pass"));
        for falsifier in Falsifier::ALL {
            assert!(first.contains(falsifier.name()));
        }
        assert!(first.contains("slope: 0.020000 per_decade"));
        assert!(first.contains("threshold: 0.050000 per_decade candidate"));
        assert!(first.contains("sample_count: 30"));
        assert!(first.contains("hardware_class: ci"));
        assert!(first.contains("toolchain_version: rustc 1.99.1"));
        assert!(first.contains("commit: 0123456"));
    }

    #[test]
    fn a_coverage_result_is_marked() {
        let descriptor = CoverageDescriptor::new(
            vec![ShardId::new(vec![1])],
            vec![ShardId::new(vec![2])],
            vec![(ShardId::new(vec![1]), CommitVersion::new(10))],
        );
        let report = input()
            .with_result_mode(ResultMode::Coverage(descriptor))
            .write();
        assert!(report.contains("result_mode: coverage"));
        assert!(report.contains("coverage: marked"));
        assert!(report.contains("missing_shards: 02"));
        assert!(report.contains("served_versions: 01=10"));
    }

    #[test]
    fn a_missing_falsifier_is_written_as_missing() {
        let mut results = results();
        results.pop();
        let input = ReportInput::new(
            WorkloadSpec::candidate(),
            HardwareClass::Ci,
            "rustc 1.99.1",
            "0123456",
            results,
        );
        let report = input.write();
        assert!(report.contains("[missing] read_p99"));
        assert!(report.contains("gate: delayed"));
    }

    #[test]
    fn a_breach_is_written_with_its_action() {
        let mut results = results();
        results[0] = FalsifierResult::Measured(record(Falsifier::SyncAmplification, 100_000));
        let input = ReportInput::new(
            WorkloadSpec::candidate(),
            HardwareClass::Ci,
            "rustc 1.99.1",
            "0123456",
            results,
        );
        let report = input.write();
        assert!(report.contains("gate: fail"));
        assert!(report.contains("[breach] sync_amplification"));
        assert!(report.contains("action: Add per-scope materialized views at the authority."));
        assert_eq!(
            input.results[0].measured().map(MeasurementRecord::verdict),
            Some(Verdict::Breach)
        );
    }

    #[test]
    fn an_empty_report_does_not_panic() {
        let input = ReportInput::new(
            WorkloadSpec::candidate(),
            HardwareClass::Ci,
            "rustc 1.99.1",
            "0123456",
            Vec::new(),
        );
        let report = input.write();
        assert!(report.ends_with('\n'));
        assert!(report.contains("gate: delayed"));
        assert_eq!(input.gate().outcome(), GateOutcome::Delayed);
    }
}
