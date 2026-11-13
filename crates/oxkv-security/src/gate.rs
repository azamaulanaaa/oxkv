//! The authentication and authorization gate.
//!
//! The gate authenticates a credential and a request signature before it calls
//! the authorizer. A write verdict appends one audit record and one
//! checkpoint. The checkpoint enters the witness queue after the commit. A
//! read verdict enters the access log only.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::receipt::{
    AuditReference, IdempotencyKey, ReceiptEnvelope, ReceiptId, ReceiptOutcome, RequestId,
};
use oxkv_contracts::version::{
    CommitVersion, DigestDomain, FactSourceId, FactVersion, Hash32, KeyVersion, ManifestDigest,
    MapVersion, PolicyVersion, ProjectionEpoch, ScopeIdentity, ShardId,
};

use crate::access::{AccessDecision, AccessEvent, AccessLog, AccessReference};
use crate::audit::{
    AuditChain, AuditCheckpoint, AuditDecision, AuditEntry, CheckpointSigner, RuleIdentity,
};
use crate::auth::{CredentialProof, CredentialVerifier, LogicalTime, PrincipalId, Subject};
use crate::error::SecurityError;
use crate::signature::{Nonce, SignatureVerifier, SignedRequest};
use crate::witness::WitnessSink;

/// The pinned inputs of one write enforcement verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcementInput {
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
    /// The commit version of an allowed write.
    pub commit_version: CommitVersion,
}

/// The authorization view of one write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteAccess<'a> {
    /// The target shard.
    pub shard_id: &'a ShardId,
    /// The base key version.
    pub base_version: KeyVersion,
    /// The digest of the idempotent body.
    pub body_digest: Hash32,
}

/// The authorization view of one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadAccess<'a> {
    /// The target shard.
    pub shard_id: &'a ShardId,
    /// The requested key.
    pub key: &'a [u8],
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
}

/// A policy authorizer.
pub trait Authorizer {
    /// Returns true when the subject may write.
    fn allow_write(&self, subject: &Subject, access: &WriteAccess<'_>) -> bool;

    /// Returns true when the subject may read.
    fn allow_read(&self, subject: &Subject, access: &ReadAccess<'_>) -> bool;
}

/// An authorizer that allows every request.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAllAuthorizer;

impl Authorizer for AllowAllAuthorizer {
    fn allow_write(&self, _subject: &Subject, _access: &WriteAccess<'_>) -> bool {
        true
    }

    fn allow_read(&self, _subject: &Subject, _access: &ReadAccess<'_>) -> bool {
        true
    }
}

/// An authorizer that denies every request.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyAllAuthorizer;

impl Authorizer for DenyAllAuthorizer {
    fn allow_write(&self, _subject: &Subject, _access: &WriteAccess<'_>) -> bool {
        false
    }

    fn allow_read(&self, _subject: &Subject, _access: &ReadAccess<'_>) -> bool {
        false
    }
}

/// One read scope request.
#[derive(Clone, PartialEq, Eq)]
pub struct ReadRequest {
    /// The request identity.
    pub request_id: RequestId,
    /// The target shard.
    pub shard_id: ShardId,
    /// The requested key.
    pub key: Vec<u8>,
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
}

impl ReadRequest {
    /// Creates a read request.
    #[must_use]
    pub fn new(
        request_id: RequestId,
        shard_id: ShardId,
        key: Vec<u8>,
        scope_identity: ScopeIdentity,
    ) -> Self {
        Self {
            request_id,
            shard_id,
            key,
            scope_identity,
        }
    }

    /// Returns the digest of the requested key.
    #[must_use]
    pub fn key_digest(&self) -> Hash32 {
        Hash32::of(&self.key)
    }
}

impl fmt::Debug for ReadRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadRequest")
            .field("request_id", &self.request_id)
            .field("shard_id", &self.shard_id)
            .field("key_digest", &self.key_digest())
            .field("scope_identity", &self.scope_identity)
            .finish_non_exhaustive()
    }
}

/// The result of one write enforcement verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    receipt: ReceiptEnvelope,
    checkpoint: AuditCheckpoint,
}

impl WriteOutcome {
    /// Returns the receipt state.
    #[must_use]
    pub const fn state(&self) -> oxkv_contracts::receipt::ReceiptState {
        self.receipt.state()
    }

    /// Returns the receipt.
    #[must_use]
    pub const fn receipt(&self) -> &ReceiptEnvelope {
        &self.receipt
    }

    /// Returns the audit reference.
    #[must_use]
    pub const fn audit_reference(&self) -> &AuditReference {
        self.receipt.audit()
    }

    /// Returns the checkpoint that covers the audit record.
    #[must_use]
    pub const fn checkpoint(&self) -> &AuditCheckpoint {
        &self.checkpoint
    }
}

/// The result of one read scope evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOutcome {
    decision: AccessDecision,
    reason_code: Option<&'static str>,
    access: AccessReference,
}

