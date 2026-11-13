//! The per-shard, hash-linked audit record.
//!
//! One terminal verdict writes one audit record. The record carries the
//! request identity, the idempotency key, the base version of each row, the
//! pinned policy version, the manifest digest, the verdict, and the reason.
//! The chain links each record to the previous record in the same shard.

use oxkv_contracts::receipt::{
    AuditReference, AuditSequence, IdempotencyKey, ReceiptId, RequestId,
};
use oxkv_contracts::version::{
    DigestDomain, Hash32, KeyVersion, ManifestDigest, PolicyVersion, ShardId,
};

use crate::request::Key;
use crate::util::{push_bytes, push_u64};

/// The genesis hash of one audit chain.
const GENESIS_HASH: Hash32 = Hash32::from_bytes([0; 32]);

/// The outcome that one audit record covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuditVerdict {
    /// The write committed.
    Committed,
    /// The authority rejected the write.
    Rejected,
    /// The base version did not match.
    Conflict,
}

impl AuditVerdict {
    /// Returns the stable verdict name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Committed => "Committed",
            Self::Rejected => "Rejected",
            Self::Conflict => "Conflict",
        }
    }

    /// Returns the stable wire tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Committed => 1,
            Self::Rejected => 2,
            Self::Conflict => 3,
        }
    }
}

/// The body of one audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecordBody {
    /// The request identity.
    pub request_id: RequestId,
    /// The client idempotency key.
    pub idempotency_key: IdempotencyKey,
    /// The shard that owns the audit chain.
    pub shard_id: ShardId,
    /// The base version of each row.
    pub base_versions: Vec<(Key, Option<KeyVersion>)>,
    /// The pinned policy version.
    pub policy_version: PolicyVersion,
    /// The manifest digest at commit.
    pub manifest_digest: ManifestDigest,
    /// The receipt that this record justifies.
    pub receipt_id: ReceiptId,
    /// The overall verdict.
    pub verdict: AuditVerdict,
    /// The overall reason code. A committed verdict has no reason.
    pub reason: Option<&'static str>,
    /// The canonical request body digest.
    pub input_digest: Hash32,
}

/// One sealed audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    sequence: AuditSequence,
    previous_hash: Hash32,
    record_hash: Hash32,
    body: AuditRecordBody,
}

impl AuditRecord {
    /// Seals one body into a record.
    ///
    /// The function computes the record hash from the sequence, the previous
    /// hash, and the body.
    #[must_use]
    pub fn seal(sequence: AuditSequence, previous_hash: Hash32, body: AuditRecordBody) -> Self {
        let record_hash = Self::compute_hash(sequence, previous_hash, &body);
        Self {
            sequence,
            previous_hash,
            record_hash,
            body,
        }
    }

    /// Returns the position in the per-shard chain.
    #[must_use]
    pub const fn sequence(&self) -> AuditSequence {
        self.sequence
    }

    /// Returns the previous record hash.
    #[must_use]
    pub const fn previous_hash(&self) -> Hash32 {
        self.previous_hash
    }

    /// Returns the record hash.
    #[must_use]
    pub const fn record_hash(&self) -> Hash32 {
        self.record_hash
    }

    /// Returns the record body.
    #[must_use]
    pub const fn body(&self) -> &AuditRecordBody {
        &self.body
    }

    /// Returns the audit reference for the receipt.
    #[must_use]
    pub fn audit_reference(&self) -> AuditReference {
        AuditReference::new(self.body.shard_id.clone(), self.sequence, self.record_hash)
    }

    /// Checks the record hash against the body.
    #[must_use]
    pub fn verify_hash(&self) -> bool {
        Self::compute_hash(self.sequence, self.previous_hash, &self.body) == self.record_hash
    }

    /// Computes the hash of one record.
    fn compute_hash(
        sequence: AuditSequence,
        previous_hash: Hash32,
        body: &AuditRecordBody,
    ) -> Hash32 {
        let mut buffer = Vec::new();
        push_u64(&mut buffer, sequence.get());
        push_bytes(&mut buffer, previous_hash.as_bytes());
        push_body(&mut buffer, body);
        Hash32::of_domain(DigestDomain::AuditRecord, &buffer)
    }
}

/// Appends the body fields to the hash input.
fn push_body(buffer: &mut Vec<u8>, body: &AuditRecordBody) {
    push_bytes(buffer, body.request_id.as_bytes());
    push_bytes(buffer, body.idempotency_key.as_bytes());
    push_bytes(buffer, body.shard_id.as_bytes());
    push_u64(
        buffer,
        u64::try_from(body.base_versions.len()).unwrap_or(u64::MAX),
    );
    for (key, base_version) in &body.base_versions {
        push_bytes(buffer, key.as_bytes());
        match base_version {
            Some(base_version) => {
                buffer.push(1);
                push_u64(buffer, base_version.get());
            }
            None => buffer.push(0),
        }
    }
    push_bytes(buffer, body.policy_version.as_bytes());
    push_bytes(buffer, body.manifest_digest.as_bytes());
    push_bytes(buffer, body.receipt_id.as_bytes());
    buffer.push(body.verdict.wire_tag());
    match body.reason {
        Some(reason) => {
            buffer.push(1);
            push_bytes(buffer, reason.as_bytes());
        }
        None => buffer.push(0),
    }
    push_bytes(buffer, body.input_digest.as_bytes());
}

/// An append-only, hash-linked audit chain for one shard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditChain {
    records: Vec<AuditRecord>,
}

impl AuditChain {
    /// Creates an empty audit chain.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the records in chain order.
    #[must_use]
    pub fn records(&self) -> &[AuditRecord] {
        &self.records
    }

    /// Returns the number of records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns true when the chain has no record.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Seals the next record without appending it.
    #[must_use]
    pub fn seal_next(&self, body: AuditRecordBody) -> AuditRecord {
        let sequence = AuditSequence::new(
            u64::try_from(self.records.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        );
        let previous_hash = self
            .records
            .last()
            .map_or(GENESIS_HASH, AuditRecord::record_hash);
        AuditRecord::seal(sequence, previous_hash, body)
    }

    /// Appends one sealed record.
    pub fn push(&mut self, record: AuditRecord) {
        self.records.push(record);
    }

    /// Checks every link and record hash.
    #[must_use]
    pub fn verify(&self) -> bool {
        let mut previous_hash = GENESIS_HASH;
        let mut expected_sequence = 1_u64;
        for record in &self.records {
            if record.sequence.get() != expected_sequence
                || record.previous_hash != previous_hash
                || !record.verify_hash()
            {
                return false;
            }
            previous_hash = record.record_hash;
            expected_sequence = expected_sequence.saturating_add(1);
        }
        true
    }
}
