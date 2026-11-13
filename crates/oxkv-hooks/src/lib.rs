//! Pure pre-write validation and post-write notification hooks.
//!
//! The crate supplies one hook pipeline. The pipeline runs pre-write
//! validation before a commit. The pipeline runs post-write derivation inside
//! the commit step.
//!
//! A pre-write hook returns a verdict only. It cannot rewrite the batch. A
//! post-write hook returns an ordered notification or no notification. Both
//! hook kinds read one explicit input envelope. Neither hook reads a clock,
//! draws randomness, performs I/O, or keeps state.
//!
//! The pipeline is deterministic. Operations apply in declared order. A
//! notification follows the same order. A replay of one operation log
//! produces the same notification bytes.
//!
//! A notification uses one version byte and fixed-width big-endian lengths.
//! The encoding has no target-dependent field. The golden vector in
//! `tests/vectors/hooks.json` pins the bytes for native and `wasm32`.
//!
//! # Contract rules
//!
//! - A pre-write rejection returns an error. It produces no commit outcome.
//! - A post-write failure aborts the batch. It produces no commit outcome.
//! - A validator reads the pre-transaction value for the key that it
//!   validates. It reads the final staged value for every other touched key.
//! - One pipeline owns one ordered list of pre-write rules and one ordered
//!   list of post-write rules.
//! - A duplicate rule id in one phase fails the build with
//!   `internal.invariant_violation`.
//!
//! The error codes come from [`oxkv_contracts::error`]. The crate uses no
//! floating point and no `unsafe` code. It compiles for native and
//! `wasm32-unknown-unknown`.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::{CommitVersion, KeyVersion, ShardId};
//! use oxkv_hooks::{
//!     HookError, HookPipeline, PostWriteHook, PreWriteHook, PriorRow, RuleId, StagedOp,
//!     Transaction, CommittedView, EventDraft, EventKind, ValidatorView,
//! };
//!
//! struct AllowAll(RuleId);
//!
//! impl PreWriteHook for AllowAll {
//!     fn rule_id(&self) -> &RuleId {
//!         &self.0
//!     }
//!
//!     fn validate(&self, _view: &ValidatorView<'_>) -> Result<(), HookError> {
//!         Ok(())
//!     }
//! }
//!
//! struct Emit(RuleId);
//!
//! impl PostWriteHook for Emit {
//!     fn rule_id(&self) -> &RuleId {
//!         &self.0
//!     }
//!
//!     fn derive(&self, view: &CommittedView<'_>) -> Result<Option<EventDraft>, HookError> {
//!         Ok(Some(EventDraft::new(
//!             EventKind::new("key_changed"),
//!             view.key().as_bytes().to_vec(),
//!         )))
//!     }
//! }
//!
//! let mut builder = HookPipeline::builder();
//! builder.register_pre(AllowAll(RuleId::new("rule.allow"))).expect("the rule is unique");
//! builder.register_post(Emit(RuleId::new("rule.emit"))).expect("the rule is unique");
//! let pipeline = builder.build();
//!
//! let mut transaction = Transaction::new(ShardId::new(b"accounts".to_vec()));
//! transaction
//!     .stage(StagedOp::put(
//!         "accounts/1".into(),
//!         b"value".to_vec(),
//!         KeyVersion::new(1),
//!         Some(PriorRow::new(b"old".to_vec(), KeyVersion::new(1))),
//!     ))
//!     .expect("the batch is valid");
//!
//! let outcome = pipeline
//!     .execute(&transaction, CommitVersion::new(2))
//!     .expect("the write is accepted");
//! assert_eq!(outcome.notifications().len(), 1);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

mod batch;
mod budget;
mod error;
mod event;
mod pipeline;
mod post;
mod pre;
mod replay;
mod rule;

pub use batch::{Key, OpKind, PriorRow, StagedOp, Transaction};
pub use budget::HookBudget;
pub use error::{HookError, HookErrorCode, PipelineBuildError};
pub use event::{EventDraft, EventId, EventKind, Notification};
pub use pipeline::{CommitOutcome, HookPipeline, HookPipelineBuilder, ValidatedBatch};
pub use post::{CommittedView, PostWriteHook};
pub use pre::{PreWriteHook, ValidatorView};
pub use replay::{ReplayRecord, replay, revalidate};
pub use rule::RuleId;