impl ReadOutcome {
    /// Returns the read decision.
    #[must_use]
    pub const fn decision(&self) -> AccessDecision {
        self.decision
    }

    /// Returns true when the read is allowed.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self.decision, AccessDecision::Allowed)
    }

    /// Returns the coarse reason code. An allowed read has no reason code.
    #[must_use]
    pub const fn reason_code(&self) -> Option<&'static str> {
        self.reason_code
    }

    /// Returns the access log reference.
    #[must_use]
    pub const fn access(&self) -> AccessReference {
        self.access
    }
}

#[derive(Debug, Clone)]
struct StoredOutcome {
    body_digest: Hash32,
    outcome: WriteOutcome,
}

/// The security gate for one shard.
#[derive(Debug)]
pub struct SecurityGate<V, A, S> {
    credentials: V,
    authorizer: A,
    signatures: S,
    nonces: BTreeSet<Nonce>,
    idempotency: BTreeMap<(PrincipalId, IdempotencyKey), StoredOutcome>,
    audit: AuditChain,
    access: AccessLog,
}

impl<V, A, S> SecurityGate<V, A, S>
where
    V: CredentialVerifier,
    A: Authorizer,
    S: SignatureVerifier,
{
    /// Creates a gate for one shard.
    #[must_use]
    pub fn new(credentials: V, authorizer: A, signatures: S, shard_id: ShardId) -> Self {
        Self {
            credentials,
            authorizer,
            signatures,
            nonces: BTreeSet::new(),
            idempotency: BTreeMap::new(),
            audit: AuditChain::new(shard_id),
            access: AccessLog::new(),
        }
    }

    /// Submits one signed write request.
    ///
    /// The method authenticates the credential and the signature before it
    /// calls the authorizer. A retry with the same key and body returns the
    /// original receipt. The method queues the witness checkpoint after the
    /// commit. The method does not wait for delivery.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::Unauthenticated`] for an unknown credential, an
    /// expired signature, or a signature mismatch. Returns
    /// [`SecurityError::StaleCredentials`] after the revocation bound. Returns
    /// [`SecurityError::ReplayedRequest`] for a used nonce with different
    /// content. Returns [`SecurityError::IdempotencyKeyReuse`] for the same key
    /// with a different body and a new nonce. Returns
    /// [`SecurityError::AuditUnavailable`] when the audit append fails. That
    /// error leaves no receipt and changes no state.
    pub fn submit_write(
        &mut self,
        proof: &CredentialProof,
        request: &SignedRequest,
        input: &EnforcementInput,
        signer: &dyn CheckpointSigner,
        witness: &mut dyn WitnessSink,
        now: LogicalTime,
    ) -> Result<WriteOutcome, SecurityError> {
        let subject = self.credentials.authenticate(proof, now)?;
        let body_digest = request.body_digest();
        let key = (
            subject.principal().clone(),
            request.idempotency_key().clone(),
        );
        if let Some(outcome) = self.retry_outcome(&key, body_digest, request.nonce())? {
            return Ok(outcome);
        }
        self.signatures.verify_request(request, now)?;
        if self.nonces.contains(&request.nonce()) {
            return Err(SecurityError::ReplayedRequest);
        }
        let allowed = self.authorizer.allow_write(
            &subject,
            &WriteAccess {
                shard_id: request.shard_id(),
                base_version: request.base_version(),
                body_digest,
            },
        );
        let decision = if allowed {
            AuditDecision::Committed
        } else {
            AuditDecision::Rejected
        };
        let reason_code = if allowed {
            None
        } else {
            Some(SecurityError::Forbidden.code())
        };
        let receipt_id =
            make_receipt_id(subject.principal(), request.idempotency_key(), body_digest);
        let entry = make_audit_entry(&subject, request, input, decision, reason_code, receipt_id);
        let record = match self.audit.append(entry) {
            Ok(record) => record,
            Err(SecurityError::AuditUnavailable) => {
                return Err(SecurityError::AuditUnavailable);
            }
            Err(_) => return Err(SecurityError::AuditChainInvalid),
        };
        let checkpoint = self
            .audit
            .checkpoint(signer)
            .map_err(|_| SecurityError::AuditChainInvalid)?;
        let audit_reference = AuditReference::new(
            self.audit.shard_id().clone(),
            record.audit_sequence,
            record.record_hash,
        );
        let receipt = make_receipt(request, input, audit_reference, decision, receipt_id)?;
        let outcome = WriteOutcome {
            receipt,
            checkpoint: checkpoint.clone(),
        };
        self.idempotency.insert(
            key,
            StoredOutcome {
                body_digest,
                outcome: outcome.clone(),
            },
        );
        self.nonces.insert(request.nonce());
        witness.enqueue(checkpoint);
        Ok(outcome)
    }

    fn retry_outcome(
        &self,
        key: &(PrincipalId, IdempotencyKey),
        body_digest: Hash32,
        nonce: Nonce,
    ) -> Result<Option<WriteOutcome>, SecurityError> {
        let Some(stored) = self.idempotency.get(key) else {
            return Ok(None);
        };
        if stored.body_digest == body_digest {
            return Ok(Some(stored.outcome.clone()));
        }
        if self.nonces.contains(&nonce) {
            return Err(SecurityError::ReplayedRequest);
        }
        Err(SecurityError::IdempotencyKeyReuse)
    }

    /// Evaluates one read scope request.
    ///
    /// The method writes one entry to the access log. It does not write to the
    /// audit chain.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::Unauthenticated`] for an unknown credential.
    /// Returns [`SecurityError::StaleCredentials`] after the revocation bound.
    pub fn evaluate_read(
        &mut self,
        proof: &CredentialProof,
        request: &ReadRequest,
        now: LogicalTime,
    ) -> Result<ReadOutcome, SecurityError> {
        let subject = self.credentials.authenticate(proof, now)?;
        let allowed = self.authorizer.allow_read(
            &subject,
            &ReadAccess {
                shard_id: &request.shard_id,
                key: &request.key,
                scope_identity: request.scope_identity,
            },
        );
        let decision = if allowed {
            AccessDecision::Allowed
        } else {
            AccessDecision::Denied
        };
        let reason_code = if allowed {
            None
        } else {
            Some(SecurityError::Forbidden.code())
        };
        let entry = self.access.append(AccessEvent {
            request_id: request.request_id,
            principal: subject.principal().clone(),
            shard_id: request.shard_id.clone(),
            key_digest: request.key_digest(),
            scope_identity: request.scope_identity,
            decision,
            reason_code,
            observed_at: now,
        });
        Ok(ReadOutcome {
            decision,
            reason_code,
            access: AccessReference {
                access_sequence: entry.access_sequence,
            },
        })
    }

    /// Returns the write audit chain.
    #[must_use]
    pub const fn audit(&self) -> &AuditChain {
        &self.audit
    }

    /// Returns the read access log.
    #[must_use]
    pub const fn access_log(&self) -> &AccessLog {
        &self.access
    }

    /// Enables or disables the audit append path.
    pub fn set_audit_available(&mut self, available: bool) {
        self.audit.set_append_available(available);
    }

    /// Returns the number of stored receipts.
    #[must_use]
    pub fn receipt_count(&self) -> usize {
        self.idempotency.len()
    }

    /// Returns the number of used nonces.
    #[must_use]
    pub fn nonce_count(&self) -> usize {
        self.nonces.len()
    }

    /// Returns the gate shard.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        self.audit.shard_id()
    }
}

