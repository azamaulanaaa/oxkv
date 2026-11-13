//! Backup as a set of shard snapshots with a version vector.
//!
//! A backup records one consistency point per shard. A global consistency
//! point requires a global write quiesce. A backup holds a retention pin that
//! blocks compaction until the backup completes or fails.

use std::collections::BTreeMap;

use oxkv_contracts::version::{
    CommitVersion, ContentDigest, KeyEncodingVersion, ManifestDigest, ManifestRevision, MapDigest,
    MapVersion, PolicyVersion, ScopeVersionVector, ShardId, SnapshotPageVersion, VersionVector,
};

use crate::error::OpsError;
use crate::receipts::{AuthorityReceiptRecord, SchedulerState};
use crate::snapshot::{
    AuditState, EffectRecord, EventOutboxEntry, IdempotencyRecord, SnapshotObject,
};

/// The write quiesce state at capture time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuiesceState {
    /// Writes may run.
    Writable,
    /// Every shard blocks writes.
    GlobalQuiesced,
}

/// The consistency point that a backup achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsistencyPoint {
    /// One commit version per shard. This is the only point without a global
    /// write quiesce.
    PerShard {
        /// The captured version vector.
        vector: ScopeVersionVector,
    },
    /// One global point under a global write quiesce.
    Global {
        /// The captured version vector.
        vector: ScopeVersionVector,
    },
}

impl ConsistencyPoint {
    /// Returns the captured version vector.
    #[must_use]
    pub const fn vector(&self) -> &ScopeVersionVector {
        match self {
            Self::PerShard { vector } | Self::Global { vector } => vector,
        }
    }

    /// Returns true when the point is global.
    #[must_use]
    pub const fn is_global(&self) -> bool {
        matches!(self, Self::Global { .. })
    }
}

/// One immutable object in a backup manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupObjectRef {
    /// The stable object key.
    pub object_key: String,
    /// The shard that owns the object.
    pub shard_id: ShardId,
    /// The commit version of the object.
    pub commit_version: CommitVersion,
    /// The snapshot and page format version.
    pub format: SnapshotPageVersion,
    /// The key encoding annex version.
    pub key_encoding: KeyEncodingVersion,
    /// The checksum of the object.
    pub digest: ContentDigest,
}

impl BackupObjectRef {
    /// Creates one object reference.
    #[must_use]
    pub fn new(snapshot: &SnapshotObject) -> Self {
        Self {
            object_key: snapshot.object_key.clone(),
            shard_id: snapshot.shard_id.clone(),
            commit_version: snapshot.commit_version,
            format: snapshot.format,
            key_encoding: snapshot.key_encoding,
            digest: snapshot.content_digest(),
        }
    }
}

/// The state that a backup carries with its snapshot set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupImage {
    /// The immutable shard snapshots.
    pub snapshots: Vec<SnapshotObject>,
    /// The authority receipts.
    pub receipts: Vec<AuthorityReceiptRecord>,
    /// The idempotency records.
    pub idempotency: Vec<IdempotencyRecord>,
    /// The event outbox state.
    pub event_outbox: Vec<EventOutboxEntry>,
    /// The audit chain state.
    pub audit: AuditState,
    /// The effect records.
    pub effects: Vec<EffectRecord>,
}

impl BackupImage {
    /// Creates an empty backup image.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one snapshot.
    pub fn add_snapshot(&mut self, snapshot: SnapshotObject) {
        self.snapshots.push(snapshot);
    }

    /// Returns the snapshot for one shard.
    #[must_use]
    pub fn snapshot(&self, shard_id: &ShardId) -> Option<&SnapshotObject> {
        self.snapshots
            .iter()
            .find(|snapshot| &snapshot.shard_id == shard_id)
    }

    /// Captures one image from a scheduler state and the remaining backup state.
    #[must_use]
    pub fn from_scheduler(
        snapshots: Vec<SnapshotObject>,
        scheduler: &SchedulerState,
        state: BackupState,
    ) -> Self {
        Self {
            snapshots,
            receipts: scheduler.terminal.clone(),
            idempotency: state.idempotency,
            event_outbox: state.event_outbox,
            audit: state.audit,
            effects: state.effects,
        }
    }
}

