//! The projection feed.
//!
//! A feed starts with one snapshot. It continues with ordered diffs. A cursor
//! resumes after one applied sequence. An expired epoch or a missing sequence
//! falls back to a fresh snapshot.

use oxkv_contracts::cursor::ProjectionCursor;
use oxkv_contracts::version::{
    FactSourceId, FactVersion, Hash32, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
};

use crate::codec::{CanonicalEncode, Encoder};
use crate::error::ProjectionError;
use crate::fact::{FactId, FactValue, StoredFact};

const SNAPSHOT_TAG: &[u8] = b"oxkv/projection-snapshot/v1";
const DIFF_TAG: &[u8] = b"oxkv/projection-diff/v1";

/// One full projection snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSnapshot {
    epoch: ProjectionEpoch,
    policy_version: PolicyVersion,
    fact_set_digest: Hash32,
    watermarks: ProjectionWatermarks,
    facts: Vec<StoredFact>,
    checksum: Hash32,
}

impl ProjectionSnapshot {
    /// Creates a snapshot and computes its checksum.
    #[must_use]
    pub fn new(
        epoch: ProjectionEpoch,
        policy_version: PolicyVersion,
        fact_set_digest: Hash32,
        watermarks: ProjectionWatermarks,
        mut facts: Vec<StoredFact>,
    ) -> Self {
        facts.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
        let mut snapshot = Self {
            epoch,
            policy_version,
            fact_set_digest,
            watermarks,
            facts,
            checksum: Hash32::from_bytes([0; 32]),
        };
        snapshot.checksum = Hash32::of(&snapshot.canonical_bytes());
        snapshot
    }

    /// Creates a snapshot with an explicit checksum.
    ///
    /// Use this constructor after decoding a frame, or to inject a corrupt
    /// frame in a fault test.
    #[must_use]
    pub fn with_checksum(
        epoch: ProjectionEpoch,
        policy_version: PolicyVersion,
        fact_set_digest: Hash32,
        watermarks: ProjectionWatermarks,
        mut facts: Vec<StoredFact>,
        checksum: Hash32,
    ) -> Self {
        facts.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
        Self {
            epoch,
            policy_version,
            fact_set_digest,
            watermarks,
            facts,
            checksum,
        }
    }

    /// Returns the epoch generation.
    #[must_use]
    pub const fn epoch(&self) -> ProjectionEpoch {
        self.epoch
    }

    /// Returns the policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the fact set digest.
    #[must_use]
    pub const fn fact_set_digest(&self) -> Hash32 {
        self.fact_set_digest
    }

    /// Returns the watermarks.
    #[must_use]
    pub const fn watermarks(&self) -> &ProjectionWatermarks {
        &self.watermarks
    }

    /// Returns the facts.
    #[must_use]
    pub fn facts(&self) -> &[StoredFact] {
        &self.facts
    }

    /// Returns the checksum.
    #[must_use]
    pub const fn checksum(&self) -> Hash32 {
        self.checksum
    }

    /// Verifies the checksum against the canonical bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::ChecksumFailed`] when the bytes do not
    /// match the checksum.
    pub fn verify(&self) -> Result<(), ProjectionError> {
        if Hash32::of(&self.canonical_bytes()) == self.checksum {
            Ok(())
        } else {
            Err(ProjectionError::ChecksumFailed)
        }
    }
}

impl CanonicalEncode for ProjectionSnapshot {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.bytes(SNAPSHOT_TAG);
        encoder.u64(self.epoch.get());
        encoder.bytes(self.policy_version.as_bytes());
        encoder.bytes(self.fact_set_digest.as_bytes());
        encoder.u64(u64::try_from(self.watermarks.iter().count()).unwrap_or(u64::MAX));
        for (source, version) in self.watermarks.iter() {
            encoder.text(source.as_str());
            encoder.u64(version.get());
        }
        encoder.u64(u64::try_from(self.facts.len()).unwrap_or(u64::MAX));
        for fact in &self.facts {
            encoder.bytes(&fact.canonical_bytes());
        }
        encoder.finish()
    }
}

