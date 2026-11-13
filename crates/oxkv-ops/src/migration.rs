//! One migration framework with three scopes.
//!
//! The scopes are artifact migration, shard format migration, and key encoding
//! migration. A shard migration quiesces writes, drains the scheduler, rewrites
//! the shard under a new epoch, verifies checksums, and flips the pointer.
//! A rollback is allowed only before the first new-format write.

use std::collections::BTreeMap;

use oxkv_contracts::version::{
    ContentDigest, KeyEncodingVersion, LeaseTerm, ShardId, SnapshotPageVersion,
};

use crate::artifact::ArtifactMigrationPlan;
use crate::error::OpsError;
use crate::maintenance::Timestamp;
use crate::receipts::{DrainReport, SchedulerState};

/// The scope of one migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MigrationScope {
    /// Rewrite snapshot and page files.
    Artifact,
    /// Rewrite one shard under a new epoch.
    ShardFormat,
    /// Rewrite the key encoding of one row set.
    KeyEncoding,
}

impl MigrationScope {
    /// Returns the canonical scope name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
            Self::ShardFormat => "shard_format",
            Self::KeyEncoding => "key_encoding",
        }
    }
}

impl core::fmt::Display for MigrationScope {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One shard store at one epoch and format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardStore {
    /// The shard identity.
    pub shard_id: ShardId,
    /// The shard epoch.
    pub epoch: LeaseTerm,
    /// The shard format version.
    pub format: SnapshotPageVersion,
    /// The key encoding annex version.
    pub key_encoding: KeyEncodingVersion,
    /// The user rows.
    pub rows: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl ShardStore {
    /// Creates a shard store.
    #[must_use]
    pub fn new(
        shard_id: ShardId,
        epoch: LeaseTerm,
        format: SnapshotPageVersion,
        key_encoding: KeyEncodingVersion,
        rows: BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Self {
        Self {
            shard_id,
            epoch,
            format,
            key_encoding,
            rows,
        }
    }

    /// Returns one row value.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.rows.get(key).map(Vec::as_slice)
    }

    /// Computes the checksum of the row set.
    ///
    /// The checksum covers every key and value in canonical order. It does not
    /// cover the epoch or the format version.
    #[must_use]
    pub fn rows_digest(&self) -> ContentDigest {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"oxkv/shard-rows/v1");
        bytes.extend_from_slice(
            &u64::try_from(self.rows.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        for (key, value) in &self.rows {
            bytes.extend_from_slice(&u64::try_from(key.len()).unwrap_or(u64::MAX).to_be_bytes());
            bytes.extend_from_slice(key);
            bytes.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            bytes.extend_from_slice(value);
        }
        ContentDigest::of_bytes(&bytes)
    }
}

/// One shard format migration plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardFormatPlan {
    /// The source format version.
    pub from: SnapshotPageVersion,
    /// The target format version.
    pub to: SnapshotPageVersion,
    /// The shard identity.
    pub shard_id: ShardId,
    /// The source epoch.
    pub source_epoch: LeaseTerm,
    /// The target epoch.
    pub target_epoch: LeaseTerm,
}

impl ShardFormatPlan {
    /// Creates a shard format migration plan from N to N+1.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::UnsupportedFormat`] when the source major is not
    /// supported. Returns [`OpsError::MigrationIncomplete`] when the target is
    /// not the next minor version.
    pub fn new(
        shard_id: ShardId,
        from: SnapshotPageVersion,
        to: SnapshotPageVersion,
        source_epoch: LeaseTerm,
    ) -> Result<Self, OpsError> {
        if from.major() != 1 {
            return Err(OpsError::UnsupportedFormat { version: from });
        }
        if from.major() != to.major() || from.minor().saturating_add(1) != to.minor() {
            return Err(OpsError::MigrationIncomplete {
                step: "shard_format_plan",
            });
        }
        Ok(Self {
            from,
            to,
            shard_id,
            source_epoch,
            target_epoch: LeaseTerm::new(source_epoch.get().saturating_add(1)),
        })
    }
}

/// One step of a shard migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MigrationStep {
    /// Stop new writes.
    QuiesceWrites,
    /// Drain the write scheduler.
    DrainScheduler,
    /// Rewrite the shard under the new epoch.
    RewriteEpoch,
    /// Verify counts and checksums.
    VerifyChecksums,
    /// Flip the pointer to the new epoch.
    FlipPointer,
}

impl MigrationStep {
    /// Every migration step in order.
    pub const ALL: [Self; 5] = [
        Self::QuiesceWrites,
        Self::DrainScheduler,
        Self::RewriteEpoch,
        Self::VerifyChecksums,
        Self::FlipPointer,
    ];

