//! Write enforcement and the atomic commit model.
//!
//! Enforcement runs at commit. It pins one policy version, evaluates each row
//! against the pinned fact projection, and records one audit record per write
//! request.
//!
//! The check order is authentication, cross-shard, version, and policy. A
//! stale base returns `Conflict` before policy runs. A batch verdict is the
//! conjunction of its row verdicts.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::{
    AuditReference, AuditSequence, IdempotencyKey, ReceiptEnvelope, ReceiptId, ReceiptOutcome,
    ReceiptState,
};
use oxkv_contracts::version::{
    CommitVersion, DigestDomain, Hash32, KeyVersion, ManifestDigest, MapVersion, PolicyVersion,
    ProjectionEpoch, ProjectionWatermarks, ShardId,
};

use crate::audit::{AuditRecord, AuditRecordBody, AuditSink, AuditVerdict};
use crate::auth::{AuthError, Authenticator, Principal};
use crate::codec::{push_bytes, push_tag};
use crate::policy::{Decision, Evaluation, PolicyEvaluator, ReasonCode, RowContext, RuleId};
use crate::projection::{AuthorizationMode, AuthorizationPolicy, FactProjection, StalenessReport};
use crate::request::{MAX_BATCH_ROWS, RowKey, WriteRequest};
use crate::state::{BusinessEvent, CommittedEffects, IndexEntry, ShardState, StoredRow};

/// The synthetic receipt reference before a terminal verdict.
const PENDING_SEQUENCE: AuditSequence = AuditSequence::new(0);

/// The zero hash that marks a pending audit reference.
const PENDING_HASH: Hash32 = Hash32::from_bytes([0; 32]);

/// A logical queue deadline. The authority injects the current tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueDeadline(u64);

impl QueueDeadline {
    /// Creates a deadline from a logical tick.
    #[must_use]
    pub const fn new(tick: u64) -> Self {
        Self(tick)
    }

    /// Returns the logical tick.
    #[must_use]
    pub const fn tick(self) -> u64 {
        self.0
    }
}

/// The policy version, manifest digest, and map version at commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PolicyCatalogEntry {
    /// The resolved policy version.
    pub policy_version: PolicyVersion,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The shard map version.
    pub map_version: MapVersion,
}

impl PolicyCatalogEntry {
    /// Creates one catalog entry.
    #[must_use]
    pub const fn new(
        policy_version: PolicyVersion,
        manifest_digest: ManifestDigest,
        map_version: MapVersion,
    ) -> Self {
        Self {
            policy_version,
            manifest_digest,
            map_version,
        }
    }
}

/// The pinned versions that one commit evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyPin {
    policy_version: PolicyVersion,
    manifest_digest: ManifestDigest,
    map_version: MapVersion,
    projection_epoch: ProjectionEpoch,
    fact_versions: ProjectionWatermarks,
}

impl PolicyPin {
    /// Pins the catalog entry against one fact projection.
    #[must_use]
    pub fn pin(catalog: PolicyCatalogEntry, projection: &FactProjection) -> Self {
        Self {
            policy_version: catalog.policy_version,
            manifest_digest: catalog.manifest_digest,
            map_version: catalog.map_version,
            projection_epoch: projection.epoch(),
            fact_versions: projection.watermarks().clone(),
        }
    }

    /// Returns the pinned policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the manifest digest.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.manifest_digest
    }

    /// Returns the map version.
    #[must_use]
    pub const fn map_version(&self) -> MapVersion {
        self.map_version
    }

    /// Returns the projection epoch.
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

/// The authority inputs to one commit.
#[derive(Debug, Clone, Copy)]
pub struct CommitContext<'a> {
    /// The current policy catalog entry.
    pub catalog: PolicyCatalogEntry,
    /// The pinned fact projection.
    pub projection: &'a FactProjection,
    /// The current fact versions per source.
    pub current_facts: &'a ProjectionWatermarks,
    /// The authorization mode and staleness bound.
    pub authorization: &'a AuthorizationPolicy,
    /// The queue deadline for a lagging projection.
    pub queue_deadline: QueueDeadline,
}

