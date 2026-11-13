//! The per-shard write audit chain.
//!
//! Every write enforcement verdict has one record. The record carries the
//! request identity, the idempotency key, the principal, the shard, the base
//! version, the pinned versions, the fact versions, the rule identity, the
//! decision, the reason code, the input digest, the previous hash, and the
//! record hash.
//!
//! The chain is append-only and hash-linked with `BLAKE3`. Checkpoints are
//! signed structures. The crate holds no key material.

use core::fmt;
use std::collections::BTreeMap;

use oxkv_contracts::receipt::{AuditSequence, IdempotencyKey, ReceiptId, RequestId};
use oxkv_contracts::version::{
    DigestDomain, FactSourceId, FactVersion, Hash32, KeyVersion, ManifestDigest, MapVersion,
    PolicyVersion, ProjectionEpoch, ShardId, Signature,
};

use crate::auth::PrincipalId;
use crate::error::SecurityError;
use crate::secret::KeyIdentifier;

/// The previous hash of the first record or checkpoint.
pub const GENESIS_HASH: Hash32 = Hash32::from_bytes([0; 32]);

/// A rule identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleIdentity(String);

impl RuleIdentity {
    /// Creates a rule identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the rule identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns true when the identity is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The decision of one write enforcement verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AuditDecision {
    /// The write is durable.
    Committed,
    /// The authority refused the write.
    Rejected,
    /// The base version did not match.
    Conflict,
}

impl AuditDecision {
    /// Every decision.
    pub const ALL: [Self; 3] = [Self::Committed, Self::Rejected, Self::Conflict];

    /// Returns the canonical decision name.
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

    /// Reads a decision from its stable wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Committed),
            2 => Some(Self::Rejected),
            3 => Some(Self::Conflict),
            _ => None,
        }
    }
}

impl fmt::Display for AuditDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The decision of one request row.
///
/// The audit record carries one row decision for each row of the request. An
/// allowed row has no reason code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRowDecision {
    /// The row key.
    pub key: Vec<u8>,
    /// The rule identity for the row.
    pub rule_identity: RuleIdentity,
    /// True when the row passed the rule.
    pub allowed: bool,
    /// The coarse reason code. An allowed row has no reason code.
    pub reason_code: Option<&'static str>,
}

/// The body of one audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// The request identity.
    pub request_id: RequestId,
    /// The client idempotency key.
    pub idempotency_key: IdempotencyKey,
    /// The authenticated principal.
    pub principal: PrincipalId,
    /// The shard that owns the chain.
    pub shard_id: ShardId,
    /// The base key version of the request.
    pub base_version: KeyVersion,
    /// The base version of every row.
    pub base_versions: Vec<(Vec<u8>, Option<KeyVersion>)>,
    /// The pinned policy version.
    pub policy_version: PolicyVersion,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The projection epoch.
    pub projection_epoch: ProjectionEpoch,
    /// The version of every fact that the rule read.
    pub fact_versions: BTreeMap<FactSourceId, FactVersion>,
    /// The rule identity.
    pub rule_identity: RuleIdentity,
    /// The bounded decision list for the rows.
    pub decisions: Vec<AuditRowDecision>,
    /// The enforcement decision.
    pub decision: AuditDecision,
    /// The coarse reason code. A committed verdict has no reason code.
    pub reason_code: Option<&'static str>,
    /// The receipt that this record justifies.
    pub receipt_id: Option<ReceiptId>,
    /// The digest of every signed request field.
    pub input_digest: Hash32,
}

/// One record in a per-shard audit chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// The one-based position in the chain.
    pub audit_sequence: AuditSequence,
    /// The record body.
    pub entry: AuditEntry,
    /// The hash of the previous record.
    pub previous_hash: Hash32,
    /// The hash of this record.
    pub record_hash: Hash32,
}

impl AuditRecord {
    /// Creates a record and computes its hash.
    #[must_use]
    pub fn new(audit_sequence: AuditSequence, entry: AuditEntry, previous_hash: Hash32) -> Self {
        let record_hash = Self::compute_hash(audit_sequence, &entry, previous_hash);
        Self {
            audit_sequence,
            entry,
            previous_hash,
            record_hash,
        }
    }

