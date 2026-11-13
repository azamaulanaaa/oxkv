//! Restore to a new epoch.
//!
//! A restore verifies every object before it serves a read. It writes a new
//! epoch and never mutates the source epoch. Terminal receipts and event
//! cursors are preserved or marked expired with a named error.

use std::collections::BTreeSet;

use oxkv_contracts::version::LeaseTerm;

use crate::backup::{BackupImage, BackupManifest};
use crate::error::OpsError;
use crate::receipts::AuthorityReceiptRecord;
use crate::snapshot::{AuditState, EffectRecord, EventOutboxEntry, SnapshotObject};

/// One restore source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSource {
    /// The source epoch.
    pub source_epoch: LeaseTerm,
    /// The backup manifest.
    pub manifest: BackupManifest,
    /// The backup image.
    pub image: BackupImage,
}

impl RestoreSource {
    /// Creates a restore source.
    #[must_use]
    pub fn new(source_epoch: LeaseTerm, manifest: BackupManifest, image: BackupImage) -> Self {
        Self {
            source_epoch,
            manifest,
            image,
        }
    }
}

/// One restore plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    /// The new epoch that the restore writes.
    pub new_epoch: LeaseTerm,
    /// The receipt retention window in milliseconds.
    pub receipt_retention_millis: u64,
    /// The age of the receipt set in milliseconds.
    pub receipt_age_millis: u64,
    /// The event retention window in milliseconds.
    pub event_retention_millis: u64,
}

impl RestorePlan {
    /// Creates a restore plan.
    #[must_use]
    pub const fn new(
        new_epoch: LeaseTerm,
        receipt_retention_millis: u64,
        receipt_age_millis: u64,
        event_retention_millis: u64,
    ) -> Self {
        Self {
            new_epoch,
            receipt_retention_millis,
            receipt_age_millis,
            event_retention_millis,
        }
    }
}

/// One expired state with its named error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredState {
    /// The key or receipt identity.
    pub identity: Vec<u8>,
    /// The named error code.
    pub reason: &'static str,
}

impl ExpiredState {
    /// Creates one expired state.
    #[must_use]
    pub const fn new(identity: Vec<u8>, reason: &'static str) -> Self {
        Self { identity, reason }
    }
}

/// The result of one restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredEpoch {
    /// The new epoch.
    pub epoch: LeaseTerm,
    /// The source epoch. The restore never changes it.
    pub source_epoch: LeaseTerm,
    /// The restored shard snapshots.
    pub snapshots: Vec<SnapshotObject>,
    /// The preserved terminal receipts.
    pub receipts: Vec<AuthorityReceiptRecord>,
    /// Receipts that the restore marked expired.
    pub expired_receipts: Vec<ExpiredState>,
    /// The preserved event cursors.
    pub event_cursors: Vec<EventOutboxEntry>,
    /// Event cursors that the restore marked expired.
    pub expired_event_cursors: Vec<ExpiredState>,
    /// The restored audit state.
    pub audit: AuditState,
    /// The restored effect records.
    pub effects: Vec<EffectRecord>,
    /// The total number of completed effect executions.
    pub effect_executions: u64,
    /// Effects that the restore executed again. This list is always empty.
    pub re_executed_effects: Vec<String>,
}

impl RestoredEpoch {
    /// Returns the snapshot for one shard.
    #[must_use]
    pub fn snapshot(&self, shard_id: &oxkv_contracts::version::ShardId) -> Option<&SnapshotObject> {
        self.snapshots
            .iter()
            .find(|snapshot| &snapshot.shard_id == shard_id)
    }

