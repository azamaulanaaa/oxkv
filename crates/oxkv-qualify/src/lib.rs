//! The portable oxkv qualification suite.
//!
//! The crate holds:
//!
//! - [`scenario`] runs one definition on native, browser-shaped, and native
//!   again. A scenario never reads the target.
//! - [`fault`] injects a link cut, a cursor expiry, a conditional-write loss,
//!   a corrupt page, a truncated frame, a crash at each commit step, and a
//!   duplicate delivery.
//! - [`vectors`] holds the append-only golden corpus for every core contract
//!   and annex.
//! - [`differential`] compares targets, versions, and expected outputs.
//! - [`fuzz`] holds the deterministic decoder and policy probes.
//! - [`report`] produces one report and one narration from one trace.
//! - [`tiers`] models the release tiers as policy data.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses no floating point. It uses no `unsafe`. It does not read a clock,
//! draw operating-system randomness, use a network, or perform I/O on a
//! semantic path.
//!
//! # Example
//!
//! ```
//! use oxkv_qualify::suite::Suite;
//!
//! let outcome = Suite::new().run();
//! assert!(outcome.passed(), "{}", outcome.report.render());
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod canonical;
pub mod clock;
pub mod differential;
pub mod fault;
pub mod fuzz;
pub mod policy;
pub mod register;
pub mod report;
pub mod rng;
pub mod scenario;
pub mod suite;
pub mod target;
pub mod tiers;
pub mod trace;
pub mod vectors;
pub mod world;

pub use fault::{CommitStep, Fault, FaultKind, FaultPlan};
pub use register::{ContractRow, Surface};
pub use report::{Narration, Report};
pub use scenario::{FIXED_SEED, ScenarioCase, ScenarioKind, ScenarioRun, default_cases, run_case};
pub use suite::{Suite, SuiteOutcome};
pub use target::Target;
pub use trace::NormalizedTrace;