    /// Returns the canonical step name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::QuiesceWrites => "quiesce_writes",
            Self::DrainScheduler => "drain_scheduler",
            Self::RewriteEpoch => "rewrite_epoch",
            Self::VerifyChecksums => "verify_checksums",
            Self::FlipPointer => "flip_pointer",
        }
    }
}

impl core::fmt::Display for MigrationStep {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The phase of one shard migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MigrationPhase {
    /// The next step is [`MigrationStep::QuiesceWrites`].
    QuiesceWrites,
    /// The next step is [`MigrationStep::DrainScheduler`].
    DrainScheduler,
    /// The next step is [`MigrationStep::RewriteEpoch`].
    RewriteEpoch,
    /// The next step is [`MigrationStep::VerifyChecksums`].
    VerifyChecksums,
    /// The next step is [`MigrationStep::FlipPointer`].
    FlipPointer,
    /// The migration is complete.
    Complete,
    /// The pointer is back at the source epoch.
    RolledBack,
}

impl MigrationPhase {
    /// Returns the canonical phase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::QuiesceWrites => "quiesce_writes",
            Self::DrainScheduler => "drain_scheduler",
            Self::RewriteEpoch => "rewrite_epoch",
            Self::VerifyChecksums => "verify_checksums",
            Self::FlipPointer => "flip_pointer",
            Self::Complete => "complete",
            Self::RolledBack => "rolled_back",
        }
    }

    /// Returns true when the phase accepts a write.
    #[must_use]
    pub const fn is_writable(self) -> bool {
        matches!(self, Self::Complete | Self::RolledBack)
    }
}

impl core::fmt::Display for MigrationPhase {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The result of one rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackOutcome {
    /// The epoch after the rollback.
    pub epoch: LeaseTerm,
    /// The format after the rollback.
    pub format: SnapshotPageVersion,
}

/// A durable checkpoint of one shard migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationCheckpoint {
    phase: MigrationPhase,
    pointer: LeaseTerm,
    first_new_format_write: bool,
    target: Option<ShardStore>,
    drain: Option<DrainReport>,
    scheduler: SchedulerState,
}

impl MigrationCheckpoint {
    /// Returns the phase at the checkpoint.
    #[must_use]
    pub const fn phase(&self) -> MigrationPhase {
        self.phase
    }

    /// Returns the pointer epoch at the checkpoint.
    #[must_use]
    pub const fn pointer_epoch(&self) -> LeaseTerm {
        self.pointer
    }

    /// Returns true when the checkpoint recorded a new-format write.
    #[must_use]
    pub const fn has_first_new_format_write(&self) -> bool {
        self.first_new_format_write
    }

    /// Returns the drained receipt report.
    #[must_use]
    pub const fn drain(&self) -> Option<&DrainReport> {
        self.drain.as_ref()
    }
}

/// One shard format migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardMigration {
    plan: ShardFormatPlan,
    source: ShardStore,
    target: Option<ShardStore>,
    phase: MigrationPhase,
    pointer: LeaseTerm,
    first_new_format_write: bool,
    scheduler: SchedulerState,
    drain: Option<DrainReport>,
}

impl ShardMigration {
    /// Starts a shard format migration.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MigrationIncomplete`] when the source shard or the
    /// source format does not match the plan. Returns
    /// [`OpsError::UnsupportedFormat`] when the source major is unknown.
    pub fn begin(
        plan: ShardFormatPlan,
        source: ShardStore,
        scheduler: SchedulerState,
    ) -> Result<Self, OpsError> {
        if source.shard_id != plan.shard_id || source.epoch != plan.source_epoch {
            return Err(OpsError::MigrationIncomplete {
                step: "shard_source_epoch",
            });
        }
        if source.format.major() != 1 {
            return Err(OpsError::UnsupportedFormat {
                version: source.format,
            });
        }
        if source.format != plan.from {
            return Err(OpsError::MigrationIncomplete {
                step: "shard_source_format",
            });
        }
        Ok(Self {
            pointer: plan.source_epoch,
            plan,
            source,
            target: None,
            phase: MigrationPhase::QuiesceWrites,
            first_new_format_write: false,
            scheduler,
            drain: None,
        })
    }

