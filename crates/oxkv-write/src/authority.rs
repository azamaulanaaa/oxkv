//! The write authority for one shard.
//!
//! The authority owns one shard. It serializes writes and assigns every key
//! version. The check order is:
//!
//! 1. receipt lookup for the client and idempotency key;
//! 2. cross-shard check;
//! 3. base-version check;
//! 4. policy check;
//! 5. commit.
//!
//! Authentication is outside this crate. The request carries the client
//! identity as an explicit value. The receipt id covers the client identity,
//! the idempotency key, and the request body digest. The receipt ledger keys
//! every receipt by the client identity and the idempotency key. The commit
//! path has no clock, randomness, or I/O.
//!
//! A production authority takes a real [`CheckpointSigner`]. Production mode
//! rejects [`StructuralSigner`]. A local authority uses the structural signer
//! and the in-memory witness queue.
//!
//! A native authority can save its state to a directory and load it again
//! after a restart. The state covers the rows, the receipts, the commit
//! version, the audit records, the signed checkpoints, and the chain tip.

use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::Arc;

use oxkv_contracts::receipt::{
    AuditReference, IdempotencyKey, ReceiptEnvelope, ReceiptId, ReceiptOutcome, ReceiptState,
};
use oxkv_contracts::version::{
    CommitVersion, DigestDomain, Hash32, KeyVersion, ManifestDigest, MapVersion, PolicyVersion,
    ProjectionEpoch, ProjectionWatermarks, ShardId,
};
use oxkv_security::audit::{
    AuditChain, AuditCheckpoint, AuditDecision, AuditEntry, AuditRowDecision,
    AuditVerificationReport, CheckpointSigner, GENESIS_HASH, SignerKind, StructuralSigner,
};
use oxkv_security::auth::PrincipalId;
use oxkv_security::error::SecurityError;
use oxkv_security::secret::KeyIdentifier;
use oxkv_security::witness::{WitnessQueue, WitnessSink};

use crate::audit::AuditRecord;
use crate::conflict::ConflictDetail;
use crate::hook::{CommitHook, HookEffect};
use crate::policy::{
    DecisionVerdict, Policy, PolicyDecision, PolicyRow, PolicyVerdict, RuleIdentity,
};
use crate::receipt::{ReceiptLedger, ReceiptLedgerError};
use crate::record::{CommitSummary, CommitView, KeyChange, KeyRecord, KeyValue};
use crate::request::{ClientId, Key, Mutation, WriteOperation, WriteRequest};
#[cfg(not(target_arch = "wasm32"))]
use crate::state::{AuthorityState, AuthorityStateError};
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
    /// The audit append failed.
    #[error("The audit append failed.")]
    AuditUnavailable,
    /// The audit chain rejected the record or the checkpoint.
    #[error("The audit chain is invalid.")]
    AuditChainInvalid,
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
            Self::AuditUnavailable => "audit.unavailable",
            Self::AuditChainInvalid => "audit.chain_invalid",
            Self::Receipt { source } => source.code(),
        }
    }
}

/// The mode of one authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AuthorityMode {
    /// A production authority. It requires a real checkpoint signer.
    Production,
    /// A local authority. It uses the structural signer.
    Local,
}

/// An error from authority startup.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityInitError {
    /// Production mode received a structural signer.
    #[error("Production mode requires a real checkpoint signer.")]
    StructuralSignerInProduction,
    /// The signer key identifier differs from the configured checkpoint key.
    #[error("The signer key {signer} does not match the configured key {configured}.")]
    CheckpointKeyMismatch {
        /// The signer key identifier.
        signer: String,
        /// The configured checkpoint key identifier.
        configured: String,
    },
}

impl AuthorityInitError {
    /// Returns the canonical error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::StructuralSignerInProduction => "authority.structural_signer_in_production",
            Self::CheckpointKeyMismatch { .. } => "authority.checkpoint_key_mismatch",
        }
    }
}

/// The configuration of one authority.
///
/// Every value is explicit. The configuration supplies no default value. The
/// configuration pins the provenance that the audit record carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityConfig {
    shard_id: ShardId,
    policy_version: PolicyVersion,
    manifest_digest: ManifestDigest,
    map_version: MapVersion,
    projection_epoch: ProjectionEpoch,
    fact_versions: ProjectionWatermarks,
    checkpoint_key: KeyIdentifier,
    mode: AuthorityMode,
}

