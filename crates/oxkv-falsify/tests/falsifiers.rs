//! Integration tests for the falsifier contract.
//!
//! The tests pin the five thresholds, the workload hashes, the gate behavior,
//! and the deterministic report.

use oxkv_contracts::version::{CommitVersion, ShardId};
use oxkv_falsify::{
    CoverageDescriptor, Falsifier, FalsifierResult, Fixed, GateOutcome, GateStatus, HardwareClass,
    Interval, MeasurementParts, MeasurementRecord, RankingCost, ReleaseGate, ReportInput,
    ResultMode, ScopeBudget, Verdict, Workload, WorkloadSpec,
};

fn record(falsifier: Falsifier, micros: i64) -> MeasurementRecord {
    MeasurementRecord::new(MeasurementParts {
        falsifier,
        slope: Fixed::from_micros(micros),
        interval: Interval::new(
            Fixed::from_micros(micros - 1_000),
            Fixed::from_micros(micros + 1_000),
        )
        .expect("the test interval is valid"),
        hardware_class: HardwareClass::Ci,
        toolchain_version: "rustc 1.99.1",
        commit: "0123456",
        sample_count: 30,
    })
    .expect("the test record is valid")
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

fn golden_report_input() -> ReportInput {
    ReportInput::new(
        WorkloadSpec::candidate(),
        HardwareClass::Ci,
        "rustc 1.99.1",
        "0123456",
        passing_results(),
    )
    .with_scope_budget(ScopeBudget::new(1_000, 900, 50))
    .with_ranking_cost(RankingCost::document_local(100, 50))
}

#[test]
fn the_golden_thresholds_are_pinned() {
    let value: serde_json::Value = serde_json::from_str(include_str!("vectors/falsifiers.json"))
        .expect("the golden file is valid JSON");
    assert_eq!(value["contract"].as_str(), Some("falsifier/v1"));
    let cases = value["cases"]
        .as_array()
        .expect("the golden file has a case list");
    assert_eq!(cases.len(), Falsifier::ALL.len());
    for case in cases {
        let name = case["name"].as_str().expect("the case names a falsifier");
        let falsifier = Falsifier::from_name(name).expect("the golden falsifier is known");
        let threshold_micros = case["threshold_micros"]
            .as_u64()
            .expect("the case has an integer threshold");
        assert_eq!(
            falsifier.threshold().limit().micros(),
            i64::try_from(threshold_micros).expect("the threshold fits")
        );
        assert_eq!(
            falsifier.threshold().unit().name(),
            case["unit"].as_str().expect("the case names a unit")
        );
        assert_eq!(
            falsifier.threshold().is_candidate(),
            case["candidate"]
                .as_bool()
                .expect("the case marks a candidate")
        );
        assert_eq!(
            falsifier.definition(),
            case["definition"]
                .as_str()
                .expect("the case has a definition")
        );
        assert_eq!(
            falsifier.workload(),
            case["workload"].as_str().expect("the case has a workload")
        );
        assert_eq!(
            falsifier.action(),
            case["action"].as_str().expect("the case has an action")
        );
    }
}

#[test]
fn the_workload_hashes_are_pinned() {
    let value: serde_json::Value = serde_json::from_str(include_str!("vectors/falsifiers.json"))
        .expect("the golden file is valid JSON");
    let spec = WorkloadSpec::candidate();
    assert_eq!(
        spec.hash().to_hex(),
        value["spec_hash"]
            .as_str()
            .expect("the file has a spec hash")
    );
    let workload = Workload::replay(&spec).expect("the candidate spec replays");
    assert_eq!(
        workload.operation_hash().to_hex(),
        value["operation_hash"]
            .as_str()
            .expect("the file has an operation hash")
    );
}

#[test]
fn each_falsifier_reports_its_record_fields() {
    for falsifier in Falsifier::ALL {
        let measured = record(falsifier, falsifier.threshold().limit().micros() - 1);
        assert_eq!(measured.falsifier(), falsifier);
        assert_eq!(measured.verdict(), Verdict::Pass);
        assert_eq!(measured.hardware_class(), HardwareClass::Ci);
        assert_eq!(measured.toolchain_version(), "rustc 1.99.1");
        assert_eq!(measured.commit(), "0123456");
        assert_eq!(measured.sample_count(), 30);
        assert_eq!(measured.threshold(), falsifier.threshold());
        assert!(measured.interval().contains(measured.slope()));
    }
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
    assert!(entry.detail().contains("measured 0.100000 per_decade"));
    assert!(entry.detail().contains("threshold 0.050000 per_decade"));
}

#[test]
fn a_missing_number_delays_the_tag() {
    let mut results = passing_results();
    results.pop();
    let report = ReleaseGate::evaluate(&results);
    assert_eq!(report.outcome(), GateOutcome::Delayed);
    assert!(!report.allows_tag());
    assert_eq!(
        report
            .entry(Falsifier::ReadP99)
            .expect("the entry exists")
            .status(),
        GateStatus::Missing
    );
}

#[test]
fn a_skipped_falsifier_fails_the_gate() {
    let mut results = passing_results();
    results[3] = FalsifierResult::skipped(Falsifier::ColdStart, "The reference machine is down.");
    let report = ReleaseGate::evaluate(&results);
    assert_eq!(report.outcome(), GateOutcome::Fail);
    assert!(!report.allows_tag());
    assert_eq!(
        report
            .entry(Falsifier::ColdStart)
            .expect("the entry exists")
            .status(),
        GateStatus::Skipped
    );
}

#[test]
fn the_report_matches_the_golden_text() {
    let report = golden_report_input().write();
    assert_eq!(report, include_str!("vectors/report.txt"));
}

#[test]
fn the_report_is_stable_across_runs() {
    assert_eq!(golden_report_input().write(), golden_report_input().write());
}

#[test]
fn a_coverage_result_is_marked() {
    let descriptor = CoverageDescriptor::new(
        vec![ShardId::new(vec![1])],
        vec![ShardId::new(vec![2])],
        vec![(ShardId::new(vec![1]), CommitVersion::new(10))],
    );
    let report = golden_report_input()
        .with_result_mode(ResultMode::Coverage(descriptor))
        .write();
    assert!(report.contains("result_mode: coverage"));
    assert!(report.contains("coverage: marked"));
    assert!(report.contains("missing_shards: 02"));
    assert!(report.contains("served_versions: 01=10"));
}

#[test]
fn the_scope_budget_uses_the_declared_device_budget() {
    let fits = ScopeBudget::new(1_000, 900, 50);
    assert!(fits.fits());
    assert_eq!(fits.headroom_bytes(), 50);
    let over = ScopeBudget::new(1_000, 900, 101);
    assert!(!over.fits());
    let report = golden_report_input().with_scope_budget(over).write();
    assert!(report.contains("fits_declared_device_budget: false"));
    assert!(report.contains("declared_device_budget_bytes: 1000"));
}

#[test]
fn ranking_cost_uses_document_local_scoring() {
    let cost = RankingCost::document_local(100, 50);
    assert_eq!(cost.total_term_visits(), 5_000);
    let report = golden_report_input().with_ranking_cost(cost).write();
    assert!(report.contains("document_local_terms: 50"));
    assert!(report.contains("document_local_only: true"));
}

#[test]
fn a_replay_is_deterministic() {
    let spec = WorkloadSpec::candidate();
    let first = Workload::replay(&spec).expect("the first replay succeeds");
    let second = Workload::replay(&spec).expect("the second replay succeeds");
    assert_eq!(first.operation_hash(), second.operation_hash());
    assert_eq!(first.operations(), second.operations());
    assert!(first.logical_ticks() >= first.operations().len() as u64);
}