    /// Returns the migration phase.
    #[must_use]
    pub const fn phase(&self) -> MigrationPhase {
        self.phase
    }

    /// Returns the pointer epoch.
    #[must_use]
    pub const fn pointer_epoch(&self) -> LeaseTerm {
        self.pointer
    }

    /// Returns the source epoch.
    #[must_use]
    pub const fn source_epoch(&self) -> LeaseTerm {
        self.plan.source_epoch
    }

    /// Returns the target epoch.
    #[must_use]
    pub const fn target_epoch(&self) -> LeaseTerm {
        self.plan.target_epoch
    }

    /// Returns true when writes are frozen.
    #[must_use]
    pub const fn is_frozen(&self) -> bool {
        !self.phase.is_writable()
    }

    /// Returns true when a new-format write occurred.
    #[must_use]
    pub const fn has_first_new_format_write(&self) -> bool {
        self.first_new_format_write
    }

    /// Returns the drained receipt report.
    #[must_use]
    pub const fn drain_report(&self) -> Option<&DrainReport> {
        self.drain.as_ref()
    }

    /// Returns the readable store for one key.
    ///
    /// Reads continue at the old format until the pointer flips.
    #[must_use]
    pub fn read(&self, key: &[u8]) -> Option<&[u8]> {
        if self.pointer == self.plan.target_epoch
            && let Some(target) = &self.target
        {
            return target.get(key);
        }
        self.source.get(key)
    }

    /// Returns the old-format store for one key.
    #[must_use]
    pub fn read_old_format(&self, key: &[u8]) -> Option<&[u8]> {
        self.source.get(key)
    }

    /// Checks one write admission.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MaintenanceFrozen`] while the migration runs.
    pub fn admit_write(&self, shard_id: &ShardId, _now: Timestamp) -> Result<(), OpsError> {
        if &self.plan.shard_id != shard_id {
            return Ok(());
        }
        if self.is_frozen() {
            return Err(OpsError::MaintenanceFrozen {
                shard_id: shard_id.clone(),
            });
        }
        Ok(())
    }

    /// Runs the next migration step.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MigrationIncomplete`] when a checksum differs.
    pub fn step(&mut self) -> Result<Option<MigrationStep>, OpsError> {
        match self.phase {
            MigrationPhase::QuiesceWrites => {
                self.phase = MigrationPhase::DrainScheduler;
                Ok(Some(MigrationStep::QuiesceWrites))
            }
            MigrationPhase::DrainScheduler => {
                self.drain = Some(self.scheduler.drain()?);
                self.phase = MigrationPhase::RewriteEpoch;
                Ok(Some(MigrationStep::DrainScheduler))
            }
            MigrationPhase::RewriteEpoch => {
                self.target = Some(ShardStore {
                    shard_id: self.source.shard_id.clone(),
                    epoch: self.plan.target_epoch,
                    format: self.plan.to,
                    key_encoding: self.source.key_encoding,
                    rows: self.source.rows.clone(),
                });
                self.phase = MigrationPhase::VerifyChecksums;
                Ok(Some(MigrationStep::RewriteEpoch))
            }
            MigrationPhase::VerifyChecksums => {
                let Some(target) = &self.target else {
                    return Err(OpsError::MigrationIncomplete {
                        step: "verify_checksums",
                    });
                };
                if target.rows != self.source.rows
                    || target.rows_digest() != self.source.rows_digest()
                {
                    return Err(OpsError::MigrationIncomplete {
                        step: "verify_checksums",
                    });
                }
                self.phase = MigrationPhase::FlipPointer;
                Ok(Some(MigrationStep::VerifyChecksums))
            }
            MigrationPhase::FlipPointer => {
                if self.target.is_none() {
                    return Err(OpsError::MigrationIncomplete {
                        step: "flip_pointer",
                    });
                }
                self.pointer = self.plan.target_epoch;
                self.phase = MigrationPhase::Complete;
                Ok(Some(MigrationStep::FlipPointer))
            }
            MigrationPhase::Complete | MigrationPhase::RolledBack => Ok(None),
        }
    }