/// The state that a scheduler state does not hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupState {
    /// The idempotency records.
    pub idempotency: Vec<IdempotencyRecord>,
    /// The event outbox state.
    pub event_outbox: Vec<EventOutboxEntry>,
    /// The audit chain state.
    pub audit: AuditState,
    /// The effect records.
    pub effects: Vec<EffectRecord>,
}

/// The manifest of one backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupManifest {
    /// The manifest version of the backup.
    pub manifest_version: ManifestRevision,
    /// The snapshot and page format version.
    pub format: SnapshotPageVersion,
    /// The key encoding annex version.
    pub key_encoding: KeyEncodingVersion,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The policy version.
    pub policy_version: PolicyVersion,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The shard map digest.
    pub map_digest: MapDigest,
    /// The captured version vector.
    pub version_vector: ScopeVersionVector,
    /// Every immutable object.
    pub objects: Vec<BackupObjectRef>,
    /// The number of receipt records.
    pub receipt_count: usize,
    /// The number of idempotency records.
    pub idempotency_count: usize,
    /// The number of event outbox entries.
    pub event_outbox_count: usize,
    /// The audit chain state.
    pub audit: AuditState,
}

impl BackupManifest {
    /// Returns the object reference for one object key.
    #[must_use]
    pub fn object(&self, object_key: &str) -> Option<&BackupObjectRef> {
        self.objects
            .iter()
            .find(|object| object.object_key == object_key)
    }

    /// Returns the commit version for one shard.
    #[must_use]
    pub fn version(&self, shard_id: &ShardId) -> Option<CommitVersion> {
        self.version_vector.get(shard_id)
    }
}

/// The version set that one backup records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupVersions {
    /// The manifest version of the backup.
    pub manifest_version: ManifestRevision,
    /// The snapshot and page format version.
    pub format: SnapshotPageVersion,
    /// The key encoding annex version.
    pub key_encoding: KeyEncodingVersion,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The policy version.
    pub policy_version: PolicyVersion,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The shard map digest.
    pub map_digest: MapDigest,
}

impl BackupVersions {
    /// Creates a backup version set.
    #[must_use]
    pub const fn new(
        manifest_version: ManifestRevision,
        format: SnapshotPageVersion,
        key_encoding: KeyEncodingVersion,
        manifest_digest: ManifestDigest,
        policy_version: PolicyVersion,
        map_version: MapVersion,
        map_digest: MapDigest,
    ) -> Self {
        Self {
            manifest_version,
            format,
            key_encoding,
            manifest_digest,
            policy_version,
            map_version,
            map_digest,
        }
    }
}

/// One backup plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPlan {
    /// The retention pin identity.
    pub pin_id: String,
    /// The version set of the backup.
    pub versions: BackupVersions,
    /// True when the operator requests a global consistency point.
    pub wants_global: bool,
}

impl BackupPlan {
    /// Creates a backup plan.
    #[must_use]
    pub fn new(pin_id: impl Into<String>, versions: BackupVersions) -> Self {
        Self {
            pin_id: pin_id.into(),
            versions,
            wants_global: false,
        }
    }

    /// Requests a global consistency point.
    #[must_use]
    pub const fn with_global_point(mut self) -> Self {
        self.wants_global = true;
        self
    }
}

/// The state of one retention pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PinState {
    /// The pin blocks compaction.
    Held,
    /// The backup completed or failed.
    Released,
    /// The pin is gone before the backup ended.
    Lost,
}

/// One backup retention pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPin {
    /// The pin identity.
    pub id: String,
    /// The shards that the pin covers.
    pub shard_ids: Vec<ShardId>,
    /// The pin state.
    pub state: PinState,
}

impl BackupPin {
    /// Creates a held pin.
    #[must_use]
    pub fn new(id: impl Into<String>, shard_ids: Vec<ShardId>) -> Self {
        Self {
            id: id.into(),
            shard_ids,
            state: PinState::Held,
        }
    }

