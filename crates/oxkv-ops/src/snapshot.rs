//! Immutable snapshot objects and the state that a backup carries.
//!
//! A [`SnapshotObject`] holds the user rows and the index entries of one
//! shard at one commit version. Its content digest covers canonical bytes.
//! The digest verifies the object before a restore serves a read.

use std::collections::BTreeMap;

use oxkv_contracts::cursor::EventSequence;
use oxkv_contracts::receipt::{AuditSequence, IdempotencyKey};
use oxkv_contracts::version::{
    CommitVersion, ContentDigest, Hash32, KeyEncodingVersion, ShardId, SnapshotPageVersion,
};

use crate::error::OpsError;

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    push_u64(output, u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    output.extend_from_slice(bytes);
}

fn push_text(output: &mut Vec<u8>, text: &str) {
    push_bytes(output, text.as_bytes());
}

/// One shard snapshot at one commit version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotObject {
    /// The stable object key in the backup store.
    pub object_key: String,
    /// The shard that owns the snapshot.
    pub shard_id: ShardId,
    /// The commit version of the snapshot.
    pub commit_version: CommitVersion,
    /// The snapshot and page format version.
    pub format: SnapshotPageVersion,
    /// The key encoding annex version.
    pub key_encoding: KeyEncodingVersion,
    /// The user rows in canonical key order.
    pub rows: BTreeMap<Vec<u8>, Vec<u8>>,
    /// The index entries in canonical key order.
    pub index: BTreeMap<Vec<u8>, Vec<String>>,
}

impl SnapshotObject {
    /// Creates a snapshot object.
    #[must_use]
    pub fn new(
        object_key: impl Into<String>,
        shard_id: ShardId,
        commit_version: CommitVersion,
        format: SnapshotPageVersion,
        key_encoding: KeyEncodingVersion,
        rows: BTreeMap<Vec<u8>, Vec<u8>>,
        index: BTreeMap<Vec<u8>, Vec<String>>,
    ) -> Self {
        Self {
            object_key: object_key.into(),
            shard_id,
            commit_version,
            format,
            key_encoding,
            rows,
            index,
        }
    }

    /// Encodes the snapshot as canonical bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut output = Vec::new();
        push_text(&mut output, "oxkv/shard-snapshot/v1");
        push_text(&mut output, &self.object_key);
        push_bytes(&mut output, self.shard_id.as_bytes());
        push_u64(&mut output, self.commit_version.get());
        push_u64(&mut output, u64::from(self.format.major()));
        push_u64(&mut output, u64::from(self.format.minor()));
        push_u64(&mut output, u64::from(self.key_encoding.get()));
        push_u64(
            &mut output,
            u64::try_from(self.rows.len()).unwrap_or(u64::MAX),
        );
        for (key, value) in &self.rows {
            push_bytes(&mut output, key);
            push_bytes(&mut output, value);
        }
        push_u64(
            &mut output,
            u64::try_from(self.index.len()).unwrap_or(u64::MAX),
        );
        for (key, postings) in &self.index {
            push_bytes(&mut output, key);
            push_u64(
                &mut output,
                u64::try_from(postings.len()).unwrap_or(u64::MAX),
            );
            for posting in postings {
                push_text(&mut output, posting);
            }
        }
        output
    }

    /// Computes the content digest of the snapshot.
    #[must_use]
    pub fn content_digest(&self) -> ContentDigest {
        ContentDigest::of_bytes(&self.canonical_bytes())
    }

    /// Verifies one expected digest.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::RestoreChecksumMismatch`] when the digest differs.
    pub fn verify(&self, expected: ContentDigest) -> Result<(), OpsError> {
        if self.content_digest() == expected {
            return Ok(());
        }
        Err(OpsError::RestoreChecksumMismatch {
            object: self.object_key.clone(),
        })
    }

    /// Returns one row value.
    #[must_use]
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.rows.get(key).map(Vec::as_slice)
    }

    /// Returns the index postings for one key.
    #[must_use]
    pub fn postings(&self, key: &[u8]) -> Option<&[String]> {
        self.index.get(key).map(Vec::as_slice)
    }
}

