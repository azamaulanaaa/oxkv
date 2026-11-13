//! Acceptance tests for versioned writes.

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptOutcome, ReceiptState, RequestId};
use oxkv_contracts::version::{
    KeyVersion, ManifestDigest, MapVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
    ShardId,
};
use oxkv_write::{
    AllowAll, AuditVerdict, Authority, AuthorityConfig, ClientId, CommitHook, CommitView,
    HookEffect, HookError, Key, Mutation, Policy, PolicyDecision, PolicyEvaluation, PolicyRow,
    ReceiptLedger, RuleIdentity, SubmitError, SubmitOutcome, WriteOperation, WriteRequest,
    rebase_request,
};

struct RejectPolicy;

impl Policy for RejectPolicy {
    fn evaluate(&self, _request: &WriteRequest, rows: &[PolicyRow<'_>]) -> PolicyEvaluation {
        let rule = RuleIdentity::new("reject-all");
        let decisions = rows
            .iter()
            .map(|row| {
                PolicyDecision::reject(row.key().clone(), rule.clone(), "policy.budget_exhausted")
            })
            .collect();
        PolicyEvaluation::reject(rule, "policy.budget_exhausted", decisions)
    }
}

struct PanicPolicy;

impl Policy for PanicPolicy {
    fn evaluate(&self, _request: &WriteRequest, _rows: &[PolicyRow<'_>]) -> PolicyEvaluation {
        panic!("the authority called policy before the version check")
    }
}

struct PanicHook;

impl CommitHook for PanicHook {
    fn name(&self) -> &'static str {
        "panic"
    }

    fn derive(&self, _view: &CommitView<'_>) -> Result<Vec<HookEffect>, HookError> {
        panic!("the authority called a hook for a non-commit")
    }
}

struct EffectHook;

impl CommitHook for EffectHook {
    fn name(&self) -> &'static str {
        "index"
    }

    fn derive(&self, view: &CommitView<'_>) -> Result<Vec<HookEffect>, HookError> {
        Ok(view
            .changes()
            .iter()
            .map(|change| {
                HookEffect::new(
                    "index",
                    change.key().as_bytes().to_vec(),
                    change.key_version().get().to_le_bytes().to_vec(),
                )
            })
            .collect())
    }
}

struct FailHook;

impl CommitHook for FailHook {
    fn name(&self) -> &'static str {
        "audit"
    }

    fn derive(&self, _view: &CommitView<'_>) -> Result<Vec<HookEffect>, HookError> {
        Err(HookError::new("audit.unavailable"))
    }
}

fn shard() -> ShardId {
    ShardId::new(vec![1])
}

fn other_shard() -> ShardId {
    ShardId::new(vec![2])
}

fn key(name: &str) -> Key {
    Key::new(name.as_bytes().to_vec())
}

fn authority() -> Authority {
    Authority::new(AuthorityConfig::new(
        shard(),
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
        MapVersion::new(6),
        ProjectionEpoch::new(7),
        ProjectionWatermarks::new(),
    ))
}

fn put_request(id: u8, name: &str, base: u64, value: &str) -> WriteRequest {
    WriteRequest::new(
        RequestId::from_bytes([id; 32]),
        ClientId::new(vec![1]),
        IdempotencyKey::new(vec![id]),
        [Mutation::put(
            key(name),
            shard(),
            KeyVersion::new(base),
            value.as_bytes().to_vec(),
        )],
    )
}

fn delete_request(id: u8, name: &str, base: u64) -> WriteRequest {
    WriteRequest::new(
        RequestId::from_bytes([id; 32]),
        ClientId::new(vec![1]),
        IdempotencyKey::new(vec![id]),
        [Mutation::delete(key(name), shard(), KeyVersion::new(base))],
    )
}

fn run(authority: &mut Authority, request: &WriteRequest) -> SubmitOutcome {
    authority
        .submit(request, &AllowAll, &[])
        .expect("the authority returns an outcome")
}

fn reason(outcome: &SubmitOutcome) -> &'static str {
    outcome
        .receipt()
        .reason_code()
        .expect("the receipt names a reason")
}