    /// Returns true when the pin blocks compaction.
    #[must_use]
    pub const fn is_held(&self) -> bool {
        matches!(self.state, PinState::Held)
    }
}

/// The set of live pins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinLedger {
    states: BTreeMap<String, PinState>,
}

impl PinLedger {
    /// Creates an empty pin ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Holds one pin.
    pub fn hold(&mut self, pin: &BackupPin) {
        self.states.insert(pin.id.clone(), PinState::Held);
    }

    /// Releases one pin.
    pub fn release(&mut self, pin_id: &str) {
        self.states.insert(pin_id.to_owned(), PinState::Released);
    }

    /// Marks one pin as lost.
    pub fn lose(&mut self, pin_id: &str) {
        self.states.insert(pin_id.to_owned(), PinState::Lost);
    }

    /// Returns the state of one pin.
    #[must_use]
    pub fn state(&self, pin_id: &str) -> Option<PinState> {
        self.states.get(pin_id).copied()
    }

    /// Returns true when one pin is held.
    #[must_use]
    pub fn any_held(&self) -> bool {
        self.states.values().any(|state| *state == PinState::Held)
    }

    /// Returns the identity of the first lost pin.
    #[must_use]
    pub fn lost_pin(&self) -> Option<&str> {
        self.states
            .iter()
            .find(|(_, state)| **state == PinState::Lost)
            .map(|(pin, _)| pin.as_str())
    }

    /// Checks that one pin is still held.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::BackupPinLost`] when the pin is absent or lost.
    pub fn require_held(&self, pin_id: &str) -> Result<(), OpsError> {
        if self.state(pin_id) == Some(PinState::Held) {
            return Ok(());
        }
        Err(OpsError::BackupPinLost {
            pin: pin_id.to_owned(),
        })
    }
}

/// The result of one backup capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupOutcome {
    /// The backup manifest.
    pub manifest: BackupManifest,
    /// The retention pin.
    pub pin: BackupPin,
    /// The achieved consistency point.
    pub consistency: ConsistencyPoint,
}

impl BackupOutcome {
    /// Completes the backup and releases the pin.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::BackupPinLost`] when the pin is absent or lost.
    pub fn complete(self, ledger: &mut PinLedger) -> Result<BackupManifest, OpsError> {
        ledger.require_held(&self.pin.id)?;
        ledger.release(&self.pin.id);
        Ok(self.manifest)
    }

    /// Fails the backup and releases the pin.
    pub fn fail(self, ledger: &mut PinLedger) {
        ledger.release(&self.pin.id);
    }
}