    /// Returns true when the record hash matches the body and the previous
    /// hash.
    #[must_use]
    pub fn verify_hash(&self) -> bool {
        Self::compute_hash(self.audit_sequence, &self.entry, self.previous_hash) == self.record_hash
    }

    /// Computes the hash of one record body and its previous hash.
    #[must_use]
    pub fn compute_hash(
        audit_sequence: AuditSequence,
        entry: &AuditEntry,
        previous_hash: Hash32,
    ) -> Hash32 {
        let mut hasher = blake3::Hasher::new();
        hasher.update(DigestDomain::AuditRecord.tag().as_bytes());
        hasher.update(&[0]);
        hasher.update(previous_hash.as_bytes());
        let mut body = Vec::new();
        push_u64(&mut body, audit_sequence.get());
        push_bytes(&mut body, entry.request_id.as_bytes());
        push_bytes(&mut body, entry.idempotency_key.as_bytes());
        push_bytes(&mut body, entry.principal.as_bytes());
        push_bytes(&mut body, entry.shard_id.as_bytes());
        push_u64(&mut body, entry.base_version.get());
        push_u64(
            &mut body,
            u64::try_from(entry.base_versions.len()).unwrap_or(u64::MAX),
        );
        for (key, base_version) in &entry.base_versions {
            push_bytes(&mut body, key);
            match base_version {
                Some(base_version) => {
                    body.push(1);
                    push_u64(&mut body, base_version.get());
                }
                None => body.push(0),
            }
        }
        push_bytes(&mut body, entry.policy_version.as_bytes());
        push_bytes(&mut body, entry.manifest_digest.as_bytes());
        push_u64(&mut body, entry.map_version.get());
        push_u64(&mut body, entry.projection_epoch.get());
        push_u64(
            &mut body,
            u64::try_from(entry.fact_versions.len()).unwrap_or(u64::MAX),
        );
        for (source, fact_version) in &entry.fact_versions {
            push_bytes(&mut body, source.as_str().as_bytes());
            push_u64(&mut body, fact_version.get());
        }
        push_bytes(&mut body, entry.rule_identity.as_str().as_bytes());
        push_u64(
            &mut body,
            u64::try_from(entry.decisions.len()).unwrap_or(u64::MAX),
        );
        for decision in &entry.decisions {
            push_bytes(&mut body, &decision.key);
            push_bytes(&mut body, decision.rule_identity.as_str().as_bytes());
            body.push(u8::from(decision.allowed));
            match decision.reason_code {
                Some(reason) => {
                    body.push(1);
                    push_bytes(&mut body, reason.as_bytes());
                }
                None => body.push(0),
            }
        }
        body.push(entry.decision.wire_tag());
        match entry.reason_code {
            Some(reason) => {
                body.push(1);
                push_bytes(&mut body, reason.as_bytes());
            }
            None => body.push(0),
        }
        match entry.receipt_id {
            Some(receipt_id) => {
                body.push(1);
                push_bytes(&mut body, receipt_id.as_bytes());
            }
            None => body.push(0),
        }
        push_bytes(&mut body, entry.input_digest.as_bytes());
        hasher.update(&body);
        Hash32::from_bytes(*hasher.finalize().as_bytes())
    }
}

/// A signed checkpoint over one chain tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditCheckpoint {
    /// The shard that owns the chain.
    pub shard_id: ShardId,
    /// The covered audit sequence.
    pub audit_sequence: AuditSequence,
    /// The covered record hash.
    pub record_hash: Hash32,
    /// The hash of the previous checkpoint.
    pub previous_checkpoint_hash: Hash32,
    /// The key identifier that signed the checkpoint.
    pub key_id: KeyIdentifier,
    /// The structural signature value.
    pub signature: Signature,
    /// The hash of the checkpoint.
    pub checkpoint_hash: Hash32,
}

/// The kind of one checkpoint signer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SignerKind {
    /// A production signer. The key material lives outside this crate.
    Production,
    /// A structural signer for tests and local runs.
    Structural,
}

