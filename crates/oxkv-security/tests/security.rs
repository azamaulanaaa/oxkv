//! Behavioral tests for the security boundary.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use oxkv_contracts::error::lookup;
use oxkv_contracts::receipt::{AuditSequence, IdempotencyKey, ReceiptState, RequestId};
use oxkv_contracts::version::{
    CommitVersion, FactSourceId, FactVersion, KeyVersion, ManifestDigest, MapVersion,
    PolicyVersion, ProjectionEpoch, ScopeIdentity, ShardId,
};
use oxkv_security::access::{AccessDecision, AccessEntry, AccessLog, RetentionClass};
use oxkv_security::audit::{AuditChain, AuditDecision, RuleIdentity, StructuralSigner};
use oxkv_security::auth::{
    CredentialDirectory, CredentialId, CredentialProof, LogicalTime, PrincipalId, RevocationPolicy,
};
use oxkv_security::error::SecurityError;
use oxkv_security::gate::{
    AllowAllAuthorizer, Authorizer, DenyAllAuthorizer, EnforcementInput, ReadAccess, ReadRequest,
    SecurityGate, WriteAccess, WriteOutcome,
};
use oxkv_security::policy::{
    PolicyAst, PolicyLimits, PolicyNode, SearchNode, SearchQuery, StepBudget, compile_policy,
    compile_search,
};
use oxkv_security::secret::{KeyIdentifier, Secret};
use oxkv_security::signature::{Nonce, RequestFields, RequestKey, SignedRequest};
use oxkv_security::witness::WitnessQueue;

fn shard_id() -> ShardId {
    ShardId::new(vec![1])
}

fn scope() -> ScopeIdentity {
    ScopeIdentity::from_bytes([3; 32])
}

fn principal() -> PrincipalId {
    PrincipalId::from_bytes(vec![2])
}

fn credential() -> CredentialId {
    CredentialId::from_bytes(vec![1])
}

fn proof() -> CredentialProof {
    CredentialProof::new(credential())
}

fn directory() -> CredentialDirectory {
    let mut directory = CredentialDirectory::new(RevocationPolicy::new(10));
    directory.insert(credential(), principal());
    directory
}

fn request_key() -> RequestKey {
    RequestKey::new([7; 32])
}

fn checkpoint_signer() -> StructuralSigner {
    StructuralSigner::new(KeyIdentifier::new("witness-key-1"))
}

fn input() -> EnforcementInput {
    let mut fact_versions = BTreeMap::new();
    fact_versions.insert(FactSourceId::new("accounts"), FactVersion::new(3));
    EnforcementInput {
        policy_version: PolicyVersion::from_bytes([4; 32]),
        manifest_digest: ManifestDigest::from_bytes([5; 32]),
        map_version: MapVersion::new(6),
        projection_epoch: ProjectionEpoch::new(7),
        fact_versions,
        rule_identity: RuleIdentity::new("accounts/write"),
        commit_version: CommitVersion::new(10),
    }
}

fn fields(body: &[u8], key: &[u8], nonce: u8, expires_at: u64) -> RequestFields {
    RequestFields {
        request_id: RequestId::from_bytes([1; 32]),
        body: body.to_vec(),
        idempotency_key: IdempotencyKey::new(key.to_vec()),
        base_version: KeyVersion::new(7),
        shard_id: shard_id(),
        nonce: Nonce::from_bytes([nonce; 32]),
        expires_at: LogicalTime::new(expires_at),
    }
}

fn signed(body: &[u8], key: &[u8], nonce: u8, expires_at: u64) -> SignedRequest {
    request_key().sign(fields(body, key, nonce, expires_at))
}

fn gate_with<A: Authorizer>(authorizer: A) -> SecurityGate<CredentialDirectory, A, RequestKey> {
    SecurityGate::new(directory(), authorizer, request_key(), shard_id())
}

fn submit<A: Authorizer>(
    gate: &mut SecurityGate<CredentialDirectory, A, RequestKey>,
    request: &SignedRequest,
    witness: &mut WitnessQueue,
    now: u64,
) -> Result<WriteOutcome, SecurityError> {
    let signer = checkpoint_signer();
    gate.submit_write(
        &proof(),
        request,
        &input(),
        &signer,
        witness,
        LogicalTime::new(now),
    )
}