/// Captures one backup image.
///
/// A global point is reported only when the caller requests it and passes a
/// global quiesce state. Without a global quiesce the result is a per-shard
/// version vector.
///
/// # Errors
///
/// Returns [`OpsError::UnsupportedFormat`] for an unknown snapshot major.
/// Returns [`OpsError::MigrationIncomplete`] for a duplicate shard, a format
/// mismatch, or an invalid snapshot digest.
pub fn capture(
    plan: &BackupPlan,
    quiesce: QuiesceState,
    image: &BackupImage,
) -> Result<BackupOutcome, OpsError> {
    let mut entries = Vec::with_capacity(image.snapshots.len());
    let mut objects = Vec::with_capacity(image.snapshots.len());
    for snapshot in &image.snapshots {
        if snapshot.format.major() != plan.versions.format.major() {
            return Err(OpsError::UnsupportedFormat {
                version: snapshot.format,
            });
        }
        if snapshot.format != plan.versions.format
            || snapshot.key_encoding != plan.versions.key_encoding
        {
            return Err(OpsError::MigrationIncomplete {
                step: "backup_format",
            });
        }
        for (existing, _) in &entries {
            if existing == &snapshot.shard_id {
                return Err(OpsError::MigrationIncomplete {
                    step: "backup_duplicate_shard",
                });
            }
        }
        entries.push((snapshot.shard_id.clone(), snapshot.commit_version));
        objects.push(BackupObjectRef::new(snapshot));
    }
    let vector = ScopeVersionVector::from_vector(VersionVector::from_entries(entries));
    let consistency = if plan.wants_global && quiesce == QuiesceState::GlobalQuiesced {
        ConsistencyPoint::Global {
            vector: vector.clone(),
        }
    } else {
        ConsistencyPoint::PerShard {
            vector: vector.clone(),
        }
    };
    let manifest = BackupManifest {
        manifest_version: plan.versions.manifest_version,
        format: plan.versions.format,
        key_encoding: plan.versions.key_encoding,
        manifest_digest: plan.versions.manifest_digest,
        policy_version: plan.versions.policy_version,
        map_version: plan.versions.map_version,
        map_digest: plan.versions.map_digest,
        version_vector: vector,
        objects,
        receipt_count: image.receipts.len(),
        idempotency_count: image.idempotency.len(),
        event_outbox_count: image.event_outbox.len(),
        audit: image.audit,
    };
    let pin = BackupPin::new(
        plan.pin_id.clone(),
        image
            .snapshots
            .iter()
            .map(|snapshot| snapshot.shard_id.clone())
            .collect(),
    );
    Ok(BackupOutcome {
        manifest,
        pin,
        consistency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::cursor::EventSequence;
    use oxkv_contracts::receipt::{AuditSequence, IdempotencyKey};

    fn shard(value: u8) -> ShardId {
        ShardId::new(vec![value])
    }

    fn snapshot(shard_value: u8, version: u64) -> SnapshotObject {
        let mut rows = BTreeMap::new();
        rows.insert(vec![shard_value], vec![shard_value]);
        SnapshotObject::new(
            format!("snapshot-{shard_value}"),
            shard(shard_value),
            CommitVersion::new(version),
            SnapshotPageVersion::new(1, 0),
            KeyEncodingVersion::new(1),
            rows,
            BTreeMap::new(),
        )
    }

    fn plan(global: bool) -> BackupPlan {
        let plan = BackupPlan::new(
            "pin-1",
            BackupVersions::new(
                ManifestRevision::new(3),
                SnapshotPageVersion::new(1, 0),
                KeyEncodingVersion::new(1),
                ManifestDigest::from_bytes([1; 32]),
                PolicyVersion::from_bytes([2; 32]),
                MapVersion::new(4),
                MapDigest::from_bytes([3; 32]),
            ),
        );
        if global {
            plan.with_global_point()
        } else {
            plan
        }
    }

    fn image() -> BackupImage {
        let mut image = BackupImage::new();
        image.add_snapshot(snapshot(1, 18));
        image.add_snapshot(snapshot(2, 9));
        image
    }

    #[test]
    fn a_backup_records_one_version_per_shard() {
        let outcome =
            capture(&plan(false), QuiesceState::Writable, &image()).expect("the backup captures");
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
        assert!(outcome.pin.is_held());
    }

    #[test]
    fn a_global_point_requires_a_global_quiesce() {
        let no_quiesce =
            capture(&plan(true), QuiesceState::Writable, &image()).expect("the backup captures");
        assert!(!no_quiesce.consistency.is_global());
        let quiesced = capture(&plan(true), QuiesceState::GlobalQuiesced, &image())
            .expect("the backup captures");
        assert!(quiesced.consistency.is_global());
    }

    #[test]
    fn a_backup_includes_receipts_idempotency_events_and_audit() {
        let mut image = image();
        image.receipts.push(AuthorityReceiptRecord::new(
            IdempotencyKey::new(vec![1]),
            oxkv_contracts::receipt::ReceiptState::Committed,
            Some(CommitVersion::new(18)),
            None,
        ));
        image.idempotency.push(IdempotencyRecord::new(
            IdempotencyKey::new(vec![1]),
            oxkv_contracts::version::Hash32::from_bytes([4; 32]),
        ));
        image
            .event_outbox
            .push(EventOutboxEntry::new(vec![1], EventSequence::new(3), 0));
        image.audit = AuditState::new(
            oxkv_contracts::version::Hash32::from_bytes([5; 32]),
            AuditSequence::new(7),
        );
        let outcome =
            capture(&plan(false), QuiesceState::Writable, &image).expect("the backup captures");
        assert_eq!(outcome.manifest.receipt_count, 1);
        assert_eq!(outcome.manifest.idempotency_count, 1);
        assert_eq!(outcome.manifest.event_outbox_count, 1);
        assert_eq!(outcome.manifest.audit.sequence.get(), 7);
    }

    #[test]
    fn a_held_pin_blocks_completion_after_loss() {
        let outcome =
            capture(&plan(false), QuiesceState::Writable, &image()).expect("the backup captures");
        let mut ledger = PinLedger::new();
        ledger.hold(&outcome.pin);
        assert!(ledger.any_held());
        ledger.lose("pin-1");
        let error = outcome
            .clone()
            .complete(&mut ledger)
            .expect_err("the pin is lost");
        assert_eq!(error.code(), "ops.backup_pin_lost");
        assert!(!ledger.any_held());
    }

    #[test]
    fn a_completed_backup_releases_the_pin() {
        let outcome =
            capture(&plan(false), QuiesceState::Writable, &image()).expect("the backup captures");
        let mut ledger = PinLedger::new();
        ledger.hold(&outcome.pin);
        let manifest = outcome.complete(&mut ledger).expect("the backup completes");
        assert_eq!(manifest.objects.len(), 2);
        assert_eq!(ledger.state("pin-1"), Some(PinState::Released));
        assert!(!ledger.any_held());
    }

    #[test]
    fn a_duplicate_shard_is_refused() {
        let mut image = BackupImage::new();
        image.add_snapshot(snapshot(1, 18));
        image.add_snapshot(snapshot(1, 19));
        let error =
            capture(&plan(false), QuiesceState::Writable, &image).expect_err("the shard repeats");
        assert_eq!(error.code(), "ops.migration_incomplete");
    }

    #[test]
    fn a_backup_from_a_scheduler_keeps_terminal_receipts() {
        let mut scheduler = SchedulerState::new();
        scheduler.add_terminal(AuthorityReceiptRecord::new(
            IdempotencyKey::new(vec![1]),
            oxkv_contracts::receipt::ReceiptState::Rejected,
            None,
            Some("shard.frozen_for_maintenance"),
        ));
        let image =
            BackupImage::from_scheduler(vec![snapshot(1, 18)], &scheduler, BackupState::default());
        assert_eq!(image.receipts.len(), 1);
    }

    #[test]
    fn a_backup_from_a_scheduler_keeps_pending_outbox_and_audit_state() {
        let mut scheduler = SchedulerState::new();
        scheduler.add_terminal(AuthorityReceiptRecord::new(
            IdempotencyKey::new(vec![1]),
            oxkv_contracts::receipt::ReceiptState::Committed,
            Some(CommitVersion::new(18)),
            None,
        ));
        let state = BackupState {
            idempotency: vec![IdempotencyRecord::new(
                IdempotencyKey::new(vec![2]),
                oxkv_contracts::version::Hash32::from_bytes([4; 32]),
            )],
            event_outbox: vec![EventOutboxEntry::new(
                b"key".to_vec(),
                EventSequence::new(5),
                7,
            )],
            audit: AuditState::new(
                oxkv_contracts::version::Hash32::from_bytes([5; 32]),
                AuditSequence::new(9),
            ),
            effects: vec![EffectRecord::new("email", true, 1)],
        };
        let image = BackupImage::from_scheduler(vec![snapshot(1, 18)], &scheduler, state.clone());
        assert_eq!(image.idempotency, state.idempotency);
        assert_eq!(image.event_outbox, state.event_outbox);
        assert_eq!(image.audit, state.audit);
        assert_eq!(image.effects, state.effects);
        assert_eq!(image.receipts.len(), 1);
    }
}
