#![allow(missing_docs)]

//! Run one offline falsifier report.
//!
//! The runner uses the candidate workload. It starts no service and opens no
//! socket. Print the deterministic report with:
//!
//! `cargo run -p oxkv-falsify --example falsify_runner`

use oxkv_falsify::{
    Falsifier, FalsifierResult, Fixed, HardwareClass, Interval, MeasurementParts,
    MeasurementRecord, RankingCost, ReportInput, ScopeBudget, Workload, WorkloadSpec,
};

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

fn main() {
    let spec = WorkloadSpec::candidate();
    let workload = Workload::replay(&spec).expect("the candidate spec replays");
    let results = vec![
        FalsifierResult::Measured(record(Falsifier::SyncAmplification, 20_000)),
        FalsifierResult::Measured(record(Falsifier::PolicyCost, 4_000_000)),
        FalsifierResult::Measured(record(Falsifier::ConflictRate, 20_000)),
        FalsifierResult::Measured(record(Falsifier::ColdStart, 1_200_000)),
        FalsifierResult::Measured(record(Falsifier::ReadP99, 50_000)),
    ];
    let input = ReportInput::new(spec, HardwareClass::Ci, "rustc 1.99.1", "0123456", results)
        .with_scope_budget(ScopeBudget::new(1_000, 900, 50))
        .with_ranking_cost(RankingCost::document_local(100, 50));
    println!("spec_hash: {}", workload.spec_hash());
    println!("operation_hash: {}", workload.operation_hash());
    print!("{}", input.write());
}