#[test]
fn an_unauthenticated_request_returns_no_receipt() {
    let mut gate = gate_with(AllowAllAuthorizer);
    let mut witness = WitnessQueue::new();
    let request = signed(b"payload", b"key", 9, 100);
    let unknown = CredentialProof::new(CredentialId::from_bytes(vec![99]));
    let signer = checkpoint_signer();
    let result = gate.submit_write(
        &unknown,
        &request,
        &input(),
        &signer,
        &mut witness,
        LogicalTime::new(10),
    );

    assert_eq!(result.unwrap_err(), SecurityError::Unauthenticated);
    assert_eq!(gate.receipt_count(), 0);
    assert!(gate.audit().is_empty());
    assert_eq!(witness.pending_len(), 0);
}

#[test]
fn a_replayed_nonce_with_a_different_body_fails() {
    let mut gate = gate_with(AllowAllAuthorizer);
    let mut witness = WitnessQueue::new();
    let first = signed(b"first body", b"same-key", 9, 100);
    submit(&mut gate, &first, &mut witness, 10).expect("the first write commits");

    let replay = signed(b"second body", b"same-key", 9, 100);
    let error = submit(&mut gate, &replay, &mut witness, 10).unwrap_err();

    assert_eq!(error, SecurityError::ReplayedRequest);
    assert_eq!(error.code(), "auth.replayed_request");
    assert_eq!(gate.receipt_count(), 1);
    assert_eq!(gate.audit().len(), 1);
}

#[test]
fn a_retry_with_the_same_key_and_body_returns_the_original_receipt() {
    let mut gate = gate_with(AllowAllAuthorizer);
    let mut witness = WitnessQueue::new();
    let request = signed(b"payload", b"key", 9, 100);

    let first = submit(&mut gate, &request, &mut witness, 10).expect("the first write commits");
    let second = submit(&mut gate, &request, &mut witness, 200)
        .expect("the retry returns the original receipt");

    assert_eq!(first.receipt(), second.receipt());
    assert_eq!(first.state(), ReceiptState::Committed);
    assert_eq!(gate.receipt_count(), 1);
    assert_eq!(gate.audit().len(), 1);
    assert_eq!(witness.pending_len(), 1);
}

#[test]
fn an_expired_signature_fails_for_a_new_request() {
    let mut gate = gate_with(AllowAllAuthorizer);
    let mut witness = WitnessQueue::new();
    let request = signed(b"payload", b"fresh-key", 9, 100);

    let error = submit(&mut gate, &request, &mut witness, 200).unwrap_err();

    assert_eq!(error, SecurityError::Unauthenticated);
    assert_eq!(gate.receipt_count(), 0);
    assert!(gate.audit().is_empty());
}

#[test]
fn a_signature_from_another_key_fails() {
    let mut gate = SecurityGate::new(
        directory(),
        AllowAllAuthorizer,
        RequestKey::new([8; 32]),
        shard_id(),
    );
    let mut witness = WitnessQueue::new();
    let request = request_key().sign(fields(b"payload", b"key", 9, 100));

    let error = submit(&mut gate, &request, &mut witness, 10).unwrap_err();

    assert_eq!(error, SecurityError::Unauthenticated);
    assert_eq!(gate.receipt_count(), 0);
}

