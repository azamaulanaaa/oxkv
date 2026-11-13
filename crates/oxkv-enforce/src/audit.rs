//! The per-shard, hash-linked audit record.
//!
//! One write request produces at most one audit record. The record carries the
//! pinned policy version, the projection epoch, the fact versions, and a
//! bounded decision list. The chain uses BLAKE3 and links each record to the
//! previous record in the same shard.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::{
    AuditReference, AuditSequence, IdempotencyKey, ReceiptId, RequestId,
};
use oxkv_contracts::version::{
    DigestDomain, FactVersion, Hash32, KeyVersion, ManifestDigest, MapVersion, PolicyVersion,
    ProjectionEpoch, ProjectionWatermarks, ShardId,
};

use crate::auth::Principal;
use crate::codec::{push_bytes, push_option_u64, push_u8, push_u64};
use crate::policy::{Decision, DecisionVerdict, ReasonCode};
use crate::projection::StalenessReport;

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
///
/// The body holds the request identity, the pinned versions, the verdict, and
/// the bounded decision list. The record adds the sequence, the previous hash,
/// and the record hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecordBody {
    /// The request identity.
    pub request_id: RequestId,
    /// The client idempotency key.
    pub idempotency_key: IdempotencyKey,
    /// The authenticated principal.
    pub principal: Principal,
    /// The shard that owns the audit chain.
    pub shard_id: ShardId,
    /// The base version of the request.
    pub base_version: Option<KeyVersion>,
    /// The pinned policy version.
    pub policy_version: PolicyVersion,
    /// The manifest digest at commit.
    pub manifest_digest: ManifestDigest,
    /// The shard map version at commit.
    pub map_version: MapVersion,
    /// The projection epoch at commit.
    pub projection_epoch: ProjectionEpoch,
    /// The fact versions that the evaluation read.
    pub fact_versions: ProjectionWatermarks,
    /// The receipt that this record justifies.
    pub receipt_id: ReceiptId,
    /// The overall verdict.
    pub verdict: AuditVerdict,
    /// The overall reason code.
    pub reason: ReasonCode,
    /// The canonical request input digest.
    pub input_digest: Hash32,
    /// The measured staleness, when the write reached the staleness check.
    pub staleness: Option<StalenessReport>,
    /// The bounded per-row decision list.
    pub decisions: Vec<Decision>,
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
    /// A durable audit sink uses this constructor. The function computes the
    /// record hash from the sequence, the previous hash, and the body.
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
    push_bytes(buffer, body.principal.as_bytes());
    push_bytes(buffer, body.shard_id.as_bytes());
    push_option_u64(buffer, body.base_version.map(KeyVersion::get));
    push_bytes(buffer, body.policy_version.as_bytes());
    push_bytes(buffer, body.manifest_digest.as_bytes());
    push_u64(buffer, body.map_version.get());
    push_u64(buffer, body.projection_epoch.get());
    push_u64(
        buffer,
        u64::try_from(body.fact_versions.iter().count()).unwrap_or(u64::MAX),
    );
    for (source, version) in body.fact_versions.iter() {
        push_bytes(buffer, source.as_str().as_bytes());
        push_u64(buffer, version.get());
    }
    push_bytes(buffer, body.receipt_id.as_bytes());
    push_u8(buffer, body.verdict.wire_tag());
    push_bytes(buffer, body.reason.as_str().as_bytes());
    push_bytes(buffer, body.input_digest.as_bytes());
    push_staleness(buffer, body.staleness.as_ref());
    push_u64(
        buffer,
        u64::try_from(body.decisions.len()).unwrap_or(u64::MAX),
    );
    for decision in &body.decisions {
        push_bytes(buffer, decision.row_key.as_bytes());
        push_bytes(buffer, decision.rule.as_str().as_bytes());
        push_u8(
            buffer,
            match decision.verdict {
                DecisionVerdict::Allow => 1,
                DecisionVerdict::Reject => 2,
            },
        );
        push_bytes(buffer, decision.reason.as_str().as_bytes());
    }
}