    /// Returns one row value.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.snapshots.iter().find_map(|snapshot| snapshot.get(key))
    }

    /// Returns the index postings for one key.
    #[must_use]
    pub fn postings(&self, key: &[u8]) -> Option<&[String]> {
        self.snapshots
            .iter()
            .find_map(|snapshot| snapshot.postings(key))
    }

    /// Returns one preserved receipt.
    #[must_use]
    pub fn receipt(
        &self,
        key: &oxkv_contracts::receipt::IdempotencyKey,
    ) -> Option<&AuthorityReceiptRecord> {
        self.receipts
            .iter()
            .find(|receipt| &receipt.idempotency_key == key)
    }

    /// Returns one preserved event cursor.
    #[must_use]
    pub fn event_cursor(&self, key: &[u8]) -> Option<&EventOutboxEntry> {
        self.event_cursors.iter().find(|entry| entry.key == key)
    }

    /// Returns the number of effects that the restore executed again.
    #[must_use]
    pub fn re_executed_count(&self) -> usize {
        self.re_executed_effects.len()
    }
}

fn verify_manifest(source: &RestoreSource) -> Result<(), OpsError> {
    let mut seen = BTreeSet::new();
    for object in &source.manifest.objects {
        let Some(snapshot) = source
            .image
            .snapshots
            .iter()
            .find(|snapshot| snapshot.object_key == object.object_key)
        else {
            return Err(OpsError::RestoreChecksumMismatch {
                object: object.object_key.clone(),
            });
        };
        let actual = snapshot.content_digest();
        if actual != object.digest {
            return Err(OpsError::RestoreChecksumMismatch {
                object: object.object_key.clone(),
            });
        }
        seen.insert(object.object_key.clone());
    }
    for snapshot in &source.image.snapshots {
        if !seen.contains(&snapshot.object_key) {
            return Err(OpsError::RestoreChecksumMismatch {
                object: snapshot.object_key.clone(),
            });
        }
    }
    Ok(())
}