#[test]
fn a_denied_read_does_not_reveal_whether_a_record_exists() {
    let mut gate = gate_with(DenyAllAuthorizer);
    let hidden_records: BTreeSet<Vec<u8>> = [b"hidden".to_vec()].into_iter().collect();
    assert!(hidden_records.contains(b"hidden".as_slice()));
    assert!(!hidden_records.contains(b"absent".as_slice()));

    let existing = gate
        .evaluate_read(
            &proof(),
            &ReadRequest::new(
                RequestId::from_bytes([11; 32]),
                shard_id(),
                b"hidden".to_vec(),
                scope(),
            ),
            LogicalTime::new(10),
        )
        .expect("an authenticated read returns a decision");
    let missing = gate
        .evaluate_read(
            &proof(),
            &ReadRequest::new(
                RequestId::from_bytes([12; 32]),
                shard_id(),
                b"absent".to_vec(),
                scope(),
            ),
            LogicalTime::new(10),
        )
        .expect("an authenticated read returns a decision");

    assert_eq!(existing.decision(), AccessDecision::Denied);
    assert_eq!(missing.decision(), AccessDecision::Denied);
    assert_eq!(existing.reason_code(), missing.reason_code());
    assert_eq!(existing.reason_code(), Some("auth.forbidden"));
    assert!(!format!("{existing:?}").contains("hidden"));
    assert!(!format!("{missing:?}").contains("absent"));
    assert_eq!(gate.audit().len(), 0);
    assert_eq!(gate.access_log().len(), 2);
}

#[test]
fn a_denied_write_returns_rejected_with_the_coarse_reason() {
    let mut gate = gate_with(DenyAllAuthorizer);
    let mut witness = WitnessQueue::new();
    let request = signed(b"payload", b"key", 9, 100);

    let outcome = submit(&mut gate, &request, &mut witness, 10).expect("the verdict is recorded");

    assert_eq!(outcome.state(), ReceiptState::Rejected);
    assert_eq!(
        outcome.receipt().reason_code(),
        Some(SecurityError::Forbidden.code())
    );
    assert_eq!(gate.audit().len(), 1);
    assert_eq!(
        gate.audit().records()[0].entry.decision,
        AuditDecision::Rejected
    );
    assert_eq!(
        gate.audit().records()[0].entry.reason_code,
        Some("auth.forbidden")
    );
    assert_eq!(gate.receipt_count(), 1);
}

#[test]
fn a_failed_audit_append_rejects_the_write() {
    let mut gate = gate_with(AllowAllAuthorizer);
    let mut witness = WitnessQueue::new();
    let request = signed(b"payload", b"key", 9, 100);
    gate.set_audit_available(false);

    let error = submit(&mut gate, &request, &mut witness, 10).unwrap_err();

    assert_eq!(error, SecurityError::AuditUnavailable);
    assert_eq!(error.code(), "audit.unavailable");
    assert_eq!(error.receipt_state(), None);
    assert!(lookup("audit.unavailable").is_some_and(|spec| spec.receipt.is_none()));
    assert_eq!(gate.receipt_count(), 0);
    assert!(gate.audit().is_empty());
    assert_eq!(gate.nonce_count(), 0);

    gate.set_audit_available(true);
    let outcome = submit(&mut gate, &request, &mut witness, 10).expect("the retry commits");
    assert_eq!(outcome.state(), ReceiptState::Committed);
}

#[test]
fn the_audit_chain_verifies_to_the_last_checkpoint() {
    let mut gate = gate_with(AllowAllAuthorizer);
    let mut witness = WitnessQueue::new();
    for nonce in 1..=3 {
        let key = format!("key-{nonce}");
        let request = signed(b"payload", key.as_bytes(), nonce, 100);
        submit(&mut gate, &request, &mut witness, 10).expect("the write commits");
    }

    let report = gate
        .audit()
        .verify()
        .expect("the chain verifies to the last checkpoint");
    assert_eq!(report.records, 3);
    assert_eq!(report.checkpoints, 3);
    assert_eq!(report.tip, Some(gate.audit().records()[2].record_hash));
    assert_eq!(report.last_checkpoint, Some(AuditSequence::new(3)));

    let mut records = gate.audit().records().to_vec();
    records[0].entry.reason_code = Some("auth.forbidden");
    let changed = AuditChain::from_parts(shard_id(), records, gate.audit().checkpoints().to_vec());
    assert_eq!(
        changed.verify().unwrap_err(),
        SecurityError::AuditChainInvalid
    );

    let mut records = gate.audit().records().to_vec();
    records.remove(1);
    let missing = AuditChain::from_parts(shard_id(), records, gate.audit().checkpoints().to_vec());
    assert_eq!(
        missing.verify().unwrap_err(),
        SecurityError::AuditChainInvalid
    );
}