    /// Runs every step until the migration completes.
    ///
    /// # Errors
    ///
    /// Returns the first step error.
    pub fn run_to_completion(&mut self) -> Result<(), OpsError> {
        for _ in MigrationStep::ALL {
            let _ = self.step()?;
        }
        if self.phase != MigrationPhase::Complete {
            return Err(OpsError::MigrationIncomplete {
                step: "run_to_completion",
            });
        }
        Ok(())
    }

    /// Records one write under the new format.
    ///
    /// This call ends the rollback window.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MigrationIncomplete`] when the pointer is still at
    /// the old epoch.
    pub fn record_new_format_write(
        &mut self,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> Result<(), OpsError> {
        if self.pointer != self.plan.target_epoch || self.phase != MigrationPhase::Complete {
            return Err(OpsError::MigrationIncomplete {
                step: "new_format_write",
            });
        }
        self.first_new_format_write = true;
        if let Some(target) = &mut self.target {
            target.rows.insert(key, value);
        }
        Ok(())
    }

    /// Rolls the pointer back to the old epoch.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::DowngradeNotAllowed`] after the first new-format
    /// write.
    pub fn rollback(&mut self) -> Result<RollbackOutcome, OpsError> {
        if self.first_new_format_write {
            return Err(OpsError::DowngradeNotAllowed);
        }
        self.pointer = self.plan.source_epoch;
        self.phase = MigrationPhase::RolledBack;
        Ok(RollbackOutcome {
            epoch: self.source.epoch,
            format: self.source.format,
        })
    }

    /// Returns a durable checkpoint.
    #[must_use]
    pub fn checkpoint(&self) -> MigrationCheckpoint {
        MigrationCheckpoint {
            phase: self.phase,
            pointer: self.pointer,
            first_new_format_write: self.first_new_format_write,
            target: self.target.clone(),
            drain: self.drain.clone(),
            scheduler: self.scheduler.clone(),
        }
    }

    /// Resumes one migration from a durable checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MigrationIncomplete`] when the checkpoint does not
    /// belong to the source shard.
    pub fn resume(
        plan: ShardFormatPlan,
        source: ShardStore,
        checkpoint: MigrationCheckpoint,
    ) -> Result<Self, OpsError> {
        if source.shard_id != plan.shard_id
            || checkpoint
                .target
                .as_ref()
                .is_some_and(|target| target.shard_id != plan.shard_id)
        {
            return Err(OpsError::MigrationIncomplete {
                step: "migration_resume",
            });
        }
        Ok(Self {
            plan,
            source,
            target: checkpoint.target,
            phase: checkpoint.phase,
            pointer: checkpoint.pointer,
            first_new_format_write: checkpoint.first_new_format_write,
            scheduler: checkpoint.scheduler,
            drain: checkpoint.drain,
        })
    }
}

/// One key translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyTranslation {
    /// The old encoded key.
    pub old: Vec<u8>,
    /// The new encoded key.
    pub new: Vec<u8>,
}

impl KeyTranslation {
    /// Creates a key translation.
    #[must_use]
    pub fn new(old: Vec<u8>, new: Vec<u8>) -> Self {
        Self { old, new }
    }
}

/// The result of one key encoding migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEncodingReport {
    /// The rewritten rows.
    pub rows: BTreeMap<Vec<u8>, Vec<u8>>,
    /// The source key encoding version.
    pub from: KeyEncodingVersion,
    /// The target key encoding version.
    pub to: KeyEncodingVersion,
    /// The number of migrated keys.
    pub migrated: usize,
}

/// One key encoding migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEncodingMigration {
    /// The source key encoding version.
    pub from: KeyEncodingVersion,
    /// The target key encoding version.
    pub to: KeyEncodingVersion,
    translations: Vec<KeyTranslation>,
}

impl KeyEncodingMigration {
    /// Creates a key encoding migration.
    #[must_use]
    pub fn new(
        from: KeyEncodingVersion,
        to: KeyEncodingVersion,
        mut translations: Vec<KeyTranslation>,
    ) -> Self {
        translations.sort_by(|left, right| left.old.cmp(&right.old));
        Self {
            from,
            to,
            translations,
        }
    }

    /// Returns the new form of one old key.
    #[must_use]
    pub fn rewrite_key(&self, old: &[u8]) -> Option<&[u8]> {
        self.translations
            .iter()
            .find(|translation| translation.old == old)
            .map(|translation| translation.new.as_slice())
    }

