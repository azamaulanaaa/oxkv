//! End-to-end tests for the operations contracts.

use std::collections::BTreeMap;

use oxkv_contracts::cursor::EventSequence;
use oxkv_contracts::receipt::{AuditSequence, IdempotencyKey, ReceiptState};
use oxkv_contracts::version::{
    CommitVersion, Hash32, KeyEncodingVersion, LeaseTerm, ManifestDigest, ManifestRevision,
    MapDigest, MapVersion, PolicyVersion, ShardId, SnapshotPageVersion,
};
use oxkv_ops::artifact::{ArtifactMigrationPlan, ArtifactObject, LayoutChange, MigratableArtifact};
use oxkv_ops::backup::{BackupImage, BackupPlan, BackupVersions, PinLedger, QuiesceState, capture};
use oxkv_ops::compaction::{CompactionBudget, CompactionPass, CompactionPolicy, CompactionState};
use oxkv_ops::control::{ControlOperation, ControlResult, ControlVersion, audit_control};
use oxkv_ops::freeze::SupportPolicy;
use oxkv_ops::maintenance::{FreezeState, MaintenanceWindow, Timestamp, WriteGate};
use oxkv_ops::migration::{
    KeyEncodingMigration, KeyTranslation, MigrationPhase, MigrationStep, ShardFormatPlan,
    ShardMigration, ShardStore,
};
use oxkv_ops::quota::{
    OperationClass, QuotaLimits, QuotaPolicy, QuotaRequest, QuotaReserve, QuotaUsage, RateLimitKey,
    RateLimiter, TokenBucket,
};
use oxkv_ops::receipts::{
    AuthorityReceiptRecord, ClientOutboxEntry, RebaseAction, SchedulerState, rebase_path,
};
use oxkv_ops::restore::{RestorePlan, RestoreSource, restore};
use oxkv_ops::snapshot::{
    AuditState, EffectRecord, EventOutboxEntry, IdempotencyRecord, SnapshotObject,
};
use oxkv_ops::telemetry::{MetricName, MetricSample, TelemetryBatch, Trace, TraceKind, TraceSpan};

fn shard(value: u64) -> ShardId {
    ShardId::new(value.to_be_bytes().to_vec())
}

fn snapshot(shard_value: u64, version: u64) -> SnapshotObject {
    let mut rows = BTreeMap::new();
    rows.insert(b"account/42".to_vec(), b"value".to_vec());
    let mut index = BTreeMap::new();
    index.insert(b"account/42".to_vec(), vec!["token".to_owned()]);
    SnapshotObject::new(
        format!("snapshot-{shard_value}"),
        shard(shard_value),
        CommitVersion::new(version),
        SnapshotPageVersion::new(1, 0),
        KeyEncodingVersion::new(1),
        rows,
        index,
    )
}

fn plan() -> ShardFormatPlan {
    ShardFormatPlan::new(
        shard(1),
        SnapshotPageVersion::new(1, 0),
        SnapshotPageVersion::new(1, 1),
        LeaseTerm::new(1),
    )
    .expect("the plan is valid")
}

fn source() -> ShardStore {
    let mut rows = BTreeMap::new();
    rows.insert(b"a".to_vec(), b"1".to_vec());
    rows.insert(b"b".to_vec(), b"2".to_vec());
    ShardStore::new(
        shard(1),
        LeaseTerm::new(1),
        SnapshotPageVersion::new(1, 0),
        KeyEncodingVersion::new(1),
        rows,
    )
}

fn backup_plan() -> BackupPlan {
    BackupPlan::new(
        "pin-1",
        BackupVersions::new(
            ManifestRevision::new(1),
            SnapshotPageVersion::new(1, 0),
            KeyEncodingVersion::new(1),
            ManifestDigest::from_bytes([1; 32]),
            PolicyVersion::from_bytes([2; 32]),
            MapVersion::new(1),
            MapDigest::from_bytes([3; 32]),
        ),
    )
}

fn backup_image() -> BackupImage {
    let mut image = BackupImage::new();
    image.add_snapshot(snapshot(1, 18));
    image.add_snapshot(snapshot(2, 9));
    image.receipts.push(AuthorityReceiptRecord::new(
        IdempotencyKey::new(vec![1]),
        ReceiptState::Committed,
        Some(CommitVersion::new(18)),
        None,
    ));
    image.receipts.push(AuthorityReceiptRecord::new(
        IdempotencyKey::new(vec![2]),
        ReceiptState::Conflict,
        None,
        Some("write.version_conflict"),
    ));
    image.idempotency.push(IdempotencyRecord::new(
        IdempotencyKey::new(vec![1]),
        Hash32::from_bytes([4; 32]),
    ));
    image.event_outbox.push(EventOutboxEntry::new(
        b"account/42".to_vec(),
        EventSequence::new(7),
        10,
    ));
    image.audit = AuditState::new(Hash32::from_bytes([5; 32]), AuditSequence::new(9));
    image.effects.push(EffectRecord::new("send-email", true, 1));
    image
}

