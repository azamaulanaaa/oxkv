//! The falsifier benchmark contract for oxkv 1.0.
//!
//! The crate defines five falsifiers. Each falsifier names one workload, one
//! definition, one threshold, and one action.
//!
//! - [`falsifier`] defines the five falsifiers and their candidate thresholds.
//! - [`workload`] generates a deterministic workload from a fixed seed and a
//!   logical clock.
//! - [`measure`] records the measured slope, interval, verdict, hardware
//!   class, toolchain version, commit, sample count, and threshold.
//! - [`gate`] fails a release on a breach, fails on a skip, and delays a tag
//!   when a falsifier number is missing.
//! - [`report`] writes one deterministic text report from the measurements.
//! - [`budget`] holds the scope budget, ranking cost, and coverage records.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses no floating point in a stored byte form. It does not read a wall
//! clock, draw operating-system entropy, or perform I/O on a semantic path.
//!
//! # Example
//!
//! ```
//! use oxkv_falsify::Falsifier;
//!
//! let threshold = Falsifier::SyncAmplification.threshold();
//! assert!(threshold.is_candidate());
//! assert_eq!(Falsifier::SyncAmplification.name(), "sync_amplification");
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod budget;
pub mod falsifier;
pub mod fixed;
pub mod gate;
pub mod measure;
pub mod report;
pub mod workload;

pub use budget::{CoverageDescriptor, RankingCost, ResultMode, ScopeBudget};
pub use falsifier::{Falsifier, Threshold, ThresholdUnit};
pub use fixed::Fixed;
pub use gate::{GateEntry, GateOutcome, GateReport, GateStatus, ReleaseGate};
pub use measure::{
    FalsifierResult, HardwareClass, Interval, MeasurementError, MeasurementParts,
    MeasurementRecord, Verdict,
};
pub use report::{ReportInput, ReportWriter};
pub use workload::{
    Axis, ConcurrencySchedule, LogicalClock, Operation, OperationKind, Seed, ShardMap, SplitMix64,
    TraceKind, Workload, WorkloadError, WorkloadSpec,
};
