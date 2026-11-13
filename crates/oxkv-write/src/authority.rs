//! The in-memory authority commit simulator.
//!
//! The authority owns one shard. It serializes writes and assigns every key
//! version. The check order is:
//!
//! 1. receipt lookup for the idempotency key;
//! 2. cross-shard check;
//! 3. base-version check;
//! 4. policy check;
//! 5. commit.
//!
//! Authentication is outside this crate. The request carries the client
//! identity as an explicit value. The receipt id covers the client identity,
//! the idempotency key, and the request body digest. The simulator has no
//! clock, randomness, or I/O.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::{
    IdempotencyKey, ReceiptEnvelope, ReceiptId, ReceiptOutcome, ReceiptState,
};
use oxkv_contracts::version::{
    CommitVersion, DigestDomain, Hash32, KeyVersion, ManifestDigest, MapVersion, PolicyVersion,
    ProjectionEpoch, ProjectionWatermarks, ShardId,
};

use crate::audit::{AuditChain, AuditRecord, AuditRecordBody, AuditVerdict};
use crate::conflict::ConflictDetail;
use crate::hook::{CommitHook, HookEffect};
use crate::policy::{Policy, PolicyDecision, PolicyRow, PolicyVerdict, RuleIdentity};
use crate::receipt::{ReceiptLedger, ReceiptLedgerError};
use crate::record::{CommitSummary, CommitView, KeyChange, KeyRecord, KeyValue};
use crate::request::{Key, WriteOperation, WriteRequest};
use crate::util::push_bytes;

const REASON_CROSS_SHARD: &str = "shard.cross_mutation_rejected";
const REASON_STALE_BASE: &str = "write.stale_base_version";
const REASON_VERSION_CONFLICT: &str = "write.version_conflict";
const REASON_QUEUED: &str = "projection.lagging";
const RULE_CROSS_SHARD: &str = "write.cross_shard_check";
const RULE_STALE_BASE: &str = "write.stale_base_check";
const RULE_VERSION: &str = "write.version_check";

/// The result of one authority submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitOutcome {
    receipt: ReceiptEnvelope,
    duplicate: bool,
    commit: Option<CommitSummary>,
    conflict: Option<ConflictDetail>,
    effects: Vec<HookEffect>,
}

impl SubmitOutcome {
    /// Creates a duplicate outcome.
    fn duplicate(receipt: ReceiptEnvelope) -> Self {
        Self {
            receipt,
            duplicate: true,
            commit: None,
            conflict: None,
            effects: Vec::new(),
        }
    }

    /// Creates a committed outcome.
    fn committed(
        receipt: ReceiptEnvelope,
        commit: CommitSummary,
        effects: Vec<HookEffect>,
    ) -> Self {
        Self {
            receipt,
            duplicate: false,
            commit: Some(commit),
            conflict: None,
            effects,
        }
    }

    /// Creates a rejected outcome.
    fn rejected(receipt: ReceiptEnvelope) -> Self {
        Self {
            receipt,
            duplicate: false,
            commit: None,
            conflict: None,
            effects: Vec::new(),
        }
    }

    /// Creates a conflict outcome.
    fn conflict_outcome(receipt: ReceiptEnvelope, conflict: ConflictDetail) -> Self {
        Self {
            receipt,
            duplicate: false,
            commit: None,
            conflict: Some(conflict),
            effects: Vec::new(),
        }
    }

    /// Returns the terminal receipt.
    #[must_use]
    pub const fn receipt(&self) -> &ReceiptEnvelope {
        &self.receipt
    }

    /// Returns the receipt state.
    #[must_use]
    pub const fn state(&self) -> ReceiptState {
        self.receipt.state()
    }

    /// Returns true when the authority returned an existing receipt.
    #[must_use]
    pub const fn is_duplicate(&self) -> bool {
        self.duplicate
    }

    /// Returns the commit summary of a fresh commit.
    #[must_use]
    pub const fn commit(&self) -> Option<&CommitSummary> {
        self.commit.as_ref()
    }

    /// Returns the conflict detail of a conflict.
    #[must_use]
    pub const fn conflict(&self) -> Option<&ConflictDetail> {
        self.conflict.as_ref()
    }

    /// Returns the effects that the commit hooks derived.
    #[must_use]
    pub fn effects(&self) -> &[HookEffect] {
        &self.effects
    }