/// One change to one fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactChange {
    fact_id: FactId,
    fact_version: FactVersion,
    value: Option<FactValue>,
}

impl FactChange {
    /// Creates a fact insert or update change.
    #[must_use]
    pub const fn upsert(fact_id: FactId, fact_version: FactVersion, value: FactValue) -> Self {
        Self {
            fact_id,
            fact_version,
            value: Some(value),
        }
    }

    /// Creates a fact removal change.
    #[must_use]
    pub const fn remove(fact_id: FactId, fact_version: FactVersion) -> Self {
        Self {
            fact_id,
            fact_version,
            value: None,
        }
    }

    /// Returns the fact identity.
    #[must_use]
    pub const fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    /// Returns the fact version.
    #[must_use]
    pub const fn fact_version(&self) -> FactVersion {
        self.fact_version
    }

    /// Returns the new value, or `None` for a removal.
    #[must_use]
    pub const fn value(&self) -> Option<&FactValue> {
        self.value.as_ref()
    }
}

impl CanonicalEncode for FactChange {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.text(self.fact_id.as_str());
        encoder.u64(self.fact_version.get());
        match &self.value {
            Some(value) => {
                encoder.u8(1);
                encoder.bytes(&value.canonical_bytes());
            }
            None => encoder.u8(0),
        }
        encoder.finish()
    }
}

/// One ordered diff for one fact source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionDiff {
    epoch: ProjectionEpoch,
    sequence: u64,
    source: FactSourceId,
    watermark: FactVersion,
    changes: Vec<FactChange>,
    checksum: Hash32,
}

impl ProjectionDiff {
    /// Creates a diff and computes its checksum.
    #[must_use]
    pub fn new(
        epoch: ProjectionEpoch,
        sequence: u64,
        source: FactSourceId,
        watermark: FactVersion,
        mut changes: Vec<FactChange>,
    ) -> Self {
        changes.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
        let mut diff = Self {
            epoch,
            sequence,
            source,
            watermark,
            changes,
            checksum: Hash32::from_bytes([0; 32]),
        };
        diff.checksum = Hash32::of(&diff.canonical_bytes());
        diff
    }

    /// Creates a diff with an explicit checksum.
    ///
    /// Use this constructor after decoding a frame, or to inject a corrupt
    /// frame in a fault test.
    #[must_use]
    pub fn with_checksum(
        epoch: ProjectionEpoch,
        sequence: u64,
        source: FactSourceId,
        watermark: FactVersion,
        mut changes: Vec<FactChange>,
        checksum: Hash32,
    ) -> Self {
        changes.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
        Self {
            epoch,
            sequence,
            source,
            watermark,
            changes,
            checksum,
        }
    }

    /// Returns the epoch generation.
    #[must_use]
    pub const fn epoch(&self) -> ProjectionEpoch {
        self.epoch
    }

    /// Returns the diff sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the fact source.
    #[must_use]
    pub const fn source(&self) -> &FactSourceId {
        &self.source
    }

    /// Returns the resulting source watermark.
    #[must_use]
    pub const fn watermark(&self) -> FactVersion {
        self.watermark
    }

    /// Returns the changes in fact identity order.
    #[must_use]
    pub fn changes(&self) -> &[FactChange] {
        &self.changes
    }

    /// Returns the checksum.
    #[must_use]
    pub const fn checksum(&self) -> Hash32 {
        self.checksum
    }

    /// Verifies the checksum against the canonical bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::ChecksumFailed`] when the bytes do not
    /// match the checksum.
    pub fn verify(&self) -> Result<(), ProjectionError> {
        if Hash32::of(&self.canonical_bytes()) == self.checksum {
            Ok(())
        } else {
            Err(ProjectionError::ChecksumFailed)
        }
    }
}

impl CanonicalEncode for ProjectionDiff {
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.bytes(DIFF_TAG);
        encoder.u64(self.epoch.get());
        encoder.u64(self.sequence);
        encoder.text(self.source.as_str());
        encoder.u64(self.watermark.get());
        encoder.u64(u64::try_from(self.changes.len()).unwrap_or(u64::MAX));
        for change in &self.changes {
            encoder.bytes(&change.canonical_bytes());
        }
        encoder.finish()
    }
}