/// The outcome of one write request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The projection is lagging and the write waits for the deadline.
    ///
    /// The live receipt has a pending audit reference. The timeout or commit
    /// writes the terminal receipt and the one audit record.
    Queued {
        /// The live receipt.
        receipt: ReceiptEnvelope,
        /// The queue deadline.
        deadline: QueueDeadline,
    },
    /// The write committed.
    Committed {
        /// The terminal receipt.
        receipt: ReceiptEnvelope,
        /// The audit record.
        audit: AuditRecord,
        /// The committed effects.
        committed: CommittedEffects,
    },
    /// The authority rejected the write.
    Rejected {
        /// The terminal receipt.
        receipt: ReceiptEnvelope,
        /// The audit record.
        audit: AuditRecord,
    },
    /// The base version did not match.
    Conflict {
        /// The terminal receipt.
        receipt: ReceiptEnvelope,
        /// The audit record.
        audit: AuditRecord,
    },
}

impl WriteOutcome {
    /// Returns the receipt.
    #[must_use]
    pub const fn receipt(&self) -> &ReceiptEnvelope {
        match self {
            Self::Queued { receipt, .. }
            | Self::Committed { receipt, .. }
            | Self::Rejected { receipt, .. }
            | Self::Conflict { receipt, .. } => receipt,
        }
    }

    /// Returns the audit record. A queued write has no record yet.
    #[must_use]
    pub const fn audit(&self) -> Option<&AuditRecord> {
        match self {
            Self::Queued { .. } => None,
            Self::Committed { audit, .. }
            | Self::Rejected { audit, .. }
            | Self::Conflict { audit, .. } => Some(audit),
        }
    }

    /// Returns the receipt state.
    #[must_use]
    pub fn state(&self) -> ReceiptState {
        self.receipt().state()
    }

    /// Returns true when the receipt is terminal.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.receipt().is_terminal()
    }

    /// Returns the receipt and consumes the outcome.
    #[must_use]
    pub fn into_receipt(self) -> ReceiptEnvelope {
        match self {
            Self::Queued { receipt, .. }
            | Self::Committed { receipt, .. }
            | Self::Rejected { receipt, .. }
            | Self::Conflict { receipt, .. } => receipt,
        }
    }
}

/// A write enforcement failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnforcementError {
    /// The request is not authenticated.
    #[error("The request failed authentication.")]
    Authentication(#[source] AuthError),
    /// The idempotency key already names a different body.
    #[error("The idempotency key already names a different body.")]
    IdempotencyKeyReuse,
    /// The audit append failed.
    #[error("The audit append is unavailable.")]
    AuditUnavailable,
}

impl EnforcementError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Authentication(error) => error.code(),
            Self::IdempotencyKeyReuse => "write.idempotency_key_reuse",
            Self::AuditUnavailable => "audit.unavailable",
        }
    }
}

/// The pending part of one queued write.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingWrite {
    request: WriteRequest,
    principal: Principal,
    pin: PolicyPin,
    staleness: StalenessReport,
    receipt_id: ReceiptId,
}

/// The stored status of one write.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StoredStatus {
    Queued {
        deadline: QueueDeadline,
        pending: PendingWrite,
    },
    Committed {
        audit: AuditRecord,
        effects: CommittedEffects,
    },
    Rejected {
        audit: AuditRecord,
    },
    Conflict {
        audit: AuditRecord,
    },
}

/// One stored write result.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredWrite {
    input_digest: Hash32,
    receipt: ReceiptEnvelope,
    status: StoredStatus,
}

impl StoredWrite {
    /// Returns the outcome for a retry.
    fn outcome(&self) -> WriteOutcome {
        match &self.status {
            StoredStatus::Queued { deadline, .. } => WriteOutcome::Queued {
                receipt: self.receipt.clone(),
                deadline: *deadline,
            },
            StoredStatus::Committed { audit, effects } => WriteOutcome::Committed {
                receipt: self.receipt.clone(),
                audit: audit.clone(),
                committed: effects.clone(),
            },
            StoredStatus::Rejected { audit } => WriteOutcome::Rejected {
                receipt: self.receipt.clone(),
                audit: audit.clone(),
            },
            StoredStatus::Conflict { audit } => WriteOutcome::Conflict {
                receipt: self.receipt.clone(),
                audit: audit.clone(),
            },
        }
    }
}

