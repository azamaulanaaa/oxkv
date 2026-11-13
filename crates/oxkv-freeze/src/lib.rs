//! The 1.0 freeze register and support policy for oxkv.
//!
//! This crate holds the frozen surfaces that a 1.x consumer can depend on:
//!
//! - [`register`] lists the five core contracts and the twelve annexes.
//! - [`state`] defines the `draft`, `candidate`, and `frozen` states.
//! - [`window`] defines the format reader window and the wire read window.
//! - [`support`] defines the support window and the no-downgrade rule.
//! - [`corpus`] checks that a vector corpus covers every register row.
//! - [`gate`] rejects a freeze claim that names no register row.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses no floating point. It does not read a clock, draw randomness, or
//! perform I/O on a semantic path.
//!
//! # Example
//!
//! ```
//! use oxkv_freeze::{find, ContractId, FreezeState};
//!
//! let row = find(ContractId::C1).expect("C1 is registered");
//! assert_eq!(row.version.as_str(), "oxkv/manifest/v1");
//! assert_eq!(row.state, FreezeState::Frozen);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod corpus;
pub mod gate;
pub mod register;
pub mod state;
pub mod support;
pub mod window;

pub use corpus::{CorpusEntry, CoverageError, VectorCorpus, check_corpus};
pub use gate::{
    EssayFreezeClaim, GateError, check_claim, check_claim_coverage, check_claims,
    check_freeze_statement,
};
pub use register::{
    ContractId, ContractVersion, REGISTER, RegisterError, RegisterRow, RowField, SurfaceDependency,
    find, is_valid_version_text, require_row, validate_register,
};
pub use state::{FreezeState, FreezeTransitionError, Phase, Release, state_at_release, transition};
pub use support::{
    FIXES_AFTER_NEXT_MAJOR_MONTHS, FormatGuard, REMOVAL_NOTICE_MONTHS, RemovalPlan,
    SECURITY_FIXES_LAST_MINOR_MONTHS, SUPPORTED_MINORS, SupportError, SupportPolicy,
};
pub use window::{
    ReaderWindow, UNSUPPORTED_FORMAT_CODE, UNSUPPORTED_WIRE_CODE, WindowError, WireWindow,
    WireWindowError,
};