/// The resume position of one projection feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionFeedCursor {
    cursor: ProjectionCursor,
    sequence: u64,
}

impl ProjectionFeedCursor {
    /// Creates a feed cursor.
    #[must_use]
    pub const fn new(cursor: ProjectionCursor, sequence: u64) -> Self {
        Self { cursor, sequence }
    }

    /// Returns the projection cursor.
    #[must_use]
    pub const fn cursor(&self) -> &ProjectionCursor {
        &self.cursor
    }

    /// Returns the last applied diff sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns true when the projection epoch is still retained.
    #[must_use]
    pub fn is_readable_at(&self, oldest_epoch: ProjectionEpoch) -> bool {
        self.cursor.is_readable_at(oldest_epoch)
    }
}

/// Checks one projection cursor against the oldest retained epoch.
///
/// # Errors
///
/// Returns [`ProjectionError::EpochInvalid`] when the cursor epoch is older
/// than the retained epoch.
pub fn validate_cursor(
    cursor: &ProjectionCursor,
    oldest_epoch: ProjectionEpoch,
) -> Result<(), ProjectionError> {
    if cursor.is_readable_at(oldest_epoch) {
        Ok(())
    } else {
        Err(ProjectionError::EpochInvalid)
    }
}

/// The answer to one resume request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedResume {
    /// Apply the ordered diffs after the cursor.
    Diffs(Vec<ProjectionDiff>),
    /// Replace the projection from this snapshot.
    FreshSnapshot(ProjectionSnapshot),
}

impl FeedResume {
    /// Returns true when the answer is a fresh snapshot.
    #[must_use]
    pub const fn is_fresh_snapshot(&self) -> bool {
        matches!(self, Self::FreshSnapshot(_))
    }
}

/// One projection feed with a snapshot and an ordered diff tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionFeed {
    snapshot: ProjectionSnapshot,
    diffs: Vec<ProjectionDiff>,
}

impl ProjectionFeed {
    /// Creates a feed.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::ChecksumFailed`] when one artifact fails its
    /// checksum. Returns [`ProjectionError::FeedSequenceInvalid`] when the diff
    /// sequence has a gap. Returns [`ProjectionError::EpochInvalid`] when a
    /// diff names another epoch.
    pub fn new(
        snapshot: ProjectionSnapshot,
        diffs: Vec<ProjectionDiff>,
    ) -> Result<Self, ProjectionError> {
        snapshot.verify()?;
        for (index, diff) in diffs.iter().enumerate() {
            diff.verify()?;
            if diff.epoch() != snapshot.epoch() {
                return Err(ProjectionError::EpochInvalid);
            }
            let expected = u64::try_from(index).unwrap_or(u64::MAX) + 1;
            if diff.sequence() != expected {
                return Err(ProjectionError::FeedSequenceInvalid {
                    expected,
                    found: diff.sequence(),
                });
            }
        }
        Ok(Self { snapshot, diffs })
    }

    /// Returns the base snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &ProjectionSnapshot {
        &self.snapshot
    }

    /// Returns the ordered diffs.
    #[must_use]
    pub fn diffs(&self) -> &[ProjectionDiff] {
        &self.diffs
    }

    /// Returns the feed epoch.
    #[must_use]
    pub const fn epoch(&self) -> ProjectionEpoch {
        self.snapshot.epoch()
    }

    /// Resumes after a cursor.
    ///
    /// An epoch mismatch or a sequence outside the tail returns a fresh
    /// snapshot. The caller applies the snapshot atomically.
    #[must_use]
    pub fn resume(&self, cursor: &ProjectionFeedCursor) -> FeedResume {
        if cursor.cursor().epoch != self.snapshot.epoch() {
            return FeedResume::FreshSnapshot(self.snapshot.clone());
        }
        let sequence = usize::try_from(cursor.sequence()).unwrap_or(usize::MAX);
        if sequence > self.diffs.len() {
            return FeedResume::FreshSnapshot(self.snapshot.clone());
        }
        FeedResume::Diffs(self.diffs[sequence..].to_vec())
    }
}