#[test]
fn a_crash_at_every_migration_step_resumes_without_key_loss() {
    for crash_after in 0..=MigrationStep::ALL.len() {
        let mut migration = ShardMigration::begin(plan(), source(), SchedulerState::new())
            .expect("the migration starts");
        for _ in 0..crash_after {
            let _ = migration.step().expect("the step succeeds");
        }
        let checkpoint = migration.checkpoint();
        let mut resumed =
            ShardMigration::resume(plan(), source(), checkpoint).expect("the migration resumes");
        resumed
            .run_to_completion()
            .expect("the migration completes");
        assert_eq!(resumed.read(b"a"), Some(b"1".as_slice()));
        assert_eq!(resumed.read(b"b"), Some(b"2".as_slice()));
        assert_eq!(resumed.phase(), MigrationPhase::Complete);
        assert_eq!(resumed.pointer_epoch(), LeaseTerm::new(2));
    }
}

#[test]
fn a_rollback_before_the_first_new_write_returns_the_old_format() {
    let mut migration = ShardMigration::begin(plan(), source(), SchedulerState::new())
        .expect("the migration starts");
    migration
        .run_to_completion()
        .expect("the migration completes");
    let outcome = migration.rollback().expect("the rollback is allowed");
    assert_eq!(outcome.format, SnapshotPageVersion::new(1, 0));
    assert_eq!(outcome.epoch, LeaseTerm::new(1));
    assert_eq!(migration.read(b"a"), Some(b"1".as_slice()));
}

#[test]
fn a_rollback_after_the_first_new_write_is_refused() {
    let mut migration = ShardMigration::begin(plan(), source(), SchedulerState::new())
        .expect("the migration starts");
    migration
        .run_to_completion()
        .expect("the migration completes");
    migration
        .record_new_format_write(b"c".to_vec(), b"3".to_vec())
        .expect("the write lands");
    let error = migration.rollback().expect_err("the rollback is refused");
    assert_eq!(error.code(), "ops.downgrade_not_allowed");
}

#[test]
fn a_backup_under_write_load_records_a_version_vector() {
    let image = backup_image();
    let outcome =
        capture(&backup_plan(), QuiesceState::Writable, &image).expect("the backup captures");
    assert_eq!(
        outcome.manifest.version(&shard(1)),
        Some(CommitVersion::new(18))
    );
    assert_eq!(
        outcome.manifest.version(&shard(2)),
        Some(CommitVersion::new(9))
    );
    assert!(!outcome.consistency.is_global());
    assert_eq!(outcome.manifest.objects.len(), 2);
    assert_eq!(outcome.manifest.receipt_count, 2);
    assert_eq!(outcome.manifest.idempotency_count, 1);
    assert_eq!(outcome.manifest.event_outbox_count, 1);
}

#[test]
fn a_restore_reproduces_every_key_value_index_receipt_and_cursor() {
    let image = backup_image();
    let outcome =
        capture(&backup_plan(), QuiesceState::GlobalQuiesced, &image).expect("the backup captures");
    let source = RestoreSource::new(LeaseTerm::new(1), outcome.manifest, image);
    let restored = restore(
        &source,
        &RestorePlan::new(LeaseTerm::new(2), 1_000, 100, 1_000),
    )
    .expect("the restore succeeds");
    assert_eq!(restored.epoch, LeaseTerm::new(2));
    assert_eq!(restored.source_epoch, LeaseTerm::new(1));
    assert_eq!(restored.get(b"account/42"), Some(b"value".as_slice()));
    assert_eq!(
        restored.postings(b"account/42"),
        Some(["token".to_owned()].as_slice())
    );
    assert!(restored.receipt(&IdempotencyKey::new(vec![1])).is_some());
    assert!(restored.receipt(&IdempotencyKey::new(vec![2])).is_some());
    assert!(restored.event_cursor(b"account/42").is_some());
    assert_eq!(restored.audit.sequence, AuditSequence::new(9));
}