#[test]
fn the_witness_receives_a_checkpoint_after_commit() {
    let mut gate = gate_with(AllowAllAuthorizer);
    let mut witness = WitnessQueue::new();
    let request = signed(b"payload", b"key", 9, 100);

    let outcome = submit(&mut gate, &request, &mut witness, 10).expect("the write commits");

    assert_eq!(witness.pending_len(), 1);
    assert_eq!(witness.delivered_len(), 0);
    assert_eq!(
        outcome.audit_reference().record_hash,
        witness
            .pending()
            .front()
            .expect("one checkpoint waits")
            .record_hash
    );
    let delivered = witness.deliver_next().expect("the queue delivers one");
    assert_eq!(delivered, *outcome.checkpoint());
    assert_eq!(witness.delivered_len(), 1);
}

#[test]
fn the_access_log_is_separate_from_the_audit_chain() {
    let mut gate = gate_with(AllowAllAuthorizer);
    let mut witness = WitnessQueue::new();
    let write = signed(b"payload", b"key", 9, 100);
    submit(&mut gate, &write, &mut witness, 10).expect("the write commits");
    assert_eq!(gate.audit().len(), 1);

    let read_id = RequestId::from_bytes([12; 32]);
    let read = ReadRequest::new(read_id, shard_id(), b"key".to_vec(), scope());
    gate.evaluate_read(&proof(), &read, LogicalTime::new(10))
        .expect("the read returns a decision");

    assert_eq!(gate.audit().len(), 1);
    assert_eq!(gate.access_log().len(), 1);
    assert_eq!(gate.access_log().entries()[0].event.request_id, read_id);
    assert!(
        gate.audit()
            .records()
            .iter()
            .all(|record| record.entry.request_id != read_id)
    );
    assert_eq!(AccessLog::retention_class(), RetentionClass::AccessLog);
    assert_eq!(AccessEntry::retention_class(), RetentionClass::AccessLog);
    assert_ne!(AccessLog::retention_class(), RetentionClass::Audit);
}

#[test]
fn secrets_do_not_appear_in_errors_or_audit_output() {
    let mut gate = gate_with(AllowAllAuthorizer);
    let mut witness = WitnessQueue::new();
    let canary = b"CANARY-SECRET-29";
    let canary_text = "CANARY-SECRET-29";
    let request = signed(canary, b"key", 9, 100);

    let outcome = submit(&mut gate, &request, &mut witness, 10).expect("the write commits");
    let export = gate.audit().export(true).expect("the chain exports");
    let text = format!("{outcome:?} {:?} {:?} {:?}", request, gate.audit(), export);
    assert!(!text.contains(canary_text));

    let read = ReadRequest::new(
        RequestId::from_bytes([12; 32]),
        shard_id(),
        canary.to_vec(),
        scope(),
    );
    gate.evaluate_read(&proof(), &read, LogicalTime::new(10))
        .expect("the read returns a decision");
    assert!(!format!("{:?}", gate.access_log()).contains(canary_text));

    let error = SecurityError::Unauthenticated;
    assert!(!format!("{error} {error:?}").contains(canary_text));

    let secret = Secret::new(canary.to_vec());
    assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
    assert_eq!(format!("{secret}"), "[REDACTED]");
}

#[test]
fn a_regex_policy_is_rejected() {
    let policy = PolicyAst {
        nodes: vec![PolicyNode::Regex(String::from(".*"))],
    };
    let error = compile_policy(&policy, PolicyLimits::new(8, 4, 1024)).unwrap_err();
    assert_eq!(error, SecurityError::RegexNotSupported);
    assert_eq!(error.code(), "policy.regex_not_supported");

    let query = SearchQuery {
        nodes: vec![SearchNode::Regex(String::from(".*"))],
    };
    assert_eq!(
        compile_search(&query).unwrap_err(),
        SecurityError::RegexNotSupported
    );
}

struct CountingAuthorizer {
    writes: Rc<Cell<usize>>,
    reads: Rc<Cell<usize>>,
}

impl Authorizer for CountingAuthorizer {
    fn allow_write(
        &self,
        _subject: &oxkv_security::auth::Subject,
        _access: &WriteAccess<'_>,
    ) -> bool {
        self.writes.set(self.writes.get() + 1);
        true
    }