/// Appends the optional staleness report to the hash input.
fn push_staleness(buffer: &mut Vec<u8>, report: Option<&StalenessReport>) {
    match report {
        Some(report) => {
            push_u8(buffer, 1);
            push_u64(
                buffer,
                u64::try_from(report.distances().len()).unwrap_or(u64::MAX),
            );
            for distance in report.distances() {
                push_bytes(buffer, distance.source.as_str().as_bytes());
                push_u64(buffer, distance.projected.get());
                push_option_u64(buffer, distance.current.map(FactVersion::get));
                push_option_u64(buffer, distance.distance);
                push_u64(buffer, distance.bound);
            }
            push_option_u64(buffer, report.max_distance());
            push_u8(buffer, u8::from(report.over_bound()));
            push_u8(buffer, u8::from(report.regression()));
            push_u8(buffer, u8::from(report.incomplete()));
        }
        None => push_u8(buffer, 0),
    }
}

/// An audit append failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuditError {
    /// The audit sink cannot append.
    #[error("The audit append is unavailable.")]
    Unavailable,
    /// The chain does not verify at the named sequence.
    #[error("The audit chain is invalid at sequence {sequence}.")]
    ChainInvalid {
        /// The first invalid sequence.
        sequence: AuditSequence,
    },
}

impl AuditError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "audit.unavailable",
            Self::ChainInvalid { .. } => "audit.chain_invalid",
        }
    }
}

/// Appends one audit record to a per-shard chain.
pub trait AuditSink {
    /// Appends one body and returns the sealed record.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::Unavailable`] when the sink cannot append.
    fn append(&mut self, body: AuditRecordBody) -> Result<AuditRecord, AuditError>;
}

/// A checkpoint at the end of one verified audit chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditCheckpoint {
    shard_id: ShardId,
    sequence: AuditSequence,
    record_hash: Hash32,
}

impl AuditCheckpoint {
    /// Returns the shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the last verified sequence.
    #[must_use]
    pub const fn sequence(&self) -> AuditSequence {
        self.sequence
    }

    /// Returns the last verified record hash.
    #[must_use]
    pub const fn record_hash(&self) -> Hash32 {
        self.record_hash
    }
}

/// An in-memory, append-only audit chain per shard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InMemoryAuditChain {
    chains: BTreeMap<ShardId, Vec<AuditRecord>>,
}

impl InMemoryAuditChain {
    /// Creates an empty audit chain.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the records for one shard.
    #[must_use]
    pub fn records(&self, shard_id: &ShardId) -> &[AuditRecord] {
        self.chains.get(shard_id).map_or(&[], Vec::as_slice)
    }

    /// Returns the number of records for one shard.
    #[must_use]
    pub fn record_count(&self, shard_id: &ShardId) -> usize {
        self.records(shard_id).len()
    }

    /// Returns the last record for one shard.
    #[must_use]
    pub fn last(&self, shard_id: &ShardId) -> Option<&AuditRecord> {
        self.records(shard_id).last()
    }

    /// Verifies one shard chain and returns its checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::ChainInvalid`] when a link, sequence, or record
    /// hash does not verify.
    pub fn verify(&self, shard_id: &ShardId) -> Result<AuditCheckpoint, AuditError> {
        let mut previous_hash = GENESIS_HASH;
        let mut expected_sequence = 1_u64;
        for record in self.records(shard_id) {
            if record.sequence.get() != expected_sequence
                || record.previous_hash != previous_hash
                || record.body.shard_id != *shard_id
                || !record.verify_hash()
            {
                return Err(AuditError::ChainInvalid {
                    sequence: AuditSequence::new(expected_sequence),
                });
            }
            previous_hash = record.record_hash;
            expected_sequence = expected_sequence.saturating_add(1);
        }
        Ok(AuditCheckpoint {
            shard_id: shard_id.clone(),
            sequence: AuditSequence::new(expected_sequence.saturating_sub(1)),
            record_hash: previous_hash,
        })
    }

    /// Verifies every shard chain.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::ChainInvalid`] when one chain does not verify.
    pub fn verify_all(&self) -> Result<(), AuditError> {
        for shard_id in self.chains.keys() {
            self.verify(shard_id)?;
        }
        Ok(())
    }
}

impl AuditSink for InMemoryAuditChain {
    fn append(&mut self, body: AuditRecordBody) -> Result<AuditRecord, AuditError> {
        let shard_id = body.shard_id.clone();
        let chain = self.chains.entry(shard_id).or_default();
        let sequence = AuditSequence::new(
            u64::try_from(chain.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        );
        let previous_hash = chain
            .last()
            .map_or(GENESIS_HASH, |record| record.record_hash);
        let record = AuditRecord::seal(sequence, previous_hash, body);
        chain.push(record.clone());
        Ok(record)
    }
}