    /// Consumes the outcome and returns the receipt.
    #[must_use]
    pub fn into_receipt(self) -> ReceiptEnvelope {
        self.receipt
    }
}

/// An error from an authority submission.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    /// A mutation has no base version.
    #[error("The mutation for the key {key} has no base version.")]
    MissingBaseVersion {
        /// The mutation key.
        key: Key,
    },
    /// The idempotency key is already used with another body.
    #[error("The idempotency key is already used with a different body.")]
    IdempotencyKeyReuse {
        /// The reused key.
        idempotency_key: IdempotencyKey,
    },
    /// A commit hook failed.
    #[error("The commit hook {hook} failed with {code}.")]
    HookFailed {
        /// The hook name.
        hook: &'static str,
        /// The hook error code.
        code: &'static str,
    },
    /// A key version or commit version cannot advance.
    #[error("The version sequence is exhausted.")]
    VersionExhausted,
    /// The receipt ledger failed.
    #[error("{source}")]
    Receipt {
        /// The ledger error.
        #[from]
        source: ReceiptLedgerError,
    },
}

impl SubmitError {
    /// Returns the canonical error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingBaseVersion { .. } => "write.missing_base_version",
            Self::IdempotencyKeyReuse { .. } => "write.idempotency_key_reuse",
            Self::HookFailed { code, .. } => code,
            Self::VersionExhausted => "internal.resource_exhausted",
            Self::Receipt { source } => source.code(),
        }
    }
}

/// The configuration of one authority.
///
/// Every value is explicit. The simulator supplies no default value. The
/// configuration pins the provenance that the audit record carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityConfig {
    shard_id: ShardId,
    policy_version: PolicyVersion,
    manifest_digest: ManifestDigest,
    map_version: MapVersion,
    projection_epoch: ProjectionEpoch,
    fact_versions: ProjectionWatermarks,
}

impl AuthorityConfig {
    /// Creates an authority configuration.
    #[must_use]
    pub fn new(
        shard_id: ShardId,
        policy_version: PolicyVersion,
        manifest_digest: ManifestDigest,
        map_version: MapVersion,
        projection_epoch: ProjectionEpoch,
        fact_versions: ProjectionWatermarks,
    ) -> Self {
        Self {
            shard_id,
            policy_version,
            manifest_digest,
            map_version,
            projection_epoch,
            fact_versions,
        }
    }

    /// Returns the shard identity.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the pinned policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the pinned manifest digest.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.manifest_digest
    }

    /// Returns the pinned shard map version.
    #[must_use]
    pub const fn map_version(&self) -> MapVersion {
        self.map_version
    }

    /// Returns the pinned projection epoch.
    #[must_use]
    pub const fn projection_epoch(&self) -> ProjectionEpoch {
        self.projection_epoch
    }

    /// Returns the pinned fact versions.
    #[must_use]
    pub const fn fact_versions(&self) -> &ProjectionWatermarks {
        &self.fact_versions
    }
}

/// The in-memory authority for one shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    shard_id: ShardId,
    policy_version: PolicyVersion,
    manifest_digest: ManifestDigest,
    map_version: MapVersion,
    projection_epoch: ProjectionEpoch,
    fact_versions: ProjectionWatermarks,
    records: BTreeMap<Key, KeyRecord>,
    receipts: ReceiptLedger,
    commit_version: CommitVersion,
    audit: AuditChain,
}

impl Authority {
    /// Creates an authority for one shard.
    #[must_use]
    pub fn new(config: AuthorityConfig) -> Self {
        Self {
            shard_id: config.shard_id,
            policy_version: config.policy_version,
            manifest_digest: config.manifest_digest,
            map_version: config.map_version,
            projection_epoch: config.projection_epoch,
            fact_versions: config.fact_versions,
            records: BTreeMap::new(),
            receipts: ReceiptLedger::new(),
            commit_version: CommitVersion::new(0),
            audit: AuditChain::new(),
        }
    }