/// Restores one backup image to a new epoch.
///
/// # Errors
///
/// Returns [`OpsError::RestoreChecksumMismatch`] when an object does not match
/// its manifest digest. Returns [`OpsError::MigrationIncomplete`] when the new
/// epoch equals the source epoch.
pub fn restore(source: &RestoreSource, plan: &RestorePlan) -> Result<RestoredEpoch, OpsError> {
    if plan.new_epoch == source.source_epoch {
        return Err(OpsError::MigrationIncomplete {
            step: "restore_epoch",
        });
    }
    verify_manifest(source)?;

    let mut receipts = Vec::new();
    let mut expired_receipts = Vec::new();
    for receipt in &source.image.receipts {
        let preserve =
            receipt.is_terminal() && plan.receipt_age_millis <= plan.receipt_retention_millis;
        if preserve {
            receipts.push(receipt.clone());
        } else {
            expired_receipts.push(ExpiredState::new(
                receipt.idempotency_key.as_bytes().to_vec(),
                "receipt.expired",
            ));
        }
    }

    let mut event_cursors = Vec::new();
    let mut expired_event_cursors = Vec::new();
    for entry in &source.image.event_outbox {
        if entry.age_millis <= plan.event_retention_millis {
            event_cursors.push(entry.clone());
        } else {
            expired_event_cursors.push(ExpiredState::new(entry.key.clone(), "event.retention_gap"));
        }
    }

    let effect_executions = source
        .image
        .effects
        .iter()
        .map(|effect| effect.execution_count)
        .sum();
    Ok(RestoredEpoch {
        epoch: plan.new_epoch,
        source_epoch: source.source_epoch,
        snapshots: source.image.snapshots.clone(),
        receipts,
        expired_receipts,
        event_cursors,
        expired_event_cursors,
        audit: source.image.audit,
        effects: source.image.effects.clone(),
        effect_executions,
        re_executed_effects: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use oxkv_contracts::cursor::EventSequence;
    use oxkv_contracts::receipt::{IdempotencyKey, ReceiptState};
    use oxkv_contracts::version::{
        CommitVersion, KeyEncodingVersion, ManifestDigest, ManifestRevision, MapDigest, MapVersion,
        PolicyVersion, ShardId, SnapshotPageVersion,
    };

    use crate::backup::{BackupPlan, BackupVersions, QuiesceState, capture};

    fn shard() -> ShardId {
        ShardId::new(vec![1])
    }

    fn snapshot() -> SnapshotObject {
        let mut rows = BTreeMap::new();
        rows.insert(b"a".to_vec(), b"1".to_vec());
        let mut index = BTreeMap::new();
        index.insert(b"a".to_vec(), vec!["one".to_owned()]);
        SnapshotObject::new(
            "snapshot-1",
            shard(),
            CommitVersion::new(18),
            SnapshotPageVersion::new(1, 0),
            KeyEncodingVersion::new(1),
            rows,
            index,
        )
    }

    fn source() -> RestoreSource {
        let mut image = BackupImage::new();
        image.add_snapshot(snapshot());
        image.receipts.push(AuthorityReceiptRecord::new(
            IdempotencyKey::new(vec![1]),
            ReceiptState::Committed,
            Some(CommitVersion::new(18)),
            None,
        ));
        image.event_outbox.push(EventOutboxEntry::new(
            b"a".to_vec(),
            EventSequence::new(4),
            0,
        ));
        image.effects.push(EffectRecord::new("email", true, 1));
        let plan = BackupPlan::new(
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
        );
        let outcome =
            capture(&plan, QuiesceState::GlobalQuiesced, &image).expect("the backup captures");
        RestoreSource::new(LeaseTerm::new(1), outcome.manifest, image)
    }

    fn plan() -> RestorePlan {
        RestorePlan::new(LeaseTerm::new(2), 1_000, 10, 1_000)
    }

    #[test]
    fn a_restore_reproduces_keys_values_indexes_receipts_and_cursors() {
        let source = source();
        let restored = restore(&source, &plan()).expect("the restore succeeds");
        assert_eq!(restored.epoch, LeaseTerm::new(2));
        assert_eq!(restored.source_epoch, LeaseTerm::new(1));
        assert_eq!(restored.get(b"a"), Some(b"1".as_slice()));
        assert_eq!(restored.postings(b"a"), Some(["one".to_owned()].as_slice()));
        assert!(restored.receipt(&IdempotencyKey::new(vec![1])).is_some());
        assert!(restored.event_cursor(b"a").is_some());
        assert_eq!(restored.audit, source.image.audit);
    }

    #[test]
    fn a_checksum_mismatch_leaves_the_source_unchanged() {
        let mut source = source();
        let before = source.clone();
        source.image.snapshots[0]
            .rows
            .insert(b"a".to_vec(), b"tampered".to_vec());
        let error = restore(&source, &plan()).expect_err("the checksum differs");
        assert_eq!(error.code(), "ops.restore_checksum_mismatch");
        assert_eq!(source.source_epoch, before.source_epoch);
        assert_eq!(
            before.image.snapshots[0].rows.get(b"a".as_slice()),
            Some(&b"1".to_vec())
        );
    }

    #[test]
    fn a_restore_does_not_execute_an_effect_again() {
        let source = source();
        let restored = restore(&source, &plan()).expect("the restore succeeds");
        assert_eq!(restored.effects.len(), 1);
        assert_eq!(restored.effect_executions, 1);
        assert_eq!(restored.re_executed_count(), 0);
        assert_eq!(restored.effects[0].execution_count, 1);
    }

    #[test]
    fn an_expired_receipt_and_cursor_name_their_errors() {
        let mut source = source();
        source.image.event_outbox[0].age_millis = 10_000;
        let restored = restore(&source, &RestorePlan::new(LeaseTerm::new(2), 100, 200, 100))
            .expect("the restore succeeds");
        assert_eq!(restored.receipts.len(), 0);
        assert_eq!(restored.expired_receipts[0].reason, "receipt.expired");
        assert_eq!(restored.event_cursors.len(), 0);
        assert_eq!(
            restored.expired_event_cursors[0].reason,
            "event.retention_gap"
        );
    }

    #[test]
    fn a_restore_needs_a_new_epoch() {
        let source = source();
        let error = restore(
            &source,
            &RestorePlan::new(LeaseTerm::new(1), 1_000, 0, 1_000),
        )
        .expect_err("the epoch repeats");
        assert_eq!(error.code(), "ops.migration_incomplete");
    }
}