/// The authority state for one shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enforcement {
    shard_id: ShardId,
    state: ShardState,
    writes: BTreeMap<IdempotencyKey, StoredWrite>,
}

impl Enforcement {
    /// Creates an empty enforcement authority for one shard.
    #[must_use]
    pub fn new(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            state: ShardState::new(),
            writes: BTreeMap::new(),
        }
    }

    /// Returns the shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the shard state.
    #[must_use]
    pub const fn state(&self) -> &ShardState {
        &self.state
    }

    /// Returns one stored receipt.
    #[must_use]
    pub fn receipt(&self, idempotency_key: &IdempotencyKey) -> Option<&ReceiptEnvelope> {
        self.writes
            .get(idempotency_key)
            .map(|stored| &stored.receipt)
    }

    /// Returns the number of stored receipts.
    #[must_use]
    pub fn receipt_count(&self) -> usize {
        self.writes.len()
    }

    /// Returns the number of queued writes.
    #[must_use]
    pub fn queued_count(&self) -> usize {
        self.writes
            .values()
            .filter(|stored| matches!(stored.status, StoredStatus::Queued { .. }))
            .count()
    }

    /// Commits or rejects one write request.
    ///
    /// # Errors
    ///
    /// Returns [`EnforcementError::Authentication`] when authentication fails.
    /// Returns [`EnforcementError::IdempotencyKeyReuse`] when the key names a
    /// different body. Returns [`EnforcementError::AuditUnavailable`] when the
    /// audit append fails. A failed audit append changes no state.
    #[allow(clippy::too_many_lines)]
    pub fn commit(
        &mut self,
        request: &WriteRequest,
        authenticator: &dyn Authenticator,
        evaluator: &dyn PolicyEvaluator,
        context: &CommitContext<'_>,
        audit: &mut dyn AuditSink,
    ) -> Result<WriteOutcome, EnforcementError> {
        let principal = authenticator
            .authenticate(request)
            .map_err(EnforcementError::Authentication)?;
        let input_digest = request.input_digest();

        if let Some(stored) = self.writes.get(request.idempotency_key()) {
            if stored.input_digest == input_digest {
                return Ok(stored.outcome());
            }
            return Err(EnforcementError::IdempotencyKeyReuse);
        }

        let shard_id = self.shard_id.clone();
        let pin = PolicyPin::pin(context.catalog, context.projection);
        let receipt_id = receipt_id_for(request);

        if request.rows().len() > MAX_BATCH_ROWS {
            let decisions = request.rows().first().map_or_else(Vec::new, |row| {
                vec![Decision::reject(
                    row.key.clone(),
                    RuleId::batch(),
                    ReasonCode::BATCH_TOO_LARGE,
                )]
            });
            return self.finish_terminal(
                AuditDraft {
                    shard_id: &shard_id,
                    request,
                    principal: &principal,
                    pin: &pin,
                    input_digest,
                    receipt_id,
                    verdict: AuditVerdict::Rejected,
                    reason: ReasonCode::BATCH_TOO_LARGE,
                    staleness: None,
                    decisions,
                    conflict_version: None,
                },
                audit,
            );
        }

        if let Some(row) = request
            .rows()
            .iter()
            .find(|row| row.shard_id != self.shard_id)
        {
            let decisions = vec![Decision::reject(
                row.key.clone(),
                RuleId::shard(),
                ReasonCode::CROSS_MUTATION,
            )];
            return self.finish_terminal(
                AuditDraft {
                    shard_id: &shard_id,
                    request,
                    principal: &principal,
                    pin: &pin,
                    input_digest,
                    receipt_id,
                    verdict: AuditVerdict::Rejected,
                    reason: ReasonCode::CROSS_MUTATION,
                    staleness: None,
                    decisions,
                    conflict_version: None,
                },
                audit,
            );
        }

        let Some(base_version) = request.base_version() else {
            let decisions = request.rows().first().map_or_else(Vec::new, |row| {
                vec![Decision::reject(
                    row.key.clone(),
                    RuleId::version(),
                    ReasonCode::MISSING_BASE_VERSION,
                )]
            });
            return self.finish_terminal(
                AuditDraft {
                    shard_id: &shard_id,
                    request,
                    principal: &principal,
                    pin: &pin,
                    input_digest,
                    receipt_id,
                    verdict: AuditVerdict::Rejected,
                    reason: ReasonCode::MISSING_BASE_VERSION,
                    staleness: None,
                    decisions,
                    conflict_version: None,
                },
                audit,
            );
        };

        if let Some(row) = request
            .rows()
            .iter()
            .find(|row| base_version > self.current_key_version(&row.key))
        {
            let decisions = vec![Decision::reject(
                row.key.clone(),
                RuleId::version(),
                ReasonCode::STALE_BASE_VERSION,
            )];
            return self.finish_terminal(
                AuditDraft {
                    shard_id: &shard_id,
                    request,
                    principal: &principal,
                    pin: &pin,
                    input_digest,
                    receipt_id,
                    verdict: AuditVerdict::Rejected,
                    reason: ReasonCode::STALE_BASE_VERSION,
                    staleness: None,
                    decisions,
                    conflict_version: None,
                },
                audit,
            );
        }

        if let Some(row) = request
            .rows()
            .iter()
            .find(|row| self.current_key_version(&row.key) != base_version)
        {
            let current_key_version = self.current_key_version(&row.key);
            let decisions = vec![Decision::reject(
                row.key.clone(),
                RuleId::version(),
                ReasonCode::VERSION_CONFLICT,
            )];
            return self.finish_terminal(
                AuditDraft {
                    shard_id: &shard_id,
                    request,
                    principal: &principal,
                    pin: &pin,
                    input_digest,
                    receipt_id,
                    verdict: AuditVerdict::Conflict,
                    reason: ReasonCode::VERSION_CONFLICT,
                    staleness: None,
                    decisions,
                    conflict_version: Some(current_key_version),
                },
                audit,
            );
        }

        let staleness = StalenessReport::evaluate(
            context.projection,
            context.current_facts,
            context.authorization.bound(),
        );

        if context.projection.verify().is_err() {
            let decisions = rejection_decisions(
                request,
                RuleId::authorization(),
                ReasonCode::PROJECTION_CHECKSUM_FAILED,
            );
            return self.finish_terminal(
                AuditDraft {
                    shard_id: &shard_id,
                    request,
                    principal: &principal,
                    pin: &pin,
                    input_digest,
                    receipt_id,
                    verdict: AuditVerdict::Rejected,
                    reason: ReasonCode::PROJECTION_CHECKSUM_FAILED,
                    staleness: None,
                    decisions,
                    conflict_version: None,
                },
                audit,
            );
        }

        if staleness.regression() {
            let decisions = rejection_decisions(
                request,
                RuleId::authorization(),
                ReasonCode::PROJECTION_WATERMARK_REGRESSION,
            );
            return self.finish_terminal(
                AuditDraft {
                    shard_id: &shard_id,
                    request,
                    principal: &principal,
                    pin: &pin,
                    input_digest,
                    receipt_id,
                    verdict: AuditVerdict::Rejected,
                    reason: ReasonCode::PROJECTION_WATERMARK_REGRESSION,
                    staleness: Some(staleness),
                    decisions,
                    conflict_version: None,
                },
                audit,
            );
        }

        if staleness.over_bound() {
            if context.authorization.mode() == AuthorizationMode::Strict {
                let decisions = rejection_decisions(
                    request,
                    RuleId::authorization(),
                    ReasonCode::AUTHORIZATION_STALE_FACTS,
                );
                return self.finish_terminal(
                    AuditDraft {
                        shard_id: &shard_id,
                        request,
                        principal: &principal,
                        pin: &pin,
                        input_digest,
                        receipt_id,
                        verdict: AuditVerdict::Rejected,
                        reason: ReasonCode::AUTHORIZATION_STALE_FACTS,
                        staleness: Some(staleness),
                        decisions,
                        conflict_version: None,
                    },
                    audit,
                );
            }
            return Ok(self.enqueue(QueuedDraft {
                request,
                principal,
                pin,
                input_digest,
                receipt_id,
                staleness,
                deadline: context.queue_deadline,
            }));
        }

        let mut decisions = Vec::with_capacity(request.rows().len());
        for row in request.rows() {
            let evaluation = {
                let row_context = RowContext {
                    before: self.state.row(&row.key),
                    after: row,
                    projection: context.projection,
                };
                evaluator.evaluate(&row_context)
            };
            match evaluation {
                Evaluation::Allow { rule } => {
                    decisions.push(Decision::allow(row.key.clone(), rule));
                }
                Evaluation::Reject { rule, reason } => {
                    decisions.push(Decision::reject(row.key.clone(), rule, reason));
                    return self.finish_terminal(
                        AuditDraft {
                            shard_id: &shard_id,
                            request,
                            principal: &principal,
                            pin: &pin,
                            input_digest,
                            receipt_id,
                            verdict: AuditVerdict::Rejected,
                            reason,
                            staleness: Some(staleness),
                            decisions,
                            conflict_version: None,
                        },
                        audit,
                    );
                }
                Evaluation::BudgetExhausted { rule } => {
                    let reason = ReasonCode::BUDGET_EXHAUSTED;
                    decisions.push(Decision::reject(row.key.clone(), rule, reason));
                    return self.finish_terminal(
                        AuditDraft {
                            shard_id: &shard_id,
                            request,
                            principal: &principal,
                            pin: &pin,
                            input_digest,
                            receipt_id,
                            verdict: AuditVerdict::Rejected,
                            reason,
                            staleness: Some(staleness),
                            decisions,
                            conflict_version: None,
                        },
                        audit,
                    );
                }
            }
        }

        self.finish_commit(
            AuditDraft {
                shard_id: &shard_id,
                request,
                principal: &principal,
                pin: &pin,
                input_digest,
                receipt_id,
                verdict: AuditVerdict::Committed,
                reason: ReasonCode::ALLOWED,
                staleness: Some(staleness),
                decisions,
                conflict_version: None,
            },
            audit,
        )
    }

    /// Expires every queued write at or past the given tick.
    ///
    /// # Errors
    ///
    /// Returns [`EnforcementError::AuditUnavailable`] when an audit append
    /// fails. The failed write stays queued.
    pub fn expire_due(
        &mut self,
        now: QueueDeadline,
        audit: &mut dyn AuditSink,
    ) -> Result<Vec<WriteOutcome>, EnforcementError> {
        let due: Vec<IdempotencyKey> = self
            .writes
            .iter()
            .filter_map(|(key, stored)| match &stored.status {
                StoredStatus::Queued { deadline, .. } if *deadline <= now => Some(key.clone()),
                _ => None,
            })
            .collect();

        let mut outcomes = Vec::with_capacity(due.len());
        for key in due {
            outcomes.push(self.expire_one(&key, audit)?);
        }
        Ok(outcomes)
    }

    /// Returns the current key version. An absent row is at version zero.
    fn current_key_version(&self, key: &RowKey) -> KeyVersion {
        self.state
            .row(key)
            .map_or(KeyVersion::new(0), StoredRow::key_version)
    }

    /// Stores a queued write and returns its live receipt.
    fn enqueue(&mut self, draft: QueuedDraft<'_>) -> WriteOutcome {
        let receipt = ReceiptEnvelope::queued(
            draft.receipt_id,
            draft.request.request_id(),
            draft.request.idempotency_key().clone(),
            pending_audit_reference(&self.shard_id),
            draft.pin.policy_version(),
            draft.pin.manifest_digest(),
            ReasonCode::PROJECTION_LAGGING.as_str(),
        );
        let pending = PendingWrite {
            request: draft.request.clone(),
            principal: draft.principal,
            pin: draft.pin,
            staleness: draft.staleness,
            receipt_id: draft.receipt_id,
        };
        self.writes.insert(
            draft.request.idempotency_key().clone(),
            StoredWrite {
                input_digest: draft.input_digest,
                receipt: receipt.clone(),
                status: StoredStatus::Queued {
                    deadline: draft.deadline,
                    pending,
                },
            },
        );
        WriteOutcome::Queued {
            receipt,
            deadline: draft.deadline,
        }
    }

    /// Expires one queued write.
    fn expire_one(
        &mut self,
        key: &IdempotencyKey,
        audit: &mut dyn AuditSink,
    ) -> Result<WriteOutcome, EnforcementError> {
        let (input_digest, request, principal, pin, staleness, receipt_id) = {
            let Some(stored) = self.writes.get(key) else {
                return Err(EnforcementError::AuditUnavailable);
            };
            let StoredStatus::Queued { pending, .. } = &stored.status else {
                return Err(EnforcementError::AuditUnavailable);
            };
            (
                stored.input_digest,
                pending.request.clone(),
                pending.principal.clone(),
                pending.pin.clone(),
                pending.staleness.clone(),
                pending.receipt_id,
            )
        };
        let shard_id = self.shard_id.clone();

        let decisions =
            rejection_decisions(&request, RuleId::authorization(), ReasonCode::QUEUE_TIMEOUT);
        let draft = AuditDraft {
            shard_id: &shard_id,
            request: &request,
            principal: &principal,
            pin: &pin,
            input_digest,
            receipt_id,
            verdict: AuditVerdict::Rejected,
            reason: ReasonCode::QUEUE_TIMEOUT,
            staleness: Some(staleness),
            decisions,
            conflict_version: None,
        };
        let record = append_audit(&draft, audit)?;
        let receipt = terminal_receipt(
            &draft,
            &record,
            ReceiptOutcome::Rejected {
                reason: ReasonCode::QUEUE_TIMEOUT.as_str(),
            },
        );

        if let Some(stored) = self.writes.get_mut(key) {
            stored.receipt = receipt.clone();
            stored.status = StoredStatus::Rejected {
                audit: record.clone(),
            };
        }
        Ok(WriteOutcome::Rejected {
            receipt,
            audit: record,
        })
    }

    /// Appends one rejection or conflict record, then writes its receipt.
    #[allow(clippy::needless_pass_by_value)]
    fn finish_terminal(
        &mut self,
        draft: AuditDraft<'_>,
        audit: &mut dyn AuditSink,
    ) -> Result<WriteOutcome, EnforcementError> {
        let record = append_audit(&draft, audit)?;
        let (outcome, status) = match draft.verdict {
            AuditVerdict::Rejected => {
                let receipt = terminal_receipt(
                    &draft,
                    &record,
                    ReceiptOutcome::Rejected {
                        reason: draft.reason.as_str(),
                    },
                );
                (
                    WriteOutcome::Rejected {
                        receipt: receipt.clone(),
                        audit: record.clone(),
                    },
                    StoredStatus::Rejected {
                        audit: record.clone(),
                    },
                )
            }
            AuditVerdict::Conflict => {
                let current_key_version = draft.conflict_version.unwrap_or(KeyVersion::new(0));
                let receipt = terminal_receipt(
                    &draft,
                    &record,
                    ReceiptOutcome::Conflict {
                        current_key_version,
                        reason: draft.reason.as_str(),
                    },
                );
                (
                    WriteOutcome::Conflict {
                        receipt: receipt.clone(),
                        audit: record.clone(),
                    },
                    StoredStatus::Conflict {
                        audit: record.clone(),
                    },
                )
            }
            AuditVerdict::Committed => return Err(EnforcementError::AuditUnavailable),
        };
        self.writes.insert(
            draft.request.idempotency_key().clone(),
            StoredWrite {
                input_digest: draft.input_digest,
                receipt: outcome.receipt().clone(),
                status,
            },
        );
        Ok(outcome)
    }

    /// Appends one committed record, then applies the whole transaction.
    #[allow(clippy::needless_pass_by_value)]
    fn finish_commit(
        &mut self,
        draft: AuditDraft<'_>,
        audit: &mut dyn AuditSink,
    ) -> Result<WriteOutcome, EnforcementError> {
        let commit_version =
            CommitVersion::new(self.state.commit_version().get().saturating_add(1));
        let key_version = KeyVersion::new(commit_version.get());
        let mut rows = Vec::with_capacity(draft.request.rows().len());
        let mut index = Vec::with_capacity(draft.request.rows().len());
        let mut events = Vec::with_capacity(draft.request.rows().len());
        for row in draft.request.rows() {
            let value_digest = Hash32::of(&row.value);
            rows.push((
                row.key.clone(),
                StoredRow::new(row.value.clone(), key_version),
            ));
            index.push(IndexEntry {
                row_key: row.key.clone(),
                value_digest,
            });
            events.push(BusinessEvent {
                key: row.key.clone(),
                value_digest,
                commit_version,
            });
        }

        let record = append_audit(&draft, audit)?;
        let receipt = terminal_receipt(
            &draft,
            &record,
            ReceiptOutcome::Committed { commit_version },
        );
        self.state.apply(commit_version, &rows, &index, &events);
        let effects = CommittedEffects {
            commit_version,
            rows: rows.iter().map(|(key, _)| key.clone()).collect(),
            index: index.clone(),
            events: events.clone(),
        };
        self.writes.insert(
            draft.request.idempotency_key().clone(),
            StoredWrite {
                input_digest: draft.input_digest,
                receipt: receipt.clone(),
                status: StoredStatus::Committed {
                    audit: record.clone(),
                    effects: effects.clone(),
                },
            },
        );
        Ok(WriteOutcome::Committed {
            receipt,
            audit: record,
            committed: effects,
        })
    }
}