#[test]
fn two_clients_rename_from_one_base() {
    let mut authority = authority();
    let create = put_request(1, "account", 0, "one");
    assert_eq!(
        run(&mut authority, &create).state(),
        ReceiptState::Committed
    );

    let first = put_request(2, "account", 1, "alpha");
    let second = put_request(3, "account", 1, "beta");
    let first_outcome = run(&mut authority, &first);
    let second_outcome = run(&mut authority, &second);

    assert_eq!(first_outcome.state(), ReceiptState::Committed);
    assert_eq!(second_outcome.state(), ReceiptState::Conflict);
    assert_eq!(reason(&second_outcome), "write.version_conflict");
    assert_eq!(
        second_outcome.receipt().current_key_version(),
        Some(KeyVersion::new(2))
    );
    assert_eq!(
        authority
            .record(&key("account"))
            .and_then(|record| record.as_value()),
        Some(b"alpha".as_slice())
    );

    let rebase = rebase_request(
        &second,
        RequestId::from_bytes([4; 32]),
        IdempotencyKey::new(vec![4]),
        &[(key("account"), KeyVersion::new(2))],
    )
    .expect("the rebase has a current base");
    let rebased = run(&mut authority, &rebase);
    assert_eq!(rebased.state(), ReceiptState::Committed);
    assert_eq!(authority.key_version(&key("account")), KeyVersion::new(3));
    assert_eq!(
        authority
            .record(&key("account"))
            .and_then(|record| record.as_value()),
        Some(b"beta".as_slice())
    );

    let retry = run(&mut authority, &second);
    assert!(retry.is_duplicate());
    assert_eq!(retry.state(), ReceiptState::Conflict);
    assert_eq!(
        authority
            .receipt(second.idempotency_key())
            .expect("the old conflict receipt remains")
            .state(),
        ReceiptState::Conflict
    );
}

#[test]
fn three_deliveries_produce_one_version_and_three_receipts() {
    let mut authority = authority();
    let request = put_request(1, "account", 0, "one");

    let first = run(&mut authority, &request);
    let second = run(&mut authority, &request);
    let third = run(&mut authority, &request);

    assert_eq!(first.state(), ReceiptState::Committed);
    assert!(!first.is_duplicate());
    assert!(second.is_duplicate());
    assert!(third.is_duplicate());
    assert_eq!(first.receipt(), second.receipt());
    assert_eq!(first.receipt(), third.receipt());
    assert_eq!(authority.key_version(&key("account")), KeyVersion::new(1));
    assert_eq!(authority.commit_version().get(), 1);
    assert_eq!(authority.receipts().len(), 1);
}

#[test]
fn a_retry_with_a_new_request_identity_returns_the_same_receipt() {
    let mut authority = authority();
    let request = put_request(1, "account", 0, "one");
    let first = run(&mut authority, &request);
    let retry = WriteRequest::new(
        RequestId::from_bytes([9; 32]),
        ClientId::new(vec![1]),
        IdempotencyKey::new(vec![1]),
        [Mutation::put(
            key("account"),
            shard(),
            KeyVersion::new(0),
            b"one".to_vec(),
        )],
    );

    let outcome = run(&mut authority, &retry);
    assert!(outcome.is_duplicate());
    assert_eq!(outcome.receipt(), first.receipt());
    assert_eq!(authority.key_version(&key("account")), KeyVersion::new(1));
}

#[test]
fn a_reused_key_with_a_different_body_fails() {
    let mut authority = authority();
    let first = put_request(1, "account", 0, "one");
    run(&mut authority, &first);

    let reuse = WriteRequest::new(
        first.request_id(),
        first.client_id().clone(),
        first.idempotency_key().clone(),
        [Mutation::put(
            key("account"),
            shard(),
            KeyVersion::new(1),
            b"two".to_vec(),
        )],
    );
    let error = authority
        .submit(&reuse, &AllowAll, &[])
        .expect_err("the body differs");

    assert_eq!(error.code(), "write.idempotency_key_reuse");
    assert_eq!(authority.receipts().len(), 1);
    assert_eq!(authority.key_version(&key("account")), KeyVersion::new(1));
    assert_eq!(authority.commit_version().get(), 1);
}

#[test]
fn a_base_ahead_of_the_current_version_is_rejected() {
    let mut authority = authority();
    let request = put_request(1, "account", 9, "one");

    let outcome = run(&mut authority, &request);

    assert_eq!(outcome.state(), ReceiptState::Rejected);
    assert_eq!(reason(&outcome), "write.stale_base_version");
    assert!(authority.record(&key("account")).is_none());
    assert_eq!(authority.commit_version().get(), 0);
    assert_eq!(
        authority
            .receipt(request.idempotency_key())
            .expect("the rejection has a receipt")
            .state(),
        ReceiptState::Rejected
    );
}

#[test]
fn create_from_absence_and_second_create_conflicts() {
    let mut authority = authority();
    let create = put_request(1, "account", 0, "one");
    let first = run(&mut authority, &create);
    assert_eq!(first.state(), ReceiptState::Committed);
    assert_eq!(authority.key_version(&key("account")), KeyVersion::new(1));

    let second = put_request(2, "account", 0, "two");
    let outcome = run(&mut authority, &second);
    assert_eq!(outcome.state(), ReceiptState::Conflict);
    assert_eq!(
        outcome.receipt().current_key_version(),
        Some(KeyVersion::new(1))
    );
    assert_eq!(
        authority
            .record(&key("account"))
            .and_then(|record| record.as_value()),
        Some(b"one".as_slice())
    );
}