    /// Returns the shard identity.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the pinned policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the pinned manifest digest.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.manifest_digest
    }

    /// Returns the pinned shard map version.
    #[must_use]
    pub const fn map_version(&self) -> MapVersion {
        self.map_version
    }

    /// Returns the pinned projection epoch.
    #[must_use]
    pub const fn projection_epoch(&self) -> ProjectionEpoch {
        self.projection_epoch
    }

    /// Returns the pinned fact versions.
    #[must_use]
    pub const fn fact_versions(&self) -> &ProjectionWatermarks {
        &self.fact_versions
    }

    /// Returns the current shard commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the current key version. An absent key has version zero.
    #[must_use]
    pub fn key_version(&self, key: &Key) -> KeyVersion {
        self.records
            .get(key)
            .map_or(KeyVersion::new(0), KeyRecord::key_version)
    }

    /// Returns one key record.
    #[must_use]
    pub fn record(&self, key: &Key) -> Option<&KeyRecord> {
        self.records.get(key)
    }

    /// Returns the receipt ledger.
    #[must_use]
    pub const fn receipts(&self) -> &ReceiptLedger {
        &self.receipts
    }

    /// Returns the audit records in chain order.
    #[must_use]
    pub fn audit_records(&self) -> &[AuditRecord] {
        self.audit.records()
    }

    /// Returns the number of audit records.
    #[must_use]
    pub fn audit_record_count(&self) -> usize {
        self.audit.len()
    }

    /// Checks the audit chain.
    #[must_use]
    pub fn verify_audit_chain(&self) -> bool {
        self.audit.verify()
    }

    /// Returns one receipt envelope.
    #[must_use]
    pub fn receipt(&self, idempotency_key: &IdempotencyKey) -> Option<&ReceiptEnvelope> {
        self.receipts.receipt(idempotency_key)
    }

    /// Submits one write request.
    ///
    /// A duplicate request returns the existing receipt. A reused key with a
    /// different body returns [`SubmitError::IdempotencyKeyReuse`].
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError::MissingBaseVersion`] when a mutation has no base
    /// version. Returns [`SubmitError::IdempotencyKeyReuse`] when the body
    /// differs. Returns [`SubmitError::HookFailed`] when a commit hook fails.
    /// Returns [`SubmitError::VersionExhausted`] when a sequence cannot
    /// advance. Returns [`SubmitError::Receipt`] when the ledger fails.
    pub fn submit(
        &mut self,
        request: &WriteRequest,
        policy: &dyn Policy,
        hooks: &[&dyn CommitHook],
    ) -> Result<SubmitOutcome, SubmitError> {
        let body_digest = request.body_digest();

        if let Some(stored) = self.receipts.get(request.idempotency_key()) {
            if stored.body_digest() != body_digest {
                return Err(SubmitError::IdempotencyKeyReuse {
                    idempotency_key: request.idempotency_key().clone(),
                });
            }
            return Ok(SubmitOutcome::duplicate(stored.envelope().clone()));
        }

        if let Some(key) = request.cross_shard_key(&self.shard_id) {
            return self.reject_cross_shard(request, body_digest, key);
        }

        if let Some(key) = request.first_missing_base() {
            return Err(SubmitError::MissingBaseVersion { key: key.clone() });
        }

        match self.check_versions(request) {
            VersionCheck::Pass => {}
            VersionCheck::Stale { key } => {
                return self.reject_stale_base(request, body_digest, key);
            }
            VersionCheck::Conflict {
                key,
                base_version,
                current_key_version,
            } => {
                return self.conflict_version(
                    request,
                    body_digest,
                    key,
                    base_version,
                    current_key_version,
                );
            }
        }

        let rows = self.policy_rows(request);
        let evaluation = policy.evaluate(request, &rows);
        let rule = evaluation.rule_identity().clone();
        let decisions = evaluation.decisions().to_vec();
        if let PolicyVerdict::Reject { reason } = evaluation.verdict() {
            let receipt = self.finish_receipt(
                request,
                body_digest,
                ReceiptOutcome::Rejected { reason },
                rule,
                decisions,
            )?;
            return Ok(SubmitOutcome::rejected(receipt));
        }

        let next_commit_version = self.next_commit_version()?;
        let changes = self.plan_changes(request)?;
        let view = CommitView::new(&self.shard_id, next_commit_version, &changes);
        let mut effects = Vec::new();
        for hook in hooks {
            match hook.derive(&view) {
                Ok(mut derived) => effects.append(&mut derived),
                Err(error) => {
                    return Err(SubmitError::HookFailed {
                        hook: hook.name(),
                        code: error.code(),
                    });
                }
            }
        }

        let receipt = self.finish_receipt(
            request,
            body_digest,
            ReceiptOutcome::Committed {
                commit_version: next_commit_version,
            },
            rule,
            decisions,
        )?;
        self.apply_changes(&changes);
        self.commit_version = next_commit_version;
        Ok(SubmitOutcome::committed(
            receipt,
            CommitSummary::new(self.shard_id.clone(), next_commit_version, changes),
            effects,
        ))
    }

    /// Builds a rejected outcome for a cross-shard mutation.
    fn reject_cross_shard(
        &mut self,
        request: &WriteRequest,
        body_digest: Hash32,
        key: &Key,
    ) -> Result<SubmitOutcome, SubmitError> {
        let rule = RuleIdentity::new(RULE_CROSS_SHARD);
        let decisions = vec![PolicyDecision::reject(
            key.clone(),
            rule.clone(),
            REASON_CROSS_SHARD,
        )];
        let receipt = self.finish_receipt(
            request,
            body_digest,
            ReceiptOutcome::Rejected {
                reason: REASON_CROSS_SHARD,
            },
            rule,
            decisions,
        )?;
        Ok(SubmitOutcome::rejected(receipt))
    }

    /// Builds a rejected outcome for a stale base version.
    fn reject_stale_base(
        &mut self,
        request: &WriteRequest,
        body_digest: Hash32,
        key: Key,
    ) -> Result<SubmitOutcome, SubmitError> {
        let rule = RuleIdentity::new(RULE_STALE_BASE);
        let decisions = vec![PolicyDecision::reject(key, rule.clone(), REASON_STALE_BASE)];
        let receipt = self.finish_receipt(
            request,
            body_digest,
            ReceiptOutcome::Rejected {
                reason: REASON_STALE_BASE,
            },
            rule,
            decisions,
        )?;
        Ok(SubmitOutcome::rejected(receipt))
    }

    /// Builds a conflict outcome for a base version mismatch.
    fn conflict_version(
        &mut self,
        request: &WriteRequest,
        body_digest: Hash32,
        key: Key,
        base_version: KeyVersion,
        current_key_version: KeyVersion,
    ) -> Result<SubmitOutcome, SubmitError> {
        let rule = RuleIdentity::new(RULE_VERSION);
        let decisions = vec![PolicyDecision::reject(
            key.clone(),
            rule.clone(),
            REASON_VERSION_CONFLICT,
        )];
        let receipt = self.finish_receipt(
            request,
            body_digest,
            ReceiptOutcome::Conflict {
                current_key_version,
                reason: REASON_VERSION_CONFLICT,
            },
            rule,
            decisions,
        )?;
        Ok(SubmitOutcome::conflict_outcome(
            receipt,
            ConflictDetail::new(key, base_version, current_key_version),
        ))
    }

    /// Builds the policy rows for one request.
    fn policy_rows<'a>(&'a self, request: &'a WriteRequest) -> Vec<PolicyRow<'a>> {
        request
            .mutations()
            .iter()
            .map(|mutation| PolicyRow::new(mutation, self.records.get(mutation.key())))
            .collect()
    }

    /// Checks the base version of every mutation.
    fn check_versions(&self, request: &WriteRequest) -> VersionCheck {
        for mutation in request.mutations() {
            let Some(base_version) = mutation.base_version() else {
                continue;
            };
            let current_key_version = self.key_version(mutation.key());
            if base_version > current_key_version {
                return VersionCheck::Stale {
                    key: mutation.key().clone(),
                };
            }
            if base_version < current_key_version {
                return VersionCheck::Conflict {
                    key: mutation.key().clone(),
                    base_version,
                    current_key_version,
                };
            }
        }
        VersionCheck::Pass
    }

    /// Returns the next shard commit version.
    fn next_commit_version(&self) -> Result<CommitVersion, SubmitError> {
        self.commit_version
            .get()
            .checked_add(1)
            .map(CommitVersion::new)
            .ok_or(SubmitError::VersionExhausted)
    }

    /// Builds the key changes for one request.
    fn plan_changes(&self, request: &WriteRequest) -> Result<Vec<KeyChange>, SubmitError> {
        let mut changes = Vec::with_capacity(request.mutations().len());
        for mutation in request.mutations() {
            let Some(base_version) = mutation.base_version() else {
                return Err(SubmitError::MissingBaseVersion {
                    key: mutation.key().clone(),
                });
            };
            let current_key_version = self.key_version(mutation.key());
            let key_version = current_key_version
                .get()
                .checked_add(1)
                .map(KeyVersion::new)
                .ok_or(SubmitError::VersionExhausted)?;
            let previous = self.records.get(mutation.key());
            changes.push(KeyChange::new(
                mutation.key().clone(),
                base_version,
                key_version,
                current_key_version,
                previous.and_then(|record| record.as_value().map(<[u8]>::to_vec)),
                mutation.operation().clone(),
            ));
        }
        Ok(changes)
    }

    /// Applies the planned changes to the key records.
    fn apply_changes(&mut self, changes: &[KeyChange]) {
        for change in changes {
            let value = match change.operation() {
                WriteOperation::Put(value) => KeyValue::Value(value.clone()),
                WriteOperation::Delete => KeyValue::Tombstone,
            };
            self.records.insert(
                change.key().clone(),
                KeyRecord::new(change.key_version(), value),
            );
        }
    }

    /// Builds and stores one audit record and its terminal receipt.
    fn finish_receipt(
        &mut self,
        request: &WriteRequest,
        body_digest: Hash32,
        outcome: ReceiptOutcome,
        rule_identity: RuleIdentity,
        decisions: Vec<PolicyDecision>,
    ) -> Result<ReceiptEnvelope, SubmitError> {
        let receipt_id = receipt_id_for(request, body_digest);
        let verdict = match outcome.state() {
            ReceiptState::Committed => AuditVerdict::Committed,
            ReceiptState::Rejected => AuditVerdict::Rejected,
            ReceiptState::Conflict => AuditVerdict::Conflict,
            ReceiptState::Queued => {
                return Err(SubmitError::Receipt {
                    source: ReceiptLedgerError::NotTerminal {
                        state: ReceiptState::Queued,
                    },
                });
            }
        };
        let reason = match outcome {
            ReceiptOutcome::Committed { .. } => None,
            ReceiptOutcome::Rejected { reason } | ReceiptOutcome::Conflict { reason, .. } => {
                Some(reason)
            }
        };
        let body = AuditRecordBody {
            request_id: request.request_id(),
            idempotency_key: request.idempotency_key().clone(),
            principal: request.client_id().clone(),
            shard_id: self.shard_id.clone(),
            base_versions: request
                .mutations()
                .iter()
                .map(|mutation| (mutation.key().clone(), mutation.base_version()))
                .collect(),
            policy_version: self.policy_version,
            manifest_digest: self.manifest_digest,
            map_version: self.map_version,
            projection_epoch: self.projection_epoch,
            fact_versions: self.fact_versions.clone(),
            rule_identity,
            decisions,
            receipt_id,
            verdict,
            reason,
            input_digest: body_digest,
        };
        let record = self.audit.seal_next(body);
        let mut receipt = ReceiptEnvelope::queued(
            receipt_id,
            request.request_id(),
            request.idempotency_key().clone(),
            record.audit_reference(),
            self.policy_version,
            self.manifest_digest,
            REASON_QUEUED,
        )
        .expect("the queued reason code is registered");
        receipt
            .close(outcome)
            .map_err(|source| SubmitError::Receipt {
                source: ReceiptLedgerError::from(source),
            })?;
        self.receipts
            .insert_terminal(body_digest, receipt.clone())
            .map_err(|source| SubmitError::Receipt { source })?;
        self.audit.push(record);
        Ok(receipt)
    }
}

/// The result of the base-version check.
enum VersionCheck {
    /// Every base matches the current key version.
    Pass,
    /// A base is ahead of the current key version.
    Stale {
        /// The stale key.
        key: Key,
    },
    /// A base is behind the current key version.
    Conflict {
        /// The conflicting key.
        key: Key,
        /// The named base version.
        base_version: KeyVersion,
        /// The current key version.
        current_key_version: KeyVersion,
    },
}

/// Returns the deterministic receipt identity for one request.
///
/// The receipt id covers the client identity, the idempotency key, and the
/// request body digest.
fn receipt_id_for(request: &WriteRequest, body_digest: Hash32) -> ReceiptId {
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, request.client_id().as_bytes());
    push_bytes(&mut bytes, request.idempotency_key().as_bytes());
    push_bytes(&mut bytes, body_digest.as_bytes());
    ReceiptId::from_bytes(*Hash32::of_domain(DigestDomain::ReceiptId, &bytes).as_bytes())
}