#[test]
fn a_lost_pin_returns_the_named_error() {
    let outcome = capture(&backup_plan(), QuiesceState::Writable, &backup_image())
        .expect("the backup captures");
    let mut ledger = PinLedger::new();
    ledger.hold(&outcome.pin);
    ledger.lose("pin-1");
    let error = outcome.complete(&mut ledger).expect_err("the pin is lost");
    assert_eq!(error.code(), "ops.backup_pin_lost");
}

#[test]
fn a_checksum_mismatch_leaves_the_source_epoch_unchanged() {
    let image = backup_image();
    let outcome =
        capture(&backup_plan(), QuiesceState::Writable, &image).expect("the backup captures");
    let mut source = RestoreSource::new(LeaseTerm::new(1), outcome.manifest, image);
    source.image.snapshots[0]
        .rows
        .insert(b"account/42".to_vec(), b"tampered".to_vec());
    let error = restore(
        &source,
        &RestorePlan::new(LeaseTerm::new(2), 1_000, 0, 1_000),
    )
    .expect_err("the checksum differs");
    assert_eq!(error.code(), "ops.restore_checksum_mismatch");
    assert_eq!(source.source_epoch, LeaseTerm::new(1));
}

#[test]
fn a_restore_does_not_re_execute_an_effect() {
    let image = backup_image();
    let outcome =
        capture(&backup_plan(), QuiesceState::Writable, &image).expect("the backup captures");
    let source = RestoreSource::new(LeaseTerm::new(1), outcome.manifest, image);
    let restored = restore(
        &source,
        &RestorePlan::new(LeaseTerm::new(2), 1_000, 0, 1_000),
    )
    .expect("the restore succeeds");
    assert_eq!(restored.effect_executions, 1);
    assert_eq!(restored.re_executed_count(), 0);
    assert_eq!(source.image.effects[0].execution_count, 1);
}

#[test]
fn an_old_outbox_entry_receives_stale_and_a_rebase_path() {
    let entry = ClientOutboxEntry::new(IdempotencyKey::new(vec![9]), LeaseTerm::new(1));
    let rebase = rebase_path(
        ManifestDigest::from_bytes([1; 32]),
        shard(1),
        CommitVersion::new(18),
        RebaseAction::RebaseOnLatest,
    );
    let error = entry
        .reconnect(LeaseTerm::new(2), rebase.clone())
        .expect_err("the entry is stale");
    assert_eq!(error.code(), "ops.client_outbox_stale");
    assert_eq!(error, oxkv_ops::OpsError::ClientOutboxStale { rebase });
}

#[test]
fn a_migration_does_not_end_a_client_local_entry() {
    let mut scheduler = SchedulerState::new();
    scheduler.add_client_local(ClientOutboxEntry::new(
        IdempotencyKey::new(vec![9]),
        LeaseTerm::new(1),
    ));
    scheduler.queue_commit(IdempotencyKey::new(vec![1]), CommitVersion::new(7));
    let mut migration =
        ShardMigration::begin(plan(), source(), scheduler).expect("the migration starts");
    migration
        .run_to_completion()
        .expect("the migration completes");
    let report = migration.drain_report().expect("the scheduler drained");
    assert!(report.all_terminal());
    assert_eq!(report.untouched_client.len(), 1);
    assert!(!report.knows(&IdempotencyKey::new(vec![9])));
}

#[test]
fn a_quota_breach_rejects_at_admission() {
    let policy = QuotaPolicy::new(
        QuotaLimits::new(100, 1_000, 1_000, 1_000, 1_000, 1_000),
        QuotaReserve::new(10, 0),
    );
    let error = policy
        .check(&QuotaUsage::new(90, 0, 0, 0, 0, 0), &QuotaRequest::write(1))
        .expect_err("the reserve is protected");
    assert_eq!(error.code(), "ops.quota_exceeded");
    assert_eq!(error.receipt_state(), Some(ReceiptState::Rejected));
}

#[test]
fn a_rate_limit_rejects_with_a_named_error() {
    let key = RateLimitKey::shard(shard(1), OperationClass::Write);
    let mut limiter = RateLimiter::new();
    limiter.configure(key.clone(), TokenBucket::new(1, 0, Timestamp::new(0)));
    assert!(limiter.admit(&key, Timestamp::new(0), 1).is_ok());
    let error = limiter
        .admit(&key, Timestamp::new(0), 1)
        .expect_err("the bucket is empty");
    assert_eq!(error.code(), "ops.rate_limited");
    assert_eq!(error.receipt_state(), None);
}