/// One event outbox entry for one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventOutboxEntry {
    /// The key that owns the event order.
    pub key: Vec<u8>,
    /// The per-key event sequence.
    pub sequence: EventSequence,
    /// The age of the entry in milliseconds.
    pub age_millis: u64,
}

impl EventOutboxEntry {
    /// Creates an event outbox entry.
    #[must_use]
    pub const fn new(key: Vec<u8>, sequence: EventSequence, age_millis: u64) -> Self {
        Self {
            key,
            sequence,
            age_millis,
        }
    }
}

/// One idempotency record that a backup carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRecord {
    /// The idempotency key.
    pub key: IdempotencyKey,
    /// The digest of the request body.
    pub body_digest: Hash32,
}

impl IdempotencyRecord {
    /// Creates an idempotency record.
    #[must_use]
    pub const fn new(key: IdempotencyKey, body_digest: Hash32) -> Self {
        Self { key, body_digest }
    }
}

/// The audit chain state that a backup carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuditState {
    /// The hash of the latest audit record.
    pub chain_tip: Hash32,
    /// The sequence of the latest audit record.
    pub sequence: AuditSequence,
}

impl AuditState {
    /// Creates an audit state.
    #[must_use]
    pub const fn new(chain_tip: Hash32, sequence: AuditSequence) -> Self {
        Self {
            chain_tip,
            sequence,
        }
    }

    /// Returns an empty audit state.
    #[must_use]
    pub const fn empty() -> Self {
        Self::new(Hash32::from_bytes([0; 32]), AuditSequence::new(0))
    }
}

impl Default for AuditState {
    fn default() -> Self {
        Self::empty()
    }
}

/// One effect record that a restore must not execute again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRecord {
    /// The effect identity.
    pub effect_id: String,
    /// True when the effect already ran.
    pub applied: bool,
    /// The number of completed executions.
    pub execution_count: u64,
}

impl EffectRecord {
    /// Creates an effect record.
    #[must_use]
    pub fn new(effect_id: impl Into<String>, applied: bool, execution_count: u64) -> Self {
        Self {
            effect_id: effect_id.into(),
            applied,
            execution_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> SnapshotObject {
        let mut rows = BTreeMap::new();
        rows.insert(b"key".to_vec(), b"value".to_vec());
        let mut index = BTreeMap::new();
        index.insert(b"key".to_vec(), vec!["token".to_owned()]);
        SnapshotObject::new(
            "snapshot-1",
            ShardId::new(vec![1]),
            CommitVersion::new(7),
            SnapshotPageVersion::new(1, 0),
            KeyEncodingVersion::new(1),
            rows,
            index,
        )
    }

    #[test]
    fn the_same_snapshot_produces_the_same_digest() {
        assert_eq!(snapshot().content_digest(), snapshot().content_digest());
    }

    #[test]
    fn a_changed_row_changes_the_digest() {
        let mut changed = snapshot();
        changed.rows.insert(b"key".to_vec(), b"other".to_vec());
        assert_ne!(snapshot().content_digest(), changed.content_digest());
    }

    #[test]
    fn a_wrong_digest_fails_verification() {
        let error = snapshot()
            .verify(ContentDigest::from_bytes([0; 32]))
            .expect_err("the digest differs");
        assert_eq!(error.code(), "ops.restore_checksum_mismatch");
        assert_eq!(
            error,
            OpsError::RestoreChecksumMismatch {
                object: "snapshot-1".to_owned()
            }
        );
    }

    #[test]
    fn a_snapshot_returns_rows_and_postings() {
        assert_eq!(snapshot().get(b"key"), Some(b"value".as_slice()));
        assert_eq!(
            snapshot().postings(b"key"),
            Some(["token".to_owned()].as_slice())
        );
        assert_eq!(snapshot().get(b"missing"), None);
    }
}
