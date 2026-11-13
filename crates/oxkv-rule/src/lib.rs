//! Portable rule lifecycle for oxkv.
//!
//! This crate holds the policy catalog and the rule lifecycle state machine:
//!
//! - [`manifest`] declares rule metadata. It does not hold a rule body.
//! - [`policy`] holds immutable content-addressed policy versions.
//! - [`catalog`] stores each policy version as an immutable catalog record.
//! - [`audit`] appends one hash-linked audit record per change.
//! - [`lifecycle`] runs the dry run, staged rollout, canary, promotion, and
//!   rollback steps.
//! - [`control`] names the `control/rule_activate` and `control/rule_rollback`
//!   RPC methods.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It reads no clock, draws no randomness, performs no I/O, uses no floating
//! point, and contains no `unsafe` code. Time, approval, and rollout state
//! enter through request values.
//!
//! # Example
//!
//! ```
//! use oxkv_rule::{RetentionFloor, RULE_ERROR_CODES};
//!
//! assert_eq!(RetentionFloor::new(2).generation(), 2);
//! assert!(RULE_ERROR_CODES.contains(&"rule.dry_run_failed"));
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod audit;
pub mod catalog;
pub mod control;
pub mod error;
pub mod lifecycle;
pub mod manifest;
pub mod policy;

mod canonical;

pub use audit::{AuditEntry, PolicyAuditLog, PolicyAuditRecord};
pub use catalog::{PolicyCatalog, PolicyRecord, RetentionFloor, catalog_shard_id};
pub use control::{
    AuthClass, CONTROL_NAMESPACE, CONTROL_OPERATIONS, ControlOperation, DATA_AUTH_CLASS,
    RULE_ACTIVATE, RULE_ROLLBACK, error_map, error_spec, operation,
};
pub use error::{RULE_ERROR_CODES, RuleError};
pub use lifecycle::{
    ActivationRequest, ApprovalSet, Approver, ChangeClass, Cohort, CohortKind, DryRun, DryRunDiff,
    DryRunReport, DryRunSample, InjectedTime, LifecycleStep, NoticeFeed, NoticeKind,
    ProjectionStamp, PromotionRequest, Role, RollbackRequest, Rollout, RolloutObservation,
    RolloutTick, RuleChange, RuleLifecycle, RuleNotice, RuleReceipt, SampleRow, StructuralDiff,
    Verdict, VerdictFlip,
};
pub use manifest::{
    AuditVisibility, CostBound, FactReadSet, RetentionClass, RuleEffect, RuleId, RuleManifest,
    RuleMetadata, RuleScope, RuleSelector,
};
pub use policy::{
    CompiledPolicy, CompiledRule, PolicyContent, PolicyId, RuleBody, SUPPORTED_POLICY_AST_VERSION,
};