#[test]
fn delete_then_create_does_not_reuse_a_version() {
    let mut authority = authority();
    assert_eq!(
        run(&mut authority, &put_request(1, "account", 0, "one")).state(),
        ReceiptState::Committed
    );

    let delete = delete_request(2, "account", 1);
    assert_eq!(
        run(&mut authority, &delete).state(),
        ReceiptState::Committed
    );
    let tombstone = authority
        .record(&key("account"))
        .expect("the tombstone is present");
    assert!(tombstone.is_tombstone());
    assert_eq!(tombstone.key_version(), KeyVersion::new(2));

    let stale_create = put_request(3, "account", 1, "two");
    let stale = run(&mut authority, &stale_create);
    assert_eq!(stale.state(), ReceiptState::Conflict);
    assert_eq!(
        stale.receipt().current_key_version(),
        Some(KeyVersion::new(2))
    );

    let create = put_request(4, "account", 2, "two");
    let outcome = run(&mut authority, &create);
    assert_eq!(outcome.state(), ReceiptState::Committed);
    assert_eq!(authority.key_version(&key("account")), KeyVersion::new(3));
    assert_ne!(authority.key_version(&key("account")), KeyVersion::new(1));
}

#[test]
fn a_cross_shard_mutation_fails_before_the_version_check() {
    let mut authority = authority();
    let request = WriteRequest::new(
        RequestId::from_bytes([1; 32]),
        ClientId::new(vec![1]),
        IdempotencyKey::new(vec![1]),
        [
            Mutation::put(
                key("account"),
                shard(),
                KeyVersion::new(99),
                b"ahead".to_vec(),
            ),
            Mutation::put(
                key("other"),
                other_shard(),
                KeyVersion::new(0),
                b"cross".to_vec(),
            ),
        ],
    );

    let outcome = run(&mut authority, &request);

    assert_eq!(outcome.state(), ReceiptState::Rejected);
    assert_eq!(reason(&outcome), "shard.cross_mutation_rejected");
    assert!(authority.record(&key("account")).is_none());
    assert!(authority.record(&key("other")).is_none());
    assert_eq!(authority.commit_version().get(), 0);
}

#[test]
fn a_stale_base_with_a_policy_violation_returns_conflict() {
    let mut authority = authority();
    run(&mut authority, &put_request(1, "account", 0, "one"));

    let request = put_request(2, "account", 0, "bad");
    let outcome = authority
        .submit(&request, &PanicPolicy, &[&PanicHook])
        .expect("the conflict is an outcome");

    assert_eq!(outcome.state(), ReceiptState::Conflict);
    assert_eq!(reason(&outcome), "write.version_conflict");
    assert_eq!(
        outcome.receipt().current_key_version(),
        Some(KeyVersion::new(1))
    );
    assert_eq!(
        authority
            .record(&key("account"))
            .and_then(|record| record.as_value()),
        Some(b"one".as_slice())
    );
}

#[test]
fn a_committed_write_writes_one_audit_record() {
    let mut authority = authority();
    let request = put_request(1, "account", 0, "one");
    let outcome = run(&mut authority, &request);

    assert_eq!(outcome.state(), ReceiptState::Committed);
    assert_eq!(authority.audit_record_count(), 1);
    let record = &authority.audit_records()[0];
    assert_eq!(record.body().verdict, AuditVerdict::Committed);
    assert_eq!(record.body().reason, None);
    assert_eq!(record.body().receipt_id, outcome.receipt().receipt_id());
    assert_eq!(outcome.receipt().audit().record_hash, record.record_hash());
    assert_eq!(outcome.receipt().audit().audit_sequence, record.sequence());
    assert!(authority.verify_audit_chain());
}

#[test]
fn a_rejected_write_writes_one_receipt_and_one_audit_record() {
    let mut authority = authority();
    let request = put_request(1, "account", 0, "bad");
    let outcome = authority
        .submit(&request, &RejectPolicy, &[])
        .expect("the rejection returns an outcome");

    assert_eq!(outcome.state(), ReceiptState::Rejected);
    assert_eq!(authority.receipts().len(), 1);
    assert_eq!(authority.audit_record_count(), 1);
    let record = &authority.audit_records()[0];
    assert_eq!(record.body().verdict, AuditVerdict::Rejected);
    assert_eq!(record.body().reason, Some("policy.budget_exhausted"));
    assert_eq!(record.body().receipt_id, outcome.receipt().receipt_id());
    assert_eq!(outcome.receipt().audit().record_hash, record.record_hash());
    assert_eq!(outcome.receipt().audit().audit_sequence, record.sequence());
    assert!(authority.verify_audit_chain());
}

