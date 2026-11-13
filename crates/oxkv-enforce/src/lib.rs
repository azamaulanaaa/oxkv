//! Deterministic write enforcement and the per-shard audit record.
//!
//! Enforcement runs at commit. It pins one policy version, evaluates each row
//! against the pinned fact projection, and writes one audit record for one
//! write request. A batch verdict is the conjunction of its row verdicts.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It does not read a clock, draw randomness, use floating point, perform I/O,
//! or use `unsafe`. The authority injects the queue deadline and the current
//! fact versions.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::{FactSourceId, FactVersion, ProjectionEpoch, ProjectionWatermarks};
//! use oxkv_enforce::{
//!     FactProjection, ProjectionWriteOrigin,
//! };
//!
//! let mut watermarks = ProjectionWatermarks::new();
//! watermarks.set(FactSourceId::new("accounts"), FactVersion::new(10));
//! let projection = FactProjection::new(ProjectionEpoch::new(7), watermarks);
//! assert_eq!(
//!     projection.admit_write(ProjectionWriteOrigin::Client),
//!     Err(oxkv_enforce::ProjectionError::ClientWriteRejected),
//! );
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

mod codec;

pub mod audit;
pub mod auth;
pub mod enforcement;
pub mod policy;
pub mod projection;
pub mod request;
pub mod state;

pub use audit::{
    AuditCheckpoint, AuditError, AuditRecord, AuditRecordBody, AuditSink, AuditVerdict,
    InMemoryAuditChain,
};
pub use auth::{AuthError, Authenticator, Principal};
pub use enforcement::{
    CommitContext, Enforcement, EnforcementError, PolicyCatalogEntry, PolicyPin, QueueDeadline,
    WriteOutcome,
};
pub use policy::{
    Decision, DecisionVerdict, Evaluation, PolicyEvaluator, ReasonCode, RowContext, RuleId,
};
pub use projection::{
    AuthorizationMode, AuthorizationPolicy, FactDistance, FactProjection, ProjectionError,
    ProjectionWriteOrigin, StalenessBound, StalenessReport,
};
pub use request::{MAX_BATCH_ROWS, RowKey, RowMutation, WriteRequest};
pub use state::{BusinessEvent, CommittedEffects, IndexEntry, ShardState, StoredRow};
