#![allow(missing_docs)]
#![allow(dead_code)]

//! Shared scenarios for the enforcement tests and golden vectors.

use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
use oxkv_contracts::version::{
    FactSourceId, FactVersion, Hash32, KeyVersion, ManifestDigest, MapVersion, PolicyVersion,
    ProjectionEpoch, ProjectionWatermarks, ShardId,
};
use oxkv_enforce::{
    AuditError, AuditRecordBody, AuditSink, AuthorizationPolicy, CommitContext, Enforcement,
    FactProjection, InMemoryAuditChain, PolicyCatalogEntry, PolicyEvaluator, ProjectionWriteOrigin,
    QueueDeadline, ReasonCode, RowContext, RowKey, RowMutation, RuleId, StalenessBound,
    StalenessReport, WriteOutcome, WriteRequest,
};
use serde_json::{Value, json};

/// The fixed shard for every scenario.
pub(crate) fn shard() -> ShardId {
    ShardId::new(vec![1])
}

/// The fixed catalog entry for every scenario.
fn catalog() -> PolicyCatalogEntry {
    PolicyCatalogEntry::new(
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
        MapVersion::new(3),
    )
}

/// The fixed queue deadline.
fn deadline() -> QueueDeadline {
    QueueDeadline::new(100)
}

/// A projection with one fact source at the given watermark.
fn projection_with(watermark: u64) -> FactProjection {
    let mut watermarks = ProjectionWatermarks::new();
    watermarks.set(FactSourceId::new("accounts"), FactVersion::new(watermark));
    FactProjection::new(ProjectionEpoch::new(7), watermarks)
}

/// The current fact versions for one fact source.
fn facts(version: u64) -> ProjectionWatermarks {
    let mut watermarks = ProjectionWatermarks::new();
    watermarks.set(FactSourceId::new("accounts"), FactVersion::new(version));
    watermarks
}

/// The default authorization policy with a bound of one version.
fn default_authorization() -> AuthorizationPolicy {
    AuthorizationPolicy::default_mode(StalenessBound::uniform(1))
}

/// The strict authorization policy with a bound of one version.
fn strict_authorization() -> AuthorizationPolicy {
    AuthorizationPolicy::strict(StalenessBound::uniform(1))
}

/// A request with one row.
fn request(
    identity: u8,
    key: &[u8],
    base_version: u64,
    value: &[u8],
    row_shard: &ShardId,
) -> WriteRequest {
    WriteRequest::new(
        RequestId::from_bytes([identity; 32]),
        IdempotencyKey::new(vec![identity]),
        KeyVersion::new(base_version),
        vec![RowMutation::new(
            RowKey::new(key.to_vec()),
            row_shard.clone(),
            value.to_vec(),
        )],
    )
}

/// A request with several rows.
fn batch_request(identity: u8, rows: Vec<RowMutation>) -> WriteRequest {
    WriteRequest::new(
        RequestId::from_bytes([identity; 32]),
        IdempotencyKey::new(vec![identity]),
        KeyVersion::new(0),
        rows,
    )
}

/// The production rules used by the scenarios.
struct PayloadRules;

impl PolicyEvaluator for PayloadRules {
    fn evaluate(&self, context: &RowContext<'_>) -> oxkv_enforce::Evaluation {
        if context.after.value == b"deny" {
            oxkv_enforce::Evaluation::Reject {
                rule: RuleId::new("rule.payload"),
                reason: ReasonCode::FORBIDDEN,
            }
        } else if context.after.value == b"budget" {
            oxkv_enforce::Evaluation::BudgetExhausted {
                rule: RuleId::new("rule.payload"),
            }
        } else {
            oxkv_enforce::Evaluation::Allow {
                rule: RuleId::new("rule.payload"),
            }
        }
    }
}

/// The authenticator that accepts every request.
struct AlwaysAuth;

impl oxkv_enforce::Authenticator for AlwaysAuth {
    fn authenticate(
        &self,
        _request: &WriteRequest,
    ) -> Result<oxkv_enforce::Principal, oxkv_enforce::AuthError> {
        Ok(oxkv_enforce::Principal::new(b"alice".to_vec()))
    }
}

/// The audit sink that always fails.
struct FailingAudit;

impl AuditSink for FailingAudit {
    fn append(&mut self, _body: AuditRecordBody) -> Result<oxkv_enforce::AuditRecord, AuditError> {
        Err(AuditError::Unavailable)
    }
}

/// One decision for comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecisionView {
    /// The row key.
    pub row_key: String,
    /// The rule identity.
    pub rule: String,
    /// The verdict.
    pub verdict: String,
    /// The reason code.
    pub reason: String,
}