    /// Migrates every row.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MigrationIncomplete`] when a key has no
    /// translation or when two keys collide.
    pub fn migrate(
        &self,
        rows: &BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Result<KeyEncodingReport, OpsError> {
        let mut migrated = BTreeMap::new();
        for (old, value) in rows {
            let Some(new) = self.rewrite_key(old) else {
                return Err(OpsError::MigrationIncomplete {
                    step: "key_encoding_translation",
                });
            };
            if migrated.insert(new.to_vec(), value.clone()).is_some() {
                return Err(OpsError::MigrationIncomplete {
                    step: "key_encoding_collision",
                });
            }
        }
        if migrated.len() != rows.len() {
            return Err(OpsError::MigrationIncomplete {
                step: "key_encoding_count",
            });
        }
        Ok(KeyEncodingReport {
            rows: migrated,
            from: self.from,
            to: self.to,
            migrated: rows.len(),
        })
    }
}

/// One migration plan of any scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationPlan {
    /// An artifact migration.
    Artifact(ArtifactMigrationPlan),
    /// A shard format migration.
    ShardFormat(ShardFormatPlan),
    /// A key encoding migration.
    KeyEncoding(KeyEncodingMigration),
}

impl MigrationPlan {
    /// Returns the scope of the plan.
    #[must_use]
    pub const fn scope(&self) -> MigrationScope {
        match self {
            Self::Artifact(_) => MigrationScope::Artifact,
            Self::ShardFormat(_) => MigrationScope::ShardFormat,
            Self::KeyEncoding(_) => MigrationScope::KeyEncoding,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::receipt::IdempotencyKey;
    use oxkv_contracts::version::CommitVersion;

    fn shard() -> ShardId {
        ShardId::new(vec![1])
    }

    fn source() -> ShardStore {
        let mut rows = BTreeMap::new();
        rows.insert(b"a".to_vec(), b"1".to_vec());
        rows.insert(b"b".to_vec(), b"2".to_vec());
        ShardStore::new(
            shard(),
            LeaseTerm::new(3),
            SnapshotPageVersion::new(1, 0),
            KeyEncodingVersion::new(1),
            rows,
        )
    }

    fn plan() -> ShardFormatPlan {
        ShardFormatPlan::new(
            shard(),
            SnapshotPageVersion::new(1, 0),
            SnapshotPageVersion::new(1, 1),
            LeaseTerm::new(3),
        )
        .expect("the plan is valid")
    }

    fn scheduler() -> SchedulerState {
        let mut scheduler = SchedulerState::new();
        scheduler.queue_commit(IdempotencyKey::new(vec![1]), CommitVersion::new(4));
        scheduler.queue_reject(IdempotencyKey::new(vec![2]));
        scheduler
    }

    #[test]
    fn every_step_runs_in_order() {
        let mut migration =
            ShardMigration::begin(plan(), source(), scheduler()).expect("the migration starts");
        let mut completed = Vec::new();
        while let Some(step) = migration.step().expect("the step succeeds") {
            completed.push(step.name());
        }
        assert_eq!(
            completed,
            vec![
                "quiesce_writes",
                "drain_scheduler",
                "rewrite_epoch",
                "verify_checksums",
                "flip_pointer",
            ]
        );
        assert_eq!(migration.phase(), MigrationPhase::Complete);
        assert_eq!(migration.pointer_epoch(), LeaseTerm::new(4));
        assert_eq!(migration.read(b"a"), Some(b"1".as_slice()));
        assert_eq!(migration.read_old_format(b"a"), Some(b"1".as_slice()));
    }

    #[test]
    fn a_checksum_mismatch_stops_the_migration() {
        let mut migration =
            ShardMigration::begin(plan(), source(), scheduler()).expect("the migration starts");
        for _ in 0..3 {
            let _ = migration.step().expect("the step succeeds");
        }
        migration
            .target
            .as_mut()
            .expect("the target exists")
            .rows
            .insert(b"a".to_vec(), b"wrong".to_vec());
        let error = migration.step().expect_err("the checksum differs");
        assert_eq!(error.code(), "ops.migration_incomplete");
        assert_eq!(migration.phase(), MigrationPhase::VerifyChecksums);
    }

    #[test]
    fn rollback_before_a_new_write_returns_the_old_format() {
        let mut migration =
            ShardMigration::begin(plan(), source(), scheduler()).expect("the migration starts");
        migration
            .run_to_completion()
            .expect("the migration completes");
        let outcome = migration.rollback().expect("rollback is allowed");
        assert_eq!(outcome.epoch, LeaseTerm::new(3));
        assert_eq!(outcome.format, SnapshotPageVersion::new(1, 0));
        assert_eq!(migration.phase(), MigrationPhase::RolledBack);
        assert_eq!(migration.read(b"a"), Some(b"1".as_slice()));
    }

    #[test]
    fn rollback_after_a_new_write_is_refused() {
        let mut migration =
            ShardMigration::begin(plan(), source(), scheduler()).expect("the migration starts");
        migration
            .run_to_completion()
            .expect("the migration completes");
        migration
            .record_new_format_write(b"c".to_vec(), b"3".to_vec())
            .expect("the write lands");
        let error = migration.rollback().expect_err("rollback is refused");
        assert_eq!(error.code(), "ops.downgrade_not_allowed");
    }

    #[test]
    fn a_crash_at_every_step_resumes_without_key_loss() {
        for crash_after in 0..=MigrationStep::ALL.len() {
            let mut migration =
                ShardMigration::begin(plan(), source(), scheduler()).expect("the migration starts");
            for _ in 0..crash_after {
                let _ = migration.step().expect("the step succeeds");
            }
            let checkpoint = migration.checkpoint();
            let mut resumed = ShardMigration::resume(plan(), source(), checkpoint)
                .expect("the migration resumes");
            resumed
                .run_to_completion()
                .expect("the migration completes");
            assert_eq!(resumed.read(b"a"), Some(b"1".as_slice()));
            assert_eq!(resumed.read(b"b"), Some(b"2".as_slice()));
            assert_eq!(resumed.phase(), MigrationPhase::Complete);
            let report = resumed.drain_report().expect("the scheduler drained");
            assert!(report.all_terminal());
        }
    }

    #[test]
    fn a_write_is_refused_during_maintenance() {
        let mut migration =
            ShardMigration::begin(plan(), source(), scheduler()).expect("the migration starts");
        let error = migration
            .admit_write(&shard(), Timestamp::new(1))
            .expect_err("the write is refused");
        assert_eq!(error.code(), "shard.frozen_for_maintenance");
        let _ = migration.step().expect("the step succeeds");
        migration
            .run_to_completion()
            .expect("the migration completes");
        assert!(migration.admit_write(&shard(), Timestamp::new(1)).is_ok());
    }

    #[test]
    fn a_key_encoding_migration_preserves_every_value() {
        let migration = KeyEncodingMigration::new(
            KeyEncodingVersion::new(1),
            KeyEncodingVersion::new(2),
            vec![
                KeyTranslation::new(b"a".to_vec(), b"A".to_vec()),
                KeyTranslation::new(b"b".to_vec(), b"B".to_vec()),
            ],
        );
        let report = migration.migrate(&source().rows).expect("the rows migrate");
        assert_eq!(report.migrated, 2);
        assert_eq!(report.rows.get(b"A".as_slice()), Some(&b"1".to_vec()));
        assert_eq!(report.rows.get(b"B".as_slice()), Some(&b"2".to_vec()));
    }

    #[test]
    fn a_key_encoding_migration_reports_a_missing_translation() {
        let migration = KeyEncodingMigration::new(
            KeyEncodingVersion::new(1),
            KeyEncodingVersion::new(2),
            vec![KeyTranslation::new(b"a".to_vec(), b"A".to_vec())],
        );
        let error = migration
            .migrate(&source().rows)
            .expect_err("a key has no translation");
        assert_eq!(error.code(), "ops.migration_incomplete");
    }

    #[test]
    fn a_key_encoding_migration_reports_a_collision() {
        let migration = KeyEncodingMigration::new(
            KeyEncodingVersion::new(1),
            KeyEncodingVersion::new(2),
            vec![
                KeyTranslation::new(b"a".to_vec(), b"A".to_vec()),
                KeyTranslation::new(b"b".to_vec(), b"A".to_vec()),
            ],
        );
        let error = migration
            .migrate(&source().rows)
            .expect_err("the keys collide");
        assert_eq!(error.code(), "ops.migration_incomplete");
    }

    #[test]
    fn a_plan_names_one_scope() {
        assert_eq!(
            MigrationPlan::ShardFormat(plan()).scope(),
            MigrationScope::ShardFormat
        );
    }
}