/// A signer for one audit checkpoint.
///
/// The signer holds key material outside this crate. The crate stores only the
/// key identifier and the signature value.
pub trait CheckpointSigner: fmt::Debug {
    /// Returns the signing key identifier.
    fn key_id(&self) -> &KeyIdentifier;

    /// Signs one checkpoint digest.
    fn sign(&self, checkpoint_digest: Hash32) -> Signature;

    /// Returns the signer kind.
    ///
    /// A production signer keeps the default value. A structural signer
    /// returns [`SignerKind::Structural`].
    fn kind(&self) -> SignerKind {
        SignerKind::Production
    }
}

/// A structural checkpoint signer with no key material.
///
/// The signer derives a deterministic 64-byte value from the key identifier
/// and the checkpoint digest. It is a structural test and operations seam. It
/// is not a production signature scheme. A production configuration must not
/// use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralSigner {
    key_id: KeyIdentifier,
}

impl StructuralSigner {
    /// Creates a structural signer.
    #[must_use]
    pub const fn new(key_id: KeyIdentifier) -> Self {
        Self { key_id }
    }
}

impl CheckpointSigner for StructuralSigner {
    fn key_id(&self) -> &KeyIdentifier {
        &self.key_id
    }

    fn kind(&self) -> SignerKind {
        SignerKind::Structural
    }

    fn sign(&self, checkpoint_digest: Hash32) -> Signature {
        let mut first = blake3::Hasher::new();
        first.update(b"oxkv/checkpoint-signature/v1");
        first.update(&[0]);
        first.update(self.key_id.as_str().as_bytes());
        first.update(checkpoint_digest.as_bytes());
        let first = first.finalize();
        let mut second = blake3::Hasher::new();
        second.update(b"oxkv/checkpoint-signature/v1/part-2");
        second.update(first.as_bytes());
        let second = second.finalize();
        let mut signature = [0_u8; 64];
        signature[..32].copy_from_slice(first.as_bytes());
        signature[32..].copy_from_slice(second.as_bytes());
        Signature::from_bytes(signature)
    }
}

/// The result of one chain verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditVerificationReport {
    /// The number of verified records.
    pub records: usize,
    /// The number of verified checkpoints.
    pub checkpoints: usize,
    /// The hash of the last record.
    pub tip: Option<Hash32>,
    /// The sequence of the last checkpoint.
    pub last_checkpoint: Option<AuditSequence>,
}

/// An append-only per-shard audit chain.
#[derive(Debug, Clone)]
pub struct AuditChain {
    shard_id: ShardId,
    records: Vec<AuditRecord>,
    checkpoints: Vec<AuditCheckpoint>,
    append_available: bool,
}