/// The observable result of one scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScenarioOutcome {
    /// The final receipt state.
    pub receipt_state: Option<String>,
    /// The final receipt reason.
    pub reason: Option<String>,
    /// The number of audit records.
    pub audit_count: usize,
    /// The number of receipts.
    pub receipt_count: usize,
    /// True when the proposed value reached the data state.
    pub data_changed: bool,
    /// The decision list.
    pub decisions: Vec<DecisionView>,
    /// True when the chain verifies.
    pub chain_verified: bool,
    /// True when the receipt names the audit record.
    pub receipt_names_audit: bool,
    /// The record hash in hexadecimal form.
    pub record_hash_hex: String,
    /// The previous hash in hexadecimal form.
    pub previous_hash_hex: String,
    /// The error code for a failed call.
    pub error_code: Option<String>,
    /// The measured maximum version distance.
    pub staleness_distance: Option<u64>,
    /// The first queued receipt state.
    pub queued_state: Option<String>,
    /// The first queued receipt reason.
    pub queued_reason: Option<String>,
    /// The audit count before the queue deadline.
    pub queued_audit_count: Option<usize>,
}

impl ScenarioOutcome {
    /// Builds the outcome from one write result.
    fn from_outcome(
        outcome: &WriteOutcome,
        engine: &Enforcement,
        chain: &InMemoryAuditChain,
        data_changed: bool,
    ) -> Self {
        let receipt = outcome.receipt();
        let audit = outcome.audit();
        let decisions = audit.map_or_else(Vec::new, |record| {
            record
                .body()
                .decisions
                .iter()
                .map(|decision| DecisionView {
                    row_key: String::from_utf8_lossy(decision.row_key.as_bytes()).into_owned(),
                    rule: decision.rule.as_str().to_owned(),
                    verdict: decision.verdict.name().to_owned(),
                    reason: decision.reason.as_str().to_owned(),
                })
                .collect()
        });
        let receipt_names_audit = audit.is_some_and(|record| {
            receipt.audit().record_hash == record.record_hash()
                && receipt.audit().audit_sequence == record.sequence()
                && record.body().receipt_id == receipt.receipt_id()
        });
        Self {
            receipt_state: Some(receipt.state().name().to_owned()),
            reason: receipt.reason_code().map(str::to_owned),
            audit_count: chain.record_count(engine.shard_id()),
            receipt_count: engine.receipt_count(),
            data_changed,
            decisions,
            chain_verified: chain.verify(engine.shard_id()).is_ok(),
            receipt_names_audit,
            record_hash_hex: audit.map_or_else(String::new, |record| record.record_hash().to_hex()),
            previous_hash_hex: audit
                .map_or_else(String::new, |record| record.previous_hash().to_hex()),
            error_code: None,
            staleness_distance: audit
                .and_then(|record| record.body().staleness.as_ref())
                .and_then(StalenessReport::max_distance),
            queued_state: None,
            queued_reason: None,
            queued_audit_count: None,
        }
    }

    /// Builds the outcome from one failed call.
    fn from_error(
        error_code: &str,
        engine: &Enforcement,
        chain: &InMemoryAuditChain,
        data_changed: bool,
    ) -> Self {
        Self {
            receipt_state: None,
            reason: None,
            audit_count: chain.record_count(engine.shard_id()),
            receipt_count: engine.receipt_count(),
            data_changed,
            decisions: Vec::new(),
            chain_verified: chain.verify(engine.shard_id()).is_ok(),
            receipt_names_audit: false,
            record_hash_hex: String::new(),
            previous_hash_hex: String::new(),
            error_code: Some(error_code.to_owned()),
            staleness_distance: None,
            queued_state: None,
            queued_reason: None,
            queued_audit_count: None,
        }
    }

    /// Returns the outcome as a JSON value.
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "receipt_state": self.receipt_state,
            "reason": self.reason,
            "audit_count": self.audit_count,
            "receipt_count": self.receipt_count,
            "data_changed": self.data_changed,
            "decisions": self.decisions.iter().map(|decision| json!({
                "row_key": decision.row_key,
                "rule": decision.rule,
                "verdict": decision.verdict,
                "reason": decision.reason,
            })).collect::<Vec<_>>(),
            "chain_verified": self.chain_verified,
            "receipt_names_audit": self.receipt_names_audit,
            "record_hash_hex": self.record_hash_hex,
            "previous_hash_hex": self.previous_hash_hex,
            "error_code": self.error_code,
            "staleness_distance": self.staleness_distance,
            "queued_state": self.queued_state,
            "queued_reason": self.queued_reason,
            "queued_audit_count": self.queued_audit_count,
        })
    }
}