#[test]
fn a_write_during_maintenance_is_refused_and_a_read_continues() {
    let mut freeze = FreezeState::new();
    let mut gate = WriteGate::new();
    gate.freeze_mut().freeze_shard(shard(1));
    gate.publish_window(MaintenanceWindow::new(
        shard(2),
        Timestamp::new(10),
        Timestamp::new(20),
    ))
    .expect("the window is valid");
    let error = gate
        .admit_write(&shard(1), Timestamp::new(0))
        .expect_err("the shard is frozen");
    assert_eq!(error.code(), "shard.frozen_for_maintenance");
    FreezeState::admit_read();
    assert!(gate.admit_write(&shard(2), Timestamp::new(15)).is_err());
    assert!(gate.admit_write(&shard(2), Timestamp::new(20)).is_ok());
    freeze.freeze_all();
    assert!(freeze.admit_write(&shard(3)).is_err());
}

#[test]
fn a_migration_incomplete_error_is_named() {
    let error = ShardFormatPlan::new(
        shard(1),
        SnapshotPageVersion::new(1, 0),
        SnapshotPageVersion::new(1, 2),
        LeaseTerm::new(1),
    )
    .expect_err("the format step is invalid");
    assert_eq!(error.code(), "ops.migration_incomplete");
}

#[test]
fn an_artifact_migration_lists_layout_changes_and_a_new_digest() {
    let plan = ArtifactMigrationPlan::new(
        SnapshotPageVersion::new(1, 0),
        SnapshotPageVersion::new(1, 1),
        vec![
            LayoutChange::new("page_sequence", "absent", "u64"),
            LayoutChange::new("provenance", "v1", "v1 plus key encoding"),
        ],
    )
    .expect("the plan is valid");
    let object = ArtifactObject::new(
        "snapshot-1",
        MigratableArtifact::Snapshot,
        SnapshotPageVersion::new(1, 0),
        KeyEncodingVersion::new(1),
        b"snapshot bytes".to_vec(),
    );
    let migrated = plan.migrate(&object).expect("the artifact migrates");
    assert_eq!(migrated.changes.len(), 2);
    assert_ne!(migrated.old_digest, migrated.new_digest);
    assert_eq!(migrated.object.format, SnapshotPageVersion::new(1, 1));
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
}

#[test]
fn a_control_action_is_audited_and_is_not_a_data_write() {
    let record = audit_control(
        ControlOperation::Migrate,
        "operator",
        Some(shard(1)),
        ControlVersion::new(ManifestDigest::from_bytes([1; 32]), MapVersion::new(2)),
        ControlResult::Accepted,
    );
    assert_eq!(record.operation.method(), "control/migrate");
    assert!(!ControlOperation::is_data_write());
    assert_eq!(record.actor, "operator");
}

#[test]
fn the_support_policy_accepts_one_previous_format() {
    let policy = SupportPolicy::new(1, 2);
    assert!(policy.can_read(SnapshotPageVersion::new(1, 2)));
    assert!(policy.can_read(SnapshotPageVersion::new(1, 1)));
    assert!(!policy.can_read(SnapshotPageVersion::new(1, 0)));
    assert!(SupportPolicy::check_downgrade(true).is_err());
}

#[test]
fn metrics_and_traces_are_pure_records() {
    let mut batch = TelemetryBatch::new();
    batch.push_metric(MetricSample::gauge(MetricName::ManifestVersion, 3));
    batch.push_metric(MetricSample::ratio(MetricName::ConflictCount, 1, 10));
    for kind in TraceKind::ALL {
        batch.push_trace(Trace::new(
            Hash32::from_bytes([1; 32]),
            kind,
            vec![TraceSpan::new("span").with_attribute("shard", "1")],
        ));
    }
    assert_eq!(
        batch.sample(MetricName::ManifestVersion),
        Some(&MetricSample::gauge(MetricName::ManifestVersion, 3))
    );
    assert_eq!(batch.traces.len(), 3);
}

#[test]
fn a_paused_compaction_holds_and_a_pin_blocks() {
    let mut policy = CompactionPolicy::new(CompactionBudget::new(1_000, 60), 0);
    let mut state = CompactionState::new(4, 4_000, 100, Timestamp::new(0));
    policy.pause(Timestamp::new(0), 1_000);
    assert_eq!(
        policy
            .run_pass(Timestamp::new(500), &PinLedger::new(), &mut state)
            .expect("the pass runs"),
        CompactionPass::Paused
    );
    assert_eq!(state.input_files, 4);
    policy.resume();
    let mut ledger = PinLedger::new();
    ledger.hold(&oxkv_ops::backup::BackupPin::new("pin-1", vec![shard(1)]));
    assert_eq!(
        policy
            .run_pass(Timestamp::new(1_000), &ledger, &mut state)
            .expect("the pass runs"),
        CompactionPass::BlockedByPin
    );
}