    fn allow_read(
        &self,
        _subject: &oxkv_security::auth::Subject,
        _access: &ReadAccess<'_>,
    ) -> bool {
        self.reads.set(self.reads.get() + 1);
        true
    }
}

#[test]
fn authentication_runs_before_authorization() {
    let writes = Rc::new(Cell::new(0));
    let reads = Rc::new(Cell::new(0));
    let authorizer = CountingAuthorizer {
        writes: Rc::clone(&writes),
        reads: Rc::clone(&reads),
    };
    let mut gate = SecurityGate::new(directory(), authorizer, request_key(), shard_id());
    let mut witness = WitnessQueue::new();
    let unknown = CredentialProof::new(CredentialId::from_bytes(vec![99]));
    let request = signed(b"payload", b"key", 9, 100);
    let signer = checkpoint_signer();

    let error = gate
        .submit_write(
            &unknown,
            &request,
            &input(),
            &signer,
            &mut witness,
            LogicalTime::new(10),
        )
        .unwrap_err();
    assert_eq!(error, SecurityError::Unauthenticated);

    let read = ReadRequest::new(
        RequestId::from_bytes([12; 32]),
        shard_id(),
        b"key".to_vec(),
        scope(),
    );
    let error = gate
        .evaluate_read(&unknown, &read, LogicalTime::new(10))
        .unwrap_err();
    assert_eq!(error, SecurityError::Unauthenticated);
    assert_eq!(writes.get(), 0);
    assert_eq!(reads.get(), 0);

    submit(&mut gate, &request, &mut witness, 10).expect("the valid write commits");
    gate.evaluate_read(&proof(), &read, LogicalTime::new(10))
        .expect("the valid read returns a decision");
    assert_eq!(writes.get(), 1);
    assert_eq!(reads.get(), 1);
}

#[test]
fn a_revoked_credential_fails_after_the_staleness_bound() {
    let mut directory = CredentialDirectory::new(RevocationPolicy::new(10));
    directory.insert(credential(), principal());
    directory.revoke(credential(), LogicalTime::new(50));
    let mut gate = SecurityGate::new(directory, AllowAllAuthorizer, request_key(), shard_id());
    let read = ReadRequest::new(
        RequestId::from_bytes([12; 32]),
        shard_id(),
        b"key".to_vec(),
        scope(),
    );

    gate.evaluate_read(&proof(), &read, LogicalTime::new(60))
        .expect("the credential is valid through the bound");
    let error = gate
        .evaluate_read(&proof(), &read, LogicalTime::new(61))
        .unwrap_err();
    assert_eq!(error, SecurityError::StaleCredentials);
    assert_eq!(gate.access_log().len(), 1);
}

#[test]
fn error_codes_match_the_contract_registry() {
    for error in SecurityError::ALL {
        let spec = lookup(error.code())
            .unwrap_or_else(|| panic!("the code {} is not registered", error.code()));
        assert_eq!(spec.receipt, error.receipt_state());
    }
    for code in [
        "auth.unauthenticated",
        "auth.stale_credentials",
        "auth.forbidden",
        "auth.replayed_request",
        "audit.unavailable",
        "audit.chain_invalid",
        "audit.export_denied",
        "policy.regex_not_supported",
    ] {
        assert!(lookup(code).is_some());
    }
}

#[test]
fn step_budget_exhaustion_denies() {
    let policy = PolicyAst {
        nodes: vec![
            PolicyNode::Constant(true),
            PolicyNode::Constant(true),
            PolicyNode::Constant(false),
        ],
    };
    let compiled = compile_policy(&policy, PolicyLimits::new(8, 8, 1024))
        .expect("the policy is within the depth budget");
    let mut budget = StepBudget::new(2);

    assert_eq!(
        compiled.evaluate(&mut budget).unwrap_err(),
        SecurityError::PolicyBudgetExhausted
    );
    assert_eq!(budget.used(), 2);
    assert_eq!(budget.remaining(), 0);

    assert_eq!(
        compile_policy(&policy, PolicyLimits::new(8, 2, 1024)).unwrap_err(),
        SecurityError::PolicyBudgetExhausted
    );
}