/// The inputs for one queued write.
struct QueuedDraft<'a> {
    request: &'a WriteRequest,
    principal: Principal,
    pin: PolicyPin,
    input_digest: Hash32,
    receipt_id: ReceiptId,
    staleness: StalenessReport,
    deadline: QueueDeadline,
}

/// The inputs for one audit record.
struct AuditDraft<'a> {
    shard_id: &'a ShardId,
    request: &'a WriteRequest,
    principal: &'a Principal,
    pin: &'a PolicyPin,
    input_digest: Hash32,
    receipt_id: ReceiptId,
    verdict: AuditVerdict,
    reason: ReasonCode,
    staleness: Option<StalenessReport>,
    decisions: Vec<Decision>,
    conflict_version: Option<KeyVersion>,
}

/// Builds one reject decision for every row.
#[allow(clippy::needless_pass_by_value)]
fn rejection_decisions(request: &WriteRequest, rule: RuleId, reason: ReasonCode) -> Vec<Decision> {
    request
        .rows()
        .iter()
        .map(|row| Decision::reject(row.key.clone(), rule.clone(), reason))
        .collect()
}

/// Appends one audit record.
fn append_audit(
    draft: &AuditDraft<'_>,
    audit: &mut dyn AuditSink,
) -> Result<AuditRecord, EnforcementError> {
    let body = AuditRecordBody {
        request_id: draft.request.request_id(),
        idempotency_key: draft.request.idempotency_key().clone(),
        principal: draft.principal.clone(),
        shard_id: draft.shard_id.clone(),
        base_version: draft.request.base_version(),
        policy_version: draft.pin.policy_version(),
        manifest_digest: draft.pin.manifest_digest(),
        map_version: draft.pin.map_version(),
        projection_epoch: draft.pin.projection_epoch(),
        fact_versions: draft.pin.fact_versions().clone(),
        receipt_id: draft.receipt_id,
        verdict: draft.verdict,
        reason: draft.reason,
        input_digest: draft.input_digest,
        staleness: draft.staleness.clone(),
        decisions: draft.decisions.clone(),
    };
    audit
        .append(body)
        .map_err(|_error| EnforcementError::AuditUnavailable)
}