fn make_audit_entry(
    subject: &Subject,
    request: &SignedRequest,
    input: &EnforcementInput,
    decision: AuditDecision,
    reason_code: Option<&'static str>,
    receipt_id: ReceiptId,
) -> AuditEntry {
    AuditEntry {
        request_id: request.request_id(),
        idempotency_key: request.idempotency_key().clone(),
        principal: subject.principal().clone(),
        shard_id: request.shard_id().clone(),
        base_version: request.base_version(),
        base_versions: Vec::new(),
        policy_version: input.policy_version,
        manifest_digest: input.manifest_digest,
        map_version: input.map_version,
        projection_epoch: input.projection_epoch,
        fact_versions: input.fact_versions.clone(),
        rule_identity: input.rule_identity.clone(),
        decisions: Vec::new(),
        decision,
        reason_code,
        receipt_id: Some(receipt_id),
        input_digest: request.input_digest(),
    }
}

fn make_receipt(
    request: &SignedRequest,
    input: &EnforcementInput,
    audit_reference: AuditReference,
    decision: AuditDecision,
    receipt_id: ReceiptId,
) -> Result<ReceiptEnvelope, SecurityError> {
    let mut receipt = ReceiptEnvelope::queued(
        receipt_id,
        request.request_id(),
        request.idempotency_key().clone(),
        audit_reference,
        input.policy_version,
        input.manifest_digest,
        "projection.lagging",
    )
    .map_err(|_error| SecurityError::InvariantViolation)?;
    let closed = if decision == AuditDecision::Committed {
        receipt.close(ReceiptOutcome::Committed {
            commit_version: input.commit_version,
        })
    } else {
        receipt.close(ReceiptOutcome::Rejected {
            reason: SecurityError::Forbidden.code(),
        })
    };
    closed.map_err(|_| SecurityError::InvariantViolation)?;
    Ok(receipt)
}

fn make_receipt_id(
    principal: &PrincipalId,
    idempotency_key: &IdempotencyKey,
    body_digest: Hash32,
) -> ReceiptId {
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, principal.as_bytes());
    push_bytes(&mut bytes, idempotency_key.as_bytes());
    push_bytes(&mut bytes, body_digest.as_bytes());
    let hash = Hash32::of_domain(DigestDomain::ReceiptId, &bytes);
    ReceiptId::from_bytes(*hash.as_bytes())
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    output.extend_from_slice(value);
}