/// Runs one named scenario.
pub(crate) fn run(name: &str) -> ScenarioOutcome {
    match name {
        "committed_write" => committed_write(),
        "rejected_policy_write" => rejected_policy_write(),
        "stale_base_policy_violation" => stale_base_policy_violation(),
        "non_strict_over_bound_timeout" => non_strict_over_bound_timeout(),
        "strict_over_bound" => strict_over_bound(),
        "audit_append_failure" => audit_append_failure(),
        "per_row_decision_list" => per_row_decision_list(),
        "batch_conjunction" => batch_conjunction(),
        "audit_chain_verifies" => audit_chain_verifies(),
        "receipt_names_audit" => receipt_names_audit(),
        "budget_exhaustion" => budget_exhaustion(),
        "projection_checksum_failure" => projection_checksum_failure(),
        "projection_client_write" => projection_client_write(),
        "cross_shard_write" => cross_shard_write(),
        "idempotency_retry" => idempotency_retry(),
        "idempotency_key_reuse" => idempotency_key_reuse(),
        "stale_base_version" => stale_base_version(),
        "missing_base_version" => missing_base_version(),
        _ => panic!("the scenario {name} is unknown"),
    }
}

/// A committed write produces one audit record.
fn committed_write() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = request(1, b"row:1", 0, b"ok", &shard);
    let outcome = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the write commits");
    let key = RowKey::new(b"row:1".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"ok");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A rejected policy write produces one receipt and one audit record.
fn rejected_policy_write() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = request(2, b"row:2", 0, b"deny", &shard);
    let outcome = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the write rejects");
    let key = RowKey::new(b"row:2".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"deny");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A stale base with a policy violation returns Conflict.
fn stale_base_policy_violation() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let first = request(3, b"row:3", 0, b"ok", &shard);
    engine
        .commit(&first, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the first write commits");
    let second = request(4, b"row:3", 0, b"deny", &shard);
    let outcome = engine
        .commit(&second, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the stale write conflicts");
    let key = RowKey::new(b"row:3".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"deny");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A non-strict over-bound write stays Queued and then times out.
fn non_strict_over_bound_timeout() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(13);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = request(5, b"row:5", 0, b"ok", &shard);
    let queued = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the write queues");
    let queued_state = Some(queued.state().name().to_owned());
    let queued_reason = queued.receipt().reason_code().map(str::to_owned);
    let queued_audit_count = Some(chain.record_count(&shard));
    let mut expired = engine
        .expire_due(deadline(), &mut chain)
        .expect("the queued write times out");
    let outcome = expired.pop().expect("one queued write expires");
    let key = RowKey::new(b"row:5".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"ok");
    let mut result = ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed);
    result.queued_state = queued_state;
    result.queued_reason = queued_reason;
    result.queued_audit_count = queued_audit_count;
    result
}

/// A strict over-bound write rejects at once.
fn strict_over_bound() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(13);
    let authorization = strict_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = request(6, b"row:6", 0, b"ok", &shard);
    let outcome = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the strict write rejects");
    let key = RowKey::new(b"row:6".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"ok");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A failed audit append rejects the write and changes no state.
fn audit_append_failure() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = request(7, b"row:7", 0, b"ok", &shard);
    let result = engine.commit(
        &request,
        &AlwaysAuth,
        &PayloadRules,
        &context,
        &mut FailingAudit,
    );
    let error_code = result.err().map_or("none", |error| error.code()).to_owned();
    ScenarioOutcome::from_error(&error_code, &engine, &chain, false)
}

/// The decision list names the failing row, rule, and reason.
fn per_row_decision_list() -> ScenarioOutcome {
    batch_conjunction()
}

/// A batch with one reject changes no row.
fn batch_conjunction() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = batch_request(
        8,
        vec![
            RowMutation::new(
                RowKey::new(b"row:8a".to_vec()),
                shard.clone(),
                b"ok".to_vec(),
            ),
            RowMutation::new(
                RowKey::new(b"row:8b".to_vec()),
                shard.clone(),
                b"deny".to_vec(),
            ),
        ],
    );
    let outcome = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the batch rejects");
    let first = RowKey::new(b"row:8a".to_vec());
    let second = RowKey::new(b"row:8b".to_vec());
    let changed = engine
        .state()
        .row(&first)
        .is_some_and(|row| row.value() == b"ok")
        || engine
            .state()
            .row(&second)
            .is_some_and(|row| row.value() == b"deny");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// Two committed writes form a verifiable hash chain.