impl AuthorityConfig {
    /// Creates a local authority configuration.
    ///
    /// The checkpoint key names the key that signs each audit checkpoint. The
    /// value is a key identifier. It is not key material. A local authority
    /// uses the structural signer.
    #[must_use]
    pub fn new(
        shard_id: ShardId,
        policy_version: PolicyVersion,
        manifest_digest: ManifestDigest,
        map_version: MapVersion,
        projection_epoch: ProjectionEpoch,
        fact_versions: ProjectionWatermarks,
        checkpoint_key: KeyIdentifier,
    ) -> Self {
        Self::with_mode(
            shard_id,
            policy_version,
            manifest_digest,
            map_version,
            projection_epoch,
            fact_versions,
            checkpoint_key,
            AuthorityMode::Local,
        )
    }

    /// Creates a production authority configuration.
    ///
    /// A production authority must receive a real checkpoint signer. Startup
    /// fails when the signer is structural.
    #[must_use]
    pub fn production(
        shard_id: ShardId,
        policy_version: PolicyVersion,
        manifest_digest: ManifestDigest,
        map_version: MapVersion,
        projection_epoch: ProjectionEpoch,
        fact_versions: ProjectionWatermarks,
        checkpoint_key: KeyIdentifier,
    ) -> Self {
        Self::with_mode(
            shard_id,
            policy_version,
            manifest_digest,
            map_version,
            projection_epoch,
            fact_versions,
            checkpoint_key,
            AuthorityMode::Production,
        )
    }

    /// Creates an authority configuration with an explicit mode.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_mode(
        shard_id: ShardId,
        policy_version: PolicyVersion,
        manifest_digest: ManifestDigest,
        map_version: MapVersion,
        projection_epoch: ProjectionEpoch,
        fact_versions: ProjectionWatermarks,
        checkpoint_key: KeyIdentifier,
        mode: AuthorityMode,
    ) -> Self {
        Self {
            shard_id,
            policy_version,
            manifest_digest,
            map_version,
            projection_epoch,
            fact_versions,
            checkpoint_key,
            mode,
        }
    }

    /// Returns the authority mode.
    #[must_use]
    pub const fn mode(&self) -> AuthorityMode {
        self.mode
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

    /// Returns the key identifier that signs each audit checkpoint.
    #[must_use]
    pub const fn checkpoint_key(&self) -> &KeyIdentifier {
        &self.checkpoint_key
    }
}

/// The write authority for one shard.
///
/// The witness type defaults to the in-memory [`WitnessQueue`]. A native
/// deployment supplies a durable outbox instead. A native authority can also
/// save its state to a directory and load it again after a restart.
#[derive(Debug, Clone)]
pub struct Authority<W = WitnessQueue> {
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
    signer: Arc<dyn CheckpointSigner>,
    witness: W,
}

impl Authority<WitnessQueue> {
    /// Creates a local authority for one shard.
    ///
    /// The authority uses the structural signer and the in-memory witness
    /// queue. Use [`Authority::open`] to supply a durable outbox.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityInitError::StructuralSignerInProduction`] when the
    /// configuration selects production mode.
    pub fn local(config: AuthorityConfig) -> Result<Self, AuthorityInitError> {
        let signer = Arc::new(StructuralSigner::new(config.checkpoint_key.clone()));
        Self::open(config, signer, WitnessQueue::new())
    }
}