impl AuditChain {
    /// Creates an empty chain for one shard.
    #[must_use]
    pub fn new(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            records: Vec::new(),
            checkpoints: Vec::new(),
            append_available: true,
        }
    }

    /// Creates a chain from imported parts.
    ///
    /// The function does not verify the parts. The caller must call
    /// [`AuditChain::verify`] before the chain is trusted.
    #[must_use]
    pub fn from_parts(
        shard_id: ShardId,
        records: Vec<AuditRecord>,
        checkpoints: Vec<AuditCheckpoint>,
    ) -> Self {
        Self {
            shard_id,
            records,
            checkpoints,
            append_available: true,
        }
    }

    /// Returns the shard that owns the chain.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns every record in sequence order.
    #[must_use]
    pub fn records(&self) -> &[AuditRecord] {
        &self.records
    }

    /// Returns every checkpoint in sequence order.
    #[must_use]
    pub fn checkpoints(&self) -> &[AuditCheckpoint] {
        &self.checkpoints
    }

    /// Returns the last checkpoint.
    #[must_use]
    pub fn last_checkpoint(&self) -> Option<&AuditCheckpoint> {
        self.checkpoints.last()
    }

    /// Returns the number of records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns true when the chain has no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Returns true when the chain can accept a record.
    #[must_use]
    pub const fn is_append_available(&self) -> bool {
        self.append_available
    }

    /// Enables or disables the append path.
    ///
    /// Operations inject a failed storage append with this method. The chain
    /// keeps its current records when an append fails.
    pub fn set_append_available(&mut self, available: bool) {
        self.append_available = available;
    }

    /// Appends one record.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::AuditUnavailable`] when the append path is
    /// disabled. Returns [`SecurityError::AuditChainInvalid`] when the entry
    /// belongs to another shard.
    pub fn append(&mut self, entry: AuditEntry) -> Result<AuditRecord, SecurityError> {
        if !self.append_available {
            return Err(SecurityError::AuditUnavailable);
        }
        if entry.shard_id != self.shard_id {
            return Err(SecurityError::AuditChainInvalid);
        }
        let next = self
            .records
            .len()
            .checked_add(1)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(SecurityError::AuditChainInvalid)?;
        let previous_hash = self
            .records
            .last()
            .map_or(GENESIS_HASH, |record| record.record_hash);
        let record = AuditRecord::new(AuditSequence::new(next), entry, previous_hash);
        self.records.push(record.clone());
        Ok(record)
    }

    /// Creates a checkpoint over the last record.
    ///
    /// The function returns the last checkpoint when it already covers the
    /// last record.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::AuditChainInvalid`] when the chain has no
    /// record.
    pub fn checkpoint(
        &mut self,
        signer: &dyn CheckpointSigner,
    ) -> Result<AuditCheckpoint, SecurityError> {
        let record = self
            .records
            .last()
            .ok_or(SecurityError::AuditChainInvalid)?
            .clone();
        if let Some(last) = self.checkpoints.last()
            && last.audit_sequence == record.audit_sequence
        {
            return Ok(last.clone());
        }
        let previous_checkpoint_hash = self
            .checkpoints
            .last()
            .map_or(GENESIS_HASH, |checkpoint| checkpoint.checkpoint_hash);
        let digest = checkpoint_digest(
            &self.shard_id,
            record.audit_sequence,
            record.record_hash,
            previous_checkpoint_hash,
            signer.key_id(),
        );
        let signature = signer.sign(digest);
        let checkpoint_hash = checkpoint_hash(digest, &signature);
        let checkpoint = AuditCheckpoint {
            shard_id: self.shard_id.clone(),
            audit_sequence: record.audit_sequence,
            record_hash: record.record_hash,
            previous_checkpoint_hash,
            key_id: signer.key_id().clone(),
            signature,
            checkpoint_hash,
        };
        self.checkpoints.push(checkpoint.clone());
        Ok(checkpoint)
    }

    /// Verifies the hash chain and every checkpoint.
    ///
    /// A non-empty chain has at least one checkpoint. The last checkpoint must
    /// cover the last record.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::AuditChainInvalid`] when a record hash, a
    /// previous hash, a sequence, or a checkpoint does not match.
    pub fn verify(&self) -> Result<AuditVerificationReport, SecurityError> {
        if self.records.is_empty() {
            if self.checkpoints.is_empty() {
                return Ok(AuditVerificationReport {
                    records: 0,
                    checkpoints: 0,
                    tip: None,
                    last_checkpoint: None,
                });
            }
            return Err(SecurityError::AuditChainInvalid);
        }
        if self.checkpoints.is_empty() {
            return Err(SecurityError::AuditChainInvalid);
        }
        for (index, record) in self.records.iter().enumerate() {
            let sequence = u64::try_from(index)
                .map_err(|_| SecurityError::AuditChainInvalid)?
                .checked_add(1)
                .ok_or(SecurityError::AuditChainInvalid)?;
            if record.audit_sequence.get() != sequence {
                return Err(SecurityError::AuditChainInvalid);
            }
            if record.entry.shard_id != self.shard_id {
                return Err(SecurityError::AuditChainInvalid);
            }
            let previous_hash = if index == 0 {
                GENESIS_HASH
            } else {
                self.records[index - 1].record_hash
            };
            if record.previous_hash != previous_hash {
                return Err(SecurityError::AuditChainInvalid);
            }
            if AuditRecord::compute_hash(record.audit_sequence, &record.entry, previous_hash)
                != record.record_hash
            {
                return Err(SecurityError::AuditChainInvalid);
            }
        }
        let tip_sequence =
            u64::try_from(self.records.len()).map_err(|_| SecurityError::AuditChainInvalid)?;
        let mut previous_checkpoint_hash = GENESIS_HASH;
        let mut previous_sequence = 0_u64;
        for checkpoint in &self.checkpoints {
            if checkpoint.shard_id != self.shard_id {
                return Err(SecurityError::AuditChainInvalid);
            }
            let sequence = checkpoint.audit_sequence.get();
            if sequence == 0 || sequence > tip_sequence || sequence <= previous_sequence {
                return Err(SecurityError::AuditChainInvalid);
            }
            let index =
                usize::try_from(sequence - 1).map_err(|_| SecurityError::AuditChainInvalid)?;
            let record = &self.records[index];
            if checkpoint.record_hash != record.record_hash {
                return Err(SecurityError::AuditChainInvalid);
            }
            if checkpoint.previous_checkpoint_hash != previous_checkpoint_hash {
                return Err(SecurityError::AuditChainInvalid);
            }
            let digest = checkpoint_digest(
                &self.shard_id,
                checkpoint.audit_sequence,
                checkpoint.record_hash,
                checkpoint.previous_checkpoint_hash,
                &checkpoint.key_id,
            );
            if checkpoint_hash(digest, &checkpoint.signature) != checkpoint.checkpoint_hash {
                return Err(SecurityError::AuditChainInvalid);
            }
            previous_checkpoint_hash = checkpoint.checkpoint_hash;
            previous_sequence = sequence;
        }
        let last = self
            .checkpoints
            .last()
            .ok_or(SecurityError::AuditChainInvalid)?;
        if last.audit_sequence.get() != tip_sequence {
            return Err(SecurityError::AuditChainInvalid);
        }
        Ok(AuditVerificationReport {
            records: self.records.len(),
            checkpoints: self.checkpoints.len(),
            tip: self.records.last().map(|record| record.record_hash),
            last_checkpoint: Some(last.audit_sequence),
        })
    }

    /// Exports the chain after an authorization check.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::AuditExportDenied`] when the caller is not
    /// authorized. Returns [`SecurityError::AuditChainInvalid`] when the chain
    /// fails verification.
    pub fn export(&self, authorized: bool) -> Result<AuditExport, SecurityError> {
        if !authorized {
            return Err(SecurityError::AuditExportDenied);
        }
        self.verify()?;
        Ok(AuditExport {
            shard_id: self.shard_id.clone(),
            records: self.records.clone(),
            checkpoints: self.checkpoints.clone(),
        })
    }
}