#[test]
fn a_missing_base_changes_no_data() {
    let mut authority = authority();
    let request = WriteRequest::new(
        RequestId::from_bytes([1; 32]),
        ClientId::new(vec![1]),
        IdempotencyKey::new(vec![1]),
        [Mutation::without_base(
            key("account"),
            shard(),
            WriteOperation::put(b"one".to_vec()),
        )],
    );

    let error = authority
        .submit(&request, &AllowAll, &[])
        .expect_err("the base is missing");

    assert_eq!(error.code(), "write.missing_base_version");
    assert_eq!(
        error,
        SubmitError::MissingBaseVersion {
            key: key("account")
        }
    );
    assert!(authority.record(&key("account")).is_none());
    assert_eq!(authority.commit_version().get(), 0);
    assert!(authority.receipts().is_empty());
    assert_eq!(authority.audit_record_count(), 0);
}

#[test]
fn a_batch_conflict_changes_no_key() {
    let mut authority = authority();
    run(&mut authority, &put_request(1, "alpha", 0, "a1"));
    run(&mut authority, &put_request(2, "beta", 0, "b1"));

    let batch = WriteRequest::new(
        RequestId::from_bytes([3; 32]),
        ClientId::new(vec![1]),
        IdempotencyKey::new(vec![3]),
        [
            Mutation::put(key("alpha"), shard(), KeyVersion::new(1), b"a2".to_vec()),
            Mutation::put(key("beta"), shard(), KeyVersion::new(0), b"b2".to_vec()),
        ],
    );
    let outcome = run(&mut authority, &batch);

    assert_eq!(outcome.state(), ReceiptState::Conflict);
    assert_eq!(
        outcome.receipt().current_key_version(),
        Some(KeyVersion::new(1))
    );
    assert_eq!(
        authority
            .record(&key("alpha"))
            .and_then(|record| record.as_value()),
        Some(b"a1".as_slice())
    );
    assert_eq!(
        authority
            .record(&key("beta"))
            .and_then(|record| record.as_value()),
        Some(b"b1".as_slice())
    );
    assert_eq!(authority.commit_version().get(), 2);
}

#[test]
fn a_rejected_write_changes_no_key_and_no_effect() {
    let mut authority = authority();
    run(&mut authority, &put_request(1, "account", 0, "one"));

    let request = put_request(2, "account", 1, "bad");
    let outcome = authority
        .submit(&request, &RejectPolicy, &[&PanicHook])
        .expect("the rejection is an outcome");

    assert_eq!(outcome.state(), ReceiptState::Rejected);
    assert_eq!(reason(&outcome), "policy.budget_exhausted");
    assert_eq!(outcome.effects(), []);
    assert_eq!(
        authority
            .record(&key("account"))
            .and_then(|record| record.as_value()),
        Some(b"one".as_slice())
    );
    assert_eq!(authority.commit_version().get(), 1);
}

#[test]
fn a_commit_derives_hook_effects() {
    let mut authority = authority();
    let request = put_request(1, "account", 0, "one");

    let outcome = authority
        .submit(&request, &AllowAll, &[&EffectHook])
        .expect("the create commits");

    assert_eq!(outcome.state(), ReceiptState::Committed);
    assert_eq!(outcome.effects().len(), 1);
    assert_eq!(outcome.effects()[0].source(), "index");
    assert_eq!(
        outcome.effects()[0].payload(),
        KeyVersion::new(1).get().to_le_bytes()
    );
}

#[test]
fn a_hook_failure_aborts_the_batch() {
    let mut authority = authority();
    let request = put_request(1, "account", 0, "one");

    let error = authority
        .submit(&request, &AllowAll, &[&FailHook])
        .expect_err("the hook fails");

    assert_eq!(error.code(), "audit.unavailable");
    assert!(authority.record(&key("account")).is_none());
    assert_eq!(authority.commit_version().get(), 0);
    assert!(authority.receipts().is_empty());
}

#[test]
fn a_terminal_receipt_is_immutable() {
    let mut authority = authority();
    let request = put_request(1, "account", 0, "one");
    let outcome = run(&mut authority, &request);
    let receipt = outcome.receipt().clone();

    let mut ledger = ReceiptLedger::new();
    ledger
        .insert_terminal(request.body_digest(), receipt.clone())
        .expect("the terminal receipt is new");
    let error = ledger
        .close(
            request.idempotency_key(),
            ReceiptOutcome::Rejected {
                reason: "write.queue_timeout",
            },
        )
        .expect_err("a terminal receipt cannot change");

    assert_eq!(error.code(), "receipt.state_invalid");
    assert_eq!(ledger.receipt(request.idempotency_key()), Some(&receipt));
}
