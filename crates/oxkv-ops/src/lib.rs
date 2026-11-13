//! Portable operations contracts for oxkv.
//!
//! This crate holds pure records and policies for the operational surfaces:
//!
//! - [`migration`] defines one framework with artifact, shard format, and key
//!   encoding scopes.
//! - [`backup`] captures shard snapshots with a version vector and a retention
//!   pin.
//! - [`restore`] verifies every object and writes a new epoch.
//! - [`quota`] rejects at admission with a named error.
//! - [`compaction`] controls budgets, pauses, and pins.
//! - [`telemetry`] records metric samples and write, query, and sync traces.
//! - [`control`] names audited control operations for the control RPC.
//! - [`freeze`] carries the support window and downgrade policy.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses no floating point, no clock, no randomness, no network, no I/O, and
//! no `unsafe` code. Time, storage, and quiesce state are injected.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::ShardId;
//! use oxkv_ops::maintenance::FreezeState;
//!
//! let mut freeze = FreezeState::new();
//! let shard = ShardId::new(vec![1]);
//! freeze.freeze_shard(shard.clone());
//! let error = freeze.admit_write(&shard).expect_err("the write is refused");
//! assert_eq!(error.code(), "shard.frozen_for_maintenance");
//! FreezeState::admit_read();
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod artifact;
pub mod backup;
pub mod compaction;
pub mod control;
pub mod error;
pub mod freeze;
pub mod maintenance;
pub mod migration;
pub mod quota;
pub mod receipts;
pub mod restore;
pub mod snapshot;
pub mod telemetry;

pub use artifact::{
    ArtifactMigrationPlan, ArtifactMigrationReport, ArtifactObject, LayoutChange,
    MigratableArtifact, MigratedArtifact,
};
pub use backup::{
    BackupImage, BackupManifest, BackupObjectRef, BackupOutcome, BackupPin, BackupPlan,
    BackupVersions, ConsistencyPoint, PinLedger, PinState, QuiesceState, capture,
};
pub use compaction::{
    CompactionBudget, CompactionPass, CompactionPolicy, CompactionReport, CompactionState,
};
pub use control::{
    ControlAuditRecord, ControlOperation, ControlResult, ControlVersion, audit_control,
};
pub use error::OpsError;
pub use freeze::{READER_WINDOW, SupportPolicy};
pub use maintenance::{FreezeState, MaintenanceWindow, Timestamp, WriteGate};
pub use migration::{
    KeyEncodingMigration, KeyEncodingReport, KeyTranslation, MigrationCheckpoint, MigrationPhase,
    MigrationPlan, MigrationScope, MigrationStep, RollbackOutcome, ShardFormatPlan, ShardMigration,
    ShardStore,
};
pub use quota::{
    OperationClass, QuotaLimits, QuotaPolicy, QuotaRequest, QuotaReserve, QuotaResource,
    QuotaUsage, RateLimitKey, RateLimitSubject, RateLimiter, TokenBucket,
};
pub use receipts::{
    AuthorityReceiptRecord, ClientOutboxEntry, DrainReport, PendingWrite, RebaseAction, RebasePath,
    SchedulerState, rebase_path,
};
pub use restore::{ExpiredState, RestorePlan, RestoreSource, RestoredEpoch, restore};
pub use snapshot::{AuditState, EffectRecord, EventOutboxEntry, IdempotencyRecord, SnapshotObject};
pub use telemetry::{
    MetricName, MetricSample, MetricValue, TelemetryBatch, Trace, TraceKind, TraceSpan,
};