/// An exported audit chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditExport {
    /// The shard that owns the chain.
    pub shard_id: ShardId,
    /// Every exported record.
    pub records: Vec<AuditRecord>,
    /// Every exported checkpoint.
    pub checkpoints: Vec<AuditCheckpoint>,
}

impl AuditExport {
    /// Returns the number of exported records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns true when the export has no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

fn checkpoint_digest(
    shard_id: &ShardId,
    audit_sequence: AuditSequence,
    record_hash: Hash32,
    previous_checkpoint_hash: Hash32,
    key_id: &KeyIdentifier,
) -> Hash32 {
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, shard_id.as_bytes());
    push_u64(&mut bytes, audit_sequence.get());
    push_bytes(&mut bytes, record_hash.as_bytes());
    push_bytes(&mut bytes, previous_checkpoint_hash.as_bytes());
    push_bytes(&mut bytes, key_id.as_str().as_bytes());
    domain_hash(b"oxkv/audit-checkpoint/v1", &bytes)
}

fn checkpoint_hash(digest: Hash32, signature: &Signature) -> Hash32 {
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, digest.as_bytes());
    push_bytes(&mut bytes, signature.as_bytes());
    domain_hash(b"oxkv/audit-checkpoint-hash/v1", &bytes)
}

fn domain_hash(tag: &[u8], bytes: &[u8]) -> Hash32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tag);
    hasher.update(&[0]);
    hasher.update(bytes);
    Hash32::from_bytes(*hasher.finalize().as_bytes())
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    push_u64(output, u64::try_from(value.len()).unwrap_or(u64::MAX));
    output.extend_from_slice(value);
}