fn audit_chain_verifies() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let first = request(9, b"row:9a", 0, b"ok", &shard);
    engine
        .commit(&first, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the first write commits");
    let second = request(10, b"row:9b", 0, b"ok", &shard);
    let outcome = engine
        .commit(&second, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the second write commits");
    let key = RowKey::new(b"row:9b".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"ok");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A terminal receipt names the audit record that justified it.
fn receipt_names_audit() -> ScenarioOutcome {
    committed_write()
}

/// A budget failure maps to Rejected, not Conflict.
fn budget_exhaustion() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = request(11, b"row:11", 0, b"budget", &shard);
    let outcome = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the budget rejection writes a receipt");
    let key = RowKey::new(b"row:11".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"budget");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A projection checksum failure fails closed.
fn projection_checksum_failure() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = FactProjection::from_parts(
        ProjectionEpoch::new(7),
        facts(10),
        Hash32::from_bytes([9; 32]),
    );
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = request(12, b"row:12", 0, b"ok", &shard);
    let outcome = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the checksum rejection writes a receipt");
    let key = RowKey::new(b"row:12".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"ok");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A client write to a projection rejects.
fn projection_client_write() -> ScenarioOutcome {
    let shard = shard();
    let engine = Enforcement::new(shard.clone());
    let chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let error = projection
        .admit_write(ProjectionWriteOrigin::Client)
        .err()
        .map_or("none", |error| error.code());
    ScenarioOutcome::from_error(error, &engine, &chain, false)
}

/// A cross-shard write rejects before the version check.
fn cross_shard_write() -> ScenarioOutcome {
    let shard = shard();
    let other = ShardId::new(vec![2]);
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = request(13, b"row:13", 9, b"deny", &other);
    let outcome = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the cross-shard rejection writes a receipt");
    let key = RowKey::new(b"row:13".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"deny");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A retry with the same key and body returns the original receipt.
fn idempotency_retry() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = request(14, b"row:14", 0, b"ok", &shard);
    let _first = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the first delivery commits");
    let _second = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the second delivery returns the receipt");
    let outcome = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the third delivery returns the receipt");
    let key = RowKey::new(b"row:14".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"ok");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A reused key with a different body fails and writes no second receipt.
fn idempotency_key_reuse() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let first = request(15, b"row:15", 0, b"ok", &shard);
    engine
        .commit(&first, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the first delivery commits");
    let second = WriteRequest::new(
        RequestId::from_bytes([15; 32]),
        IdempotencyKey::new(vec![15]),
        KeyVersion::new(0),
        vec![RowMutation::new(
            RowKey::new(b"row:15".to_vec()),
            shard,
            b"other".to_vec(),
        )],
    );
    let error = engine
        .commit(&second, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .err()
        .map_or("none", |error| error.code());
    ScenarioOutcome::from_error(error, &engine, &chain, true)
}

/// A base version ahead of the current version rejects.
fn stale_base_version() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let first = request(16, b"row:16", 0, b"ok", &shard);
    engine
        .commit(&first, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the first delivery commits");
    let second = request(17, b"row:16", 2, b"ahead", &shard);
    let outcome = engine
        .commit(&second, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the ahead base rejects");
    let key = RowKey::new(b"row:16".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"ahead");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}

/// A request without a base version rejects.
fn missing_base_version() -> ScenarioOutcome {
    let shard = shard();
    let mut engine = Enforcement::new(shard.clone());
    let mut chain = InMemoryAuditChain::new();
    let projection = projection_with(10);
    let current = facts(11);
    let authorization = default_authorization();
    let context = CommitContext {
        catalog: catalog(),
        projection: &projection,
        current_facts: &current,
        authorization: &authorization,
        queue_deadline: deadline(),
    };
    let request = WriteRequest::without_base_version(
        RequestId::from_bytes([18; 32]),
        IdempotencyKey::new(vec![18]),
        vec![RowMutation::new(
            RowKey::new(b"row:18".to_vec()),
            shard,
            b"ok".to_vec(),
        )],
    );
    let outcome = engine
        .commit(&request, &AlwaysAuth, &PayloadRules, &context, &mut chain)
        .expect("the missing base rejection writes a receipt");
    let key = RowKey::new(b"row:18".to_vec());
    let changed = engine
        .state()
        .row(&key)
        .is_some_and(|row| row.value() == b"ok");
    ScenarioOutcome::from_outcome(&outcome, &engine, &chain, changed)
}
