//! Scope declaration, identity, membership, diffs, and result modes.
//!
//! A scope is a policy-filtered query over one or more user table prefixes. It
//! is a view, not a cache. This crate supplies the data types that the
//! authority, the replica, and the search index share:
//!
//! - [`declaration`] declares the query as data.
//! - [`identity`] computes the deterministic scope identity.
//! - [`version`] keeps the scope version vector monotone.
//! - [`diff`] carries add and remove intervals with per-version churn.
//! - [`membership`] holds exact logical members at one pin.
//! - [`plan`] compiles a cross-prefix scope into one shard cursor per shard.
//! - [`result`] separates exact results from coverage results.
//! - [`residency`] keeps page residency separate from membership.
//!
//! The crate does not implement storage, the replica mirror, or the search
//! index. It uses no clock, no randomness, no I/O, no floating point, and no
//! `unsafe`. It compiles for native and `wasm32-unknown-unknown`.
//!
//! # Result modes
//!
//! Exact mode needs every targeted shard at the pin. A missing shard returns
//! `scope.shard_unreachable` and no data. Coverage mode returns a
//! [`result::CoverageResult`] with a descriptor. The descriptor lists answered
//! shards, missing shards, and served versions. A coverage result never claims
//! exactness, even when the descriptor is complete.
//!
//! # Budgets
//!
//! Admission limits apply at subscribe. An over-limit scope returns
//! `scope.over_limit` and transfers no data. Device and hydration budgets
//! return `scope.device_budget_exceeded` and
//! `scope.hydration_budget_exceeded`. A degraded scope is marked degraded and
//! does not claim exact offline.
//!
//! # Example
//!
//! ```
//! use oxkv_scope::{
//!     AdmissionLimits, Ordering, PolicySelector, ProjectionSelector, RuleSetSelector,
//!     ScopeDeclaration, TablePrefix,
//! };
//!
//! let declaration = ScopeDeclaration::new(
//!     [TablePrefix::parse("accounts").expect("valid table")],
//!     PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
//!     ProjectionSelector::all(),
//!     Ordering::CanonicalKey,
//!     AdmissionLimits::unbounded(),
//! )
//! .expect("valid declaration");
//!
//! assert_eq!(declaration.identity(), declaration.identity());
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod declaration;
pub mod diff;
pub mod error;
pub mod identity;
pub mod membership;
pub mod plan;
pub mod residency;
pub mod result;
pub mod version;

pub use declaration::{
    AdmissionLimits, AdmissionMode, AdmissionReport, DegradationReason, FieldName, Ordering,
    PolicySelector, ProjectionSelector, RuleSetSelector, ScopeDeclaration, ScopeStats,
    SubscribeBudget, TablePrefix,
};
pub use diff::{AddRecord, Churn, DiffInterval, DiffLog, DiffRecord, RemoveReason, RemoveRecord};
pub use error::{AdmissionLimit, QueryError, ResidencyError, ScopeError};
pub use identity::scope_identity;
pub use membership::{LocalRead, Member, Membership, ScopeMaterialization};
pub use plan::{ShardAssignment, ShardCursor, ShardPlan, ShardPlanEntry};
pub use residency::{EvictedPage, EvictionReason, ReplicaResidency, ResidencyPage};
pub use result::{
    CoverageDescriptor, CoverageResult, ExactResult, QueryMode, QueryOutcome, ResultMode, ScopePin,
    ScopeRow, ShardResponse, ShardServed,
};
pub use version::{ScopeVersion, version_vector};
