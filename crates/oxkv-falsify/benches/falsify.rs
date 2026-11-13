#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

//! Offline benchmark targets for the falsifier contract.
//!
//! The benchmarks use one deterministic workload. They start no service, open
//! no socket, and read no clock outside the `criterion` harness. Run them with
//! `cargo bench -p oxkv-falsify`.
//!
//! The targets are:
//!
//! - `workload_replay`: one full deterministic replay of the candidate spec.
//! - `fixed_slope`: one integer slope computation.
//! - `gate_evaluation`: one release gate over five measurements.
//! - `report_write`: one deterministic text report.
//! - `threshold_lookup`: one lookup of every candidate threshold.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
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
        .expect("the benchmark interval is valid"),
        hardware_class: HardwareClass::Ci,
        toolchain_version: "rustc benchmark",
        commit: "benchmark",
        sample_count: 30,
    })
    .expect("the benchmark record is valid")
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

fn benchmark_workload_replay(criterion: &mut Criterion) {
    let spec = WorkloadSpec::candidate();
    criterion.bench_function("workload_replay", |bencher| {
        bencher.iter(|| {
            let workload = Workload::replay(black_box(&spec)).expect("the replay succeeds");
            black_box(workload.operation_hash());
        });
    });
}

fn benchmark_fixed_slope(criterion: &mut Criterion) {
    criterion.bench_function("fixed_slope", |bencher| {
        bencher.iter(|| {
            black_box(Fixed::slope_per_decade(
                black_box(1_000),
                black_box(1_100),
                black_box(2),
            ));
        });
    });
}

fn benchmark_gate_evaluation(criterion: &mut Criterion) {
    let results = results();
    criterion.bench_function("gate_evaluation", |bencher| {
        bencher.iter(|| {
            black_box(oxkv_falsify::ReleaseGate::evaluate(black_box(&results)));
        });
    });
}

fn benchmark_report_write(criterion: &mut Criterion) {
    let input = ReportInput::new(
        WorkloadSpec::candidate(),
        HardwareClass::Ci,
        "rustc benchmark",
        "benchmark",
        results(),
    )
    .with_scope_budget(ScopeBudget::new(1_000_000, 900_000, 50_000))
    .with_ranking_cost(RankingCost::document_local(100, 50));
    criterion.bench_function("report_write", |bencher| {
        bencher.iter(|| {
            black_box(input.write());
        });
    });
}

fn benchmark_threshold_lookup(criterion: &mut Criterion) {
    criterion.bench_function("threshold_lookup", |bencher| {
        bencher.iter(|| {
            for falsifier in Falsifier::ALL {
                black_box(falsifier.threshold());
            }
        });
    });
}

criterion_group!(
    benches,
    benchmark_workload_replay,
    benchmark_fixed_slope,
    benchmark_gate_evaluation,
    benchmark_report_write,
    benchmark_threshold_lookup,
);
criterion_main!(benches);
