//! Acceptance tests for versioned writes.

use oxkv_contracts::receipt::{
    AuditSequence, IdempotencyKey, ReceiptOutcome, ReceiptState, RequestId,
};
use oxkv_contracts::version::{
    KeyVersion, ManifestDigest, MapVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
    ShardId,
};
use oxkv_security::secret::KeyIdentifier;
use oxkv_write::{
    AllowAll, AuditChain, AuditVerdict, Authority, AuthorityConfig, ClientId, CommitHook,
    CommitView, HookEffect, HookError, Key, Mutation, Policy, PolicyDecision, PolicyEvaluation,
    PolicyRow, ReceiptLedger, RuleIdentity, SubmitError, SubmitOutcome, WriteOperation,
    WriteRequest, rebase_request,
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
    Authority::local(AuthorityConfig::new(
        shard(),
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
        MapVersion::new(6),
        ProjectionEpoch::new(7),
        ProjectionWatermarks::new(),
        KeyIdentifier::new("checkpoint-key-1"),
    ))
    .expect("the local authority starts")
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

fn put_request_for_client(
    request_id: u8,
    client: u8,
    idempotency: u8,
    name: &str,
    base: u64,
    value: &str,
) -> WriteRequest {
    WriteRequest::new(
        RequestId::from_bytes([request_id; 32]),
        ClientId::new(vec![client]),
        IdempotencyKey::new(vec![idempotency]),
        [Mutation::put(
            key(name),
            shard(),
            KeyVersion::new(base),
            value.as_bytes().to_vec(),
        )],
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
            .receipt(second.client_id(), second.idempotency_key())
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
            .receipt(request.client_id(), request.idempotency_key())
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
    assert_eq!(record.entry.decision, AuditVerdict::Committed);
    assert_eq!(record.entry.reason_code, None);
    assert_eq!(
        record.entry.receipt_id,
        Some(outcome.receipt().receipt_id())
    );
    assert_eq!(outcome.receipt().audit().record_hash, record.record_hash);
    assert_eq!(
        outcome.receipt().audit().audit_sequence,
        record.audit_sequence
    );
    assert!(authority.verify_audit_chain().is_ok());
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
    assert_eq!(record.entry.decision, AuditVerdict::Rejected);
    assert_eq!(record.entry.reason_code, Some("policy.budget_exhausted"));
    assert_eq!(
        record.entry.receipt_id,
        Some(outcome.receipt().receipt_id())
    );
    assert_eq!(outcome.receipt().audit().record_hash, record.record_hash);
    assert_eq!(
        outcome.receipt().audit().audit_sequence,
        record.audit_sequence
    );
    assert!(authority.verify_audit_chain().is_ok());
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
        .insert_terminal(request.client_id(), request.body_digest(), receipt.clone())
        .expect("the terminal receipt is new");
    let error = ledger
        .close(
            request.client_id(),
            request.idempotency_key(),
            ReceiptOutcome::Rejected {
                reason: "write.queue_timeout",
            },
        )
        .expect_err("a terminal receipt cannot change");

    assert_eq!(error.code(), "receipt.state_invalid");
    assert_eq!(
        ledger.receipt(request.client_id(), request.idempotency_key()),
        Some(&receipt)
    );
}

#[test]
fn two_clients_with_the_same_key_and_body_get_different_receipts() {
    let mut authority = authority();
    let client_a = put_request_for_client(0x11, 0xa1, 0xc3, "account", 0, "one");
    let client_b = put_request_for_client(0x12, 0xa2, 0xc3, "account", 0, "one");
    assert_eq!(client_a.body_digest(), client_b.body_digest());

    let first = run(&mut authority, &client_a);
    let second = run(&mut authority, &client_b);

    assert_eq!(first.state(), ReceiptState::Committed);
    assert_eq!(second.state(), ReceiptState::Conflict);
    assert!(!second.is_duplicate());
    assert_ne!(first.receipt().receipt_id(), second.receipt().receipt_id());
    assert_eq!(authority.receipts().len(), 2);

    let retry_a = run(&mut authority, &client_a);
    assert!(retry_a.is_duplicate());
    assert_eq!(retry_a.receipt(), first.receipt());

    let retry_b = run(&mut authority, &client_b);
    assert!(retry_b.is_duplicate());
    assert_eq!(retry_b.receipt(), second.receipt());
    assert_eq!(authority.receipts().len(), 2);
}

#[test]
fn the_receipt_ledger_does_not_leak_one_client_receipt_to_another() {
    let mut authority = authority();
    let client_a = put_request_for_client(0x21, 0xb1, 0xd4, "account", 0, "one");
    let client_b = put_request_for_client(0x22, 0xb2, 0xd4, "account", 0, "one");
    let first = run(&mut authority, &client_a);
    let second = run(&mut authority, &client_b);

    let stored_a = authority
        .receipt(client_a.client_id(), client_a.idempotency_key())
        .expect("client A has a receipt");
    let stored_b = authority
        .receipt(client_b.client_id(), client_b.idempotency_key())
        .expect("client B has a receipt");

    assert_eq!(stored_a, first.receipt());
    assert_eq!(stored_b, second.receipt());
    assert_ne!(stored_a, stored_b);
    assert!(
        authority
            .receipts()
            .contains(client_a.client_id(), client_a.idempotency_key())
    );
    assert!(
        authority
            .receipts()
            .contains(client_b.client_id(), client_b.idempotency_key())
    );
    assert_eq!(authority.receipts().len(), 2);
}

#[test]
fn the_audit_chain_verifies_to_the_last_signed_checkpoint() {
    let mut authority = authority();
    let create = run(&mut authority, &put_request(1, "account", 0, "one"));
    let conflict = run(&mut authority, &put_request(2, "account", 0, "two"));
    let rejected = authority
        .submit(&put_request(3, "account", 1, "bad"), &RejectPolicy, &[])
        .expect("the rejection returns an outcome");

    assert_eq!(create.state(), ReceiptState::Committed);
    assert_eq!(conflict.state(), ReceiptState::Conflict);
    assert_eq!(rejected.state(), ReceiptState::Rejected);

    let report = authority
        .verify_audit_chain()
        .expect("the chain verifies to the last checkpoint");
    assert_eq!(report.records, 3);
    assert_eq!(report.checkpoints, 3);
    assert_eq!(report.last_checkpoint, Some(AuditSequence::new(3)));
    assert_eq!(report.tip, Some(authority.audit_records()[2].record_hash));

    let last = authority
        .last_audit_checkpoint()
        .expect("the chain has a checkpoint");
    assert_eq!(last.audit_sequence, AuditSequence::new(3));
    assert_eq!(last.record_hash, report.tip.expect("the chain has a tip"));
    assert_eq!(last.key_id.as_str(), "checkpoint-key-1");

    for (checkpoint, record) in authority
        .audit_checkpoints()
        .iter()
        .zip(authority.audit_records())
    {
        assert_eq!(checkpoint.audit_sequence, record.audit_sequence);
        assert_eq!(checkpoint.record_hash, record.record_hash);
    }

    let mut records = authority.audit_records().to_vec();
    records[0].entry.reason_code = Some("audit.unavailable");
    let changed = AuditChain::from_parts(shard(), records, authority.audit_checkpoints().to_vec());
    assert!(changed.verify().is_err());
}

#[test]
fn the_witness_receives_the_checkpoint_after_commit() {
    let mut authority = authority();
    let outcome = run(&mut authority, &put_request(1, "account", 0, "one"));

    assert_eq!(outcome.state(), ReceiptState::Committed);
    assert_eq!(authority.witness().delivered_len(), 0);
    assert_eq!(authority.witness().pending_len(), 1);

    let record = &authority.audit_records()[0];
    let pending = authority
        .witness()
        .pending()
        .front()
        .expect("the witness has one checkpoint");
    assert_eq!(pending.audit_sequence, record.audit_sequence);
    assert_eq!(pending.record_hash, record.record_hash);

    let delivered = authority.witness_mut().deliver_all();
    assert_eq!(delivered.len(), 1);
    assert_eq!(authority.witness().pending_len(), 0);
    assert_eq!(authority.witness().delivered_len(), 1);
    assert_eq!(
        delivered[0].checkpoint_hash,
        authority
            .last_audit_checkpoint()
            .expect("the chain has a checkpoint")
            .checkpoint_hash
    );
}

#[test]
fn the_commit_path_does_not_block_on_the_witness() {
    let mut authority = authority();
    let mut terminal_verdicts = 0_usize;
    for id in 1..=3 {
        let outcome = run(&mut authority, &put_request(id, "account", 0, "one"));
        assert!(outcome.state().is_terminal());
        terminal_verdicts += 1;
        assert_eq!(authority.witness().delivered_len(), 0);
    }

    assert_eq!(terminal_verdicts, 3);
    assert_eq!(authority.witness().pending_len(), terminal_verdicts);
    assert_eq!(authority.witness().delivered_len(), 0);
}