impl<W> Authority<W> {
    /// Creates an authority from a configuration, a checkpoint signer, and a
    /// witness outbox.
    ///
    /// Production mode requires a real checkpoint signer. Startup fails when
    /// production mode receives a structural signer. The signer key identifier
    /// must match the configured checkpoint key.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityInitError::StructuralSignerInProduction`] when the
    /// configuration selects production mode and the signer is structural.
    /// Returns [`AuthorityInitError::CheckpointKeyMismatch`] when the signer
    /// key identifier differs from the configured key.
    pub fn open(
        config: AuthorityConfig,
        signer: Arc<dyn CheckpointSigner>,
        witness: W,
    ) -> Result<Self, AuthorityInitError> {
        if config.mode == AuthorityMode::Production && signer.kind() == SignerKind::Structural {
            return Err(AuthorityInitError::StructuralSignerInProduction);
        }
        if signer.key_id() != &config.checkpoint_key {
            return Err(AuthorityInitError::CheckpointKeyMismatch {
                signer: signer.key_id().as_str().to_owned(),
                configured: config.checkpoint_key.as_str().to_owned(),
            });
        }
        let audit = AuditChain::new(config.shard_id.clone());
        Ok(Self {
            shard_id: config.shard_id,
            policy_version: config.policy_version,
            manifest_digest: config.manifest_digest,
            map_version: config.map_version,
            projection_epoch: config.projection_epoch,
            fact_versions: config.fact_versions,
            records: BTreeMap::new(),
            receipts: ReceiptLedger::new(),
            commit_version: CommitVersion::new(0),
            audit,
            signer,
            witness,
        })
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

    /// Checks the audit chain to the last signed checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::AuditChainInvalid`] when a record hash, a
    /// previous hash, a sequence, or a checkpoint does not match.
    pub fn verify_audit_chain(&self) -> Result<AuditVerificationReport, SecurityError> {
        self.audit.verify()
    }

    /// Returns the signed checkpoints in chain order.
    #[must_use]
    pub fn audit_checkpoints(&self) -> &[AuditCheckpoint] {
        self.audit.checkpoints()
    }

    /// Returns the last signed checkpoint.
    #[must_use]
    pub fn last_audit_checkpoint(&self) -> Option<&AuditCheckpoint> {
        self.audit.last_checkpoint()
    }

    /// Returns the asynchronous witness outbox.
    #[must_use]
    pub const fn witness(&self) -> &W {
        &self.witness
    }

    /// Returns the asynchronous witness outbox for delivery.
    #[must_use]
    pub const fn witness_mut(&mut self) -> &mut W {
        &mut self.witness
    }

    /// Returns one receipt envelope.
    #[must_use]
    pub fn receipt(
        &self,
        client_id: &ClientId,
        idempotency_key: &IdempotencyKey,
    ) -> Option<&ReceiptEnvelope> {
        self.receipts.receipt(client_id, idempotency_key)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<W> Authority<W> {
    /// Saves the authority state to a directory.
    ///
    /// The method writes one state file in the directory. The file holds the
    /// rows and their key versions, the receipts, the commit version, the
    /// audit records, the signed checkpoints, and the chain tip. The method
    /// writes a temporary file and renames it over the state file.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityStateError::Io`] when the directory or the file is
    /// not writable.
    pub fn save_state(&self, directory: impl AsRef<Path>) -> Result<(), AuthorityStateError> {
        let state = AuthorityState {
            shard_id: self.shard_id.clone(),
            records: self.records.clone(),
            receipts: self.receipts.clone(),
            commit_version: self.commit_version,
            audit_records: self.audit.records().to_vec(),
            checkpoints: self.audit.checkpoints().to_vec(),
            tip: self
                .audit
                .records()
                .last()
                .map_or(GENESIS_HASH, |record| record.record_hash),
        };
        state.save(directory.as_ref())
    }

    /// Loads the authority state from a directory.
    ///
    /// The method replaces the rows, the receipts, the commit version, the
    /// audit records, and the signed checkpoints. The method keeps the
    /// configuration and the witness outbox. A failed load changes no state.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityStateError::MissingState`] when the state file is
    /// absent. Returns [`AuthorityStateError::Truncated`] when the file is
    /// short. Returns [`AuthorityStateError::IntegrityMismatch`] when the
    /// integrity value does not match. Returns
    /// [`AuthorityStateError::UnsupportedVersion`] when the format version is
    /// unknown. Returns [`AuthorityStateError::UnknownFormat`] when the magic
    /// value is unknown. Returns [`AuthorityStateError::Corrupt`] when the
    /// payload or the audit chain is invalid.
    pub fn load_state(&mut self, directory: impl AsRef<Path>) -> Result<(), AuthorityStateError> {
        let state = AuthorityState::load(directory.as_ref())?;
        if state.shard_id != self.shard_id {
            return Err(AuthorityStateError::Corrupt {
                reason: "the saved shard does not match the open shard",
            });
        }
        let audit = AuditChain::from_parts(
            self.shard_id.clone(),
            state.audit_records,
            state.checkpoints,
        );
        let report = audit.verify().map_err(|_| AuthorityStateError::Corrupt {
            reason: "the saved audit chain does not verify",
        })?;
        let expected_tip = report.tip.unwrap_or(GENESIS_HASH);
        if state.tip != expected_tip {
            return Err(AuthorityStateError::Corrupt {
                reason: "the saved chain tip does not match the audit chain",
            });
        }
        self.records = state.records;
        self.receipts = state.receipts;
        self.commit_version = state.commit_version;
        self.audit = audit;
        Ok(())
    }
}

impl<W: WitnessSink> Authority<W> {
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
    /// advance. Returns [`SubmitError::AuditUnavailable`] when the audit append
    /// fails. An audit failure writes no receipt and changes no state. Returns
    /// [`SubmitError::Receipt`] when the ledger fails.
    pub fn submit(
        &mut self,
        request: &WriteRequest,
        policy: &dyn Policy,
        hooks: &[&dyn CommitHook],
    ) -> Result<SubmitOutcome, SubmitError> {
        let body_digest = request.body_digest();

        if let Some(stored) = self
            .receipts
            .get(request.client_id(), request.idempotency_key())
        {
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
            let (receipt, checkpoint) = self.finish_receipt(
                request,
                body_digest,
                ReceiptOutcome::Rejected { reason },
                rule,
                &decisions,
            )?;
            self.witness.enqueue(checkpoint);
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

        let (receipt, checkpoint) = self.finish_receipt(
            request,
            body_digest,
            ReceiptOutcome::Committed {
                commit_version: next_commit_version,
            },
            rule,
            &decisions,
        )?;
        self.apply_changes(&changes);
        self.commit_version = next_commit_version;
        self.witness.enqueue(checkpoint);
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
        let (receipt, checkpoint) = self.finish_receipt(
            request,
            body_digest,
            ReceiptOutcome::Rejected {
                reason: REASON_CROSS_SHARD,
            },
            rule,
            &decisions,
        )?;
        self.witness.enqueue(checkpoint);
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
        let (receipt, checkpoint) = self.finish_receipt(
            request,
            body_digest,
            ReceiptOutcome::Rejected {
                reason: REASON_STALE_BASE,
            },
            rule,
            &decisions,
        )?;
        self.witness.enqueue(checkpoint);
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
        let (receipt, checkpoint) = self.finish_receipt(
            request,
            body_digest,
            ReceiptOutcome::Conflict {
                current_key_version,
                reason: REASON_VERSION_CONFLICT,
            },
            rule,
            &decisions,
        )?;
        self.witness.enqueue(checkpoint);
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

    /// Builds and stores one audit record, its signed checkpoint, and its
    /// terminal receipt.
    ///
    /// The function appends the audit record before it writes the receipt. An
    /// audit append failure returns an error and writes no receipt.
    fn finish_receipt(
        &mut self,
        request: &WriteRequest,
        body_digest: Hash32,
        outcome: ReceiptOutcome,
        rule_identity: RuleIdentity,
        decisions: &[PolicyDecision],
    ) -> Result<(ReceiptEnvelope, AuditCheckpoint), SubmitError> {
        let receipt_id = receipt_id_for(request, body_digest);
        let verdict = match outcome.state() {
            ReceiptState::Committed => AuditDecision::Committed,
            ReceiptState::Rejected => AuditDecision::Rejected,
            ReceiptState::Conflict => AuditDecision::Conflict,
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
        let base_version = request
            .mutations()
            .first()
            .and_then(Mutation::base_version)
            .unwrap_or(KeyVersion::new(0));
        let entry = AuditEntry {
            request_id: request.request_id(),
            idempotency_key: request.idempotency_key().clone(),
            principal: PrincipalId::from_bytes(request.client_id().as_bytes().to_vec()),
            shard_id: self.shard_id.clone(),
            base_version,
            base_versions: request
                .mutations()
                .iter()
                .map(|mutation| (mutation.key().as_bytes().to_vec(), mutation.base_version()))
                .collect(),
            policy_version: self.policy_version,
            manifest_digest: self.manifest_digest,
            map_version: self.map_version,
            projection_epoch: self.projection_epoch,
            fact_versions: self
                .fact_versions
                .iter()
                .map(|(source, version)| (source.clone(), version))
                .collect(),
            rule_identity,
            decisions: decisions
                .iter()
                .map(|decision| AuditRowDecision {
                    key: decision.key().as_bytes().to_vec(),
                    rule_identity: decision.rule().clone(),
                    allowed: decision.verdict() == DecisionVerdict::Allow,
                    reason_code: decision.reason(),
                })
                .collect(),
            decision: verdict,
            reason_code: reason,
            receipt_id: Some(receipt_id),
            input_digest: body_digest,
        };
        let record = self.audit.append(entry).map_err(|error| match error {
            SecurityError::AuditUnavailable => SubmitError::AuditUnavailable,
            _ => SubmitError::AuditChainInvalid,
        })?;
        let checkpoint = self
            .audit
            .checkpoint(self.signer.as_ref())
            .map_err(|_| SubmitError::AuditChainInvalid)?;
        let mut receipt = ReceiptEnvelope::queued(
            receipt_id,
            request.request_id(),
            request.idempotency_key().clone(),
            AuditReference::new(
                self.shard_id.clone(),
                record.audit_sequence,
                record.record_hash,
            ),
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
            .insert_terminal(request.client_id(), body_digest, receipt.clone())
            .map_err(|source| SubmitError::Receipt { source })?;
        Ok((receipt, checkpoint))
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