/// Builds one terminal receipt with its audit reference.
fn terminal_receipt(
    draft: &AuditDraft<'_>,
    record: &AuditRecord,
    outcome: ReceiptOutcome,
) -> ReceiptEnvelope {
    let mut receipt = ReceiptEnvelope::queued(
        draft.receipt_id,
        draft.request.request_id(),
        draft.request.idempotency_key().clone(),
        record.audit_reference(),
        draft.pin.policy_version(),
        draft.pin.manifest_digest(),
        draft.reason.as_str(),
    );
    receipt
        .close(outcome)
        .expect("a queued receipt closes exactly once");
    receipt
}

/// Returns the pending audit reference of a queued receipt.
fn pending_audit_reference(shard_id: &ShardId) -> AuditReference {
    AuditReference::new(shard_id.clone(), PENDING_SEQUENCE, PENDING_HASH)
}

/// Computes the deterministic receipt identity.
fn receipt_id_for(request: &WriteRequest) -> ReceiptId {
    let mut buffer = Vec::new();
    push_tag(&mut buffer, b"oxkv/enforce-receipt/v1");
    push_bytes(&mut buffer, request.request_id().as_bytes());
    push_bytes(&mut buffer, request.idempotency_key().as_bytes());
    ReceiptId::from_bytes(*Hash32::of_domain(DigestDomain::ReceiptId, &buffer).as_bytes())
}
