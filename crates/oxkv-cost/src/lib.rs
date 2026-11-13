//! The oxkv query cost model.
//!
//! The crate provides six surfaces:
//!
//! - [`plan`] builds a rule-based query plan and its cost.
//! - [`budget`] checks a plan against the query cost and fan-out limits.
//! - [`explain`] renders one stable canonical explain record.
//! - [`merge`] merges ordered shard streams under one total order.
//! - [`coverage`] implements exact and coverage result modes.
//! - [`cursor`] extends the contract [`oxkv_contracts::CostCursor`].
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses integer arithmetic only. It does not read a clock, draw randomness,
//! use a regular expression, or perform I/O.
//!
//! # Example
//!
//! ```
//! use oxkv_cost::budget::CostBudget;
//! use oxkv_cost::plan::{CostInputs, MergeShape, QueryPlan, ShardPlan};
//! use oxkv_contracts::{
//!     ManifestRevision, MapVersion, PolicyVersion, ShardId, StatisticsVersion,
//! };
//!
//! let inputs = CostInputs::new(
//!     ManifestRevision::new(7),
//!     PolicyVersion::from_bytes([1; 32]),
//!     MapVersion::new(3),
//!     Some(StatisticsVersion::new(2)),
//! );
//! let plan = QueryPlan::new(
//!     inputs,
//!     "term",
//!     MergeShape::Search,
//!     vec![ShardPlan::new(ShardId::new(vec![1]), 0, 100, 10, 4)],
//!     20,
//! )
//! .expect("the plan is valid");
//! let budget = CostBudget::new(2000, 2, 100);
//! assert!(budget.admit(&plan).is_ok());
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod budget;
pub mod coverage;
pub mod cursor;
pub mod error;
pub mod explain;
pub mod merge;
pub mod plan;

mod util;

pub use budget::{Admission, CostBudget, Verdict};
pub use coverage::{
    CoverageDescriptor, PageError, PageRequest, QueryPage, ResultMode, ShardReply, assemble_page,
};
pub use cursor::{CostCursor, CostCursorExpiry, CostCursorExt, CostFrontier};
pub use error::{CostCode, CostError};
pub use explain::ExplainRecord;
pub use merge::{
    MergeOptions, MergeOrder, MergeOutput, QueryRow, ShardStream, compare_rows, merge_streams,
};
pub use plan::{COST_CONTRACT_VERSION, CostInputs, MergeShape, QueryPlan, ShardPlan};
