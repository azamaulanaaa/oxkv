//! Tests for the receipt identity and the audit record fields.

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptState, RequestId};
use oxkv_contracts::version::{
    FactSourceId, FactVersion, Hash32, KeyVersion, ManifestDigest, MapVersion, PolicyVersion,
    ProjectionEpoch, ProjectionWatermarks, ShardId,
};
use oxkv_write::{
    AllowAll, Authority, AuthorityConfig, ClientId, DecisionVerdict, Key, Mutation, Policy,
    PolicyDecision, PolicyEvaluation, PolicyRow, RuleIdentity, SubmitOutcome, WriteRequest,
};

struct FixedPolicy;

impl Policy for FixedPolicy {
    fn evaluate(&self, _request: &WriteRequest, rows: &[PolicyRow<'_>]) -> PolicyEvaluation {
        let rule = RuleIdentity::new("rule.accounts.rename");
        let decisions = rows
            .iter()
            .map(|row| PolicyDecision::allow(row.key().clone(), rule.clone()))
            .collect();
        PolicyEvaluation::allow(rule, decisions)
    }
}

struct RejectPolicy;

impl Policy for RejectPolicy {
    fn evaluate(&self, _request: &WriteRequest, rows: &[PolicyRow<'_>]) -> PolicyEvaluation {
        let rule = RuleIdentity::new("rule.accounts.deny");
        let decisions = rows
            .iter()
            .map(|row| PolicyDecision::reject(row.key().clone(), rule.clone(), "policy.not_total"))
            .collect();
        PolicyEvaluation::reject(rule, "policy.not_total", decisions)
    }
}

fn shard() -> ShardId {
    ShardId::new(vec![1])
}

fn key(name: &str) -> Key {
    Key::new(name.as_bytes().to_vec())
}

fn fact_versions() -> ProjectionWatermarks {
    let mut versions = ProjectionWatermarks::new();
    versions.set(FactSourceId::new("accounts"), FactVersion::new(12));
    versions.set(FactSourceId::new("orders"), FactVersion::new(3));
    versions
}

fn new_authority() -> Authority {
    Authority::new(AuthorityConfig::new(
        shard(),
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
        MapVersion::new(9),
        ProjectionEpoch::new(4),
        fact_versions(),
    ))
}

fn write_request(request_id: u8, client: u8, idempotency: u8) -> WriteRequest {
    WriteRequest::new(
        RequestId::from_bytes([request_id; 32]),
        ClientId::new(vec![client]),
        IdempotencyKey::new(vec![idempotency]),
        [Mutation::put(
            key("account"),
            shard(),
            KeyVersion::new(0),
            b"one".to_vec(),
        )],
    )
}

fn run(authority: &mut Authority, request: &WriteRequest, policy: &dyn Policy) -> SubmitOutcome {
    authority
        .submit(request, policy, &[])
        .expect("the authority returns an outcome")
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

#[test]
fn the_receipt_id_covers_the_client_id_the_key_and_the_digest() {
    let request = write_request(0x11, 0xa1, 0xc3);
    let mut authority = new_authority();
    let outcome = run(&mut authority, &request, &AllowAll);

    // The golden value pins the client id, the idempotency key, and the
    // canonical request body digest.
    assert_eq!(
        hex(request.client_id().as_bytes()),
        "a1",
        "the client id input changed"
    );
    assert_eq!(
        hex(request.idempotency_key().as_bytes()),
        "c3",
        "the idempotency key input changed"
    );
    assert_eq!(
        hex(request.body_digest().as_bytes()),
        "fd41a9730db56a79db402b978c0bf09d5672f1840575f5bb983c9e1edddb5eeb",
        "the request digest input changed"
    );
    assert_eq!(
        hex(outcome.receipt().receipt_id().as_bytes()),
        "594ffdcf2c71739725f97a5e6a3e1f7a40ddfbadb47c4152e944a4c76ae6b366",
        "the receipt id changed"
    );

    let other_client = write_request(0x12, 0xa2, 0xc3);
    let mut authority = new_authority();
    let other = run(&mut authority, &other_client, &AllowAll);
    assert_ne!(
        other.receipt().receipt_id(),
        outcome.receipt().receipt_id(),
        "a new client id must change the receipt id"
    );

    let other_key = write_request(0x13, 0xa1, 0xc4);
    let mut authority = new_authority();
    let other = run(&mut authority, &other_key, &AllowAll);
    assert_ne!(
        other.receipt().receipt_id(),
        outcome.receipt().receipt_id(),
        "a new idempotency key must change the receipt id"
    );

    let other_body = WriteRequest::new(
        RequestId::from_bytes([0x14; 32]),
        ClientId::new(vec![0xa1]),
        IdempotencyKey::new(vec![0xc3]),
        [Mutation::put(
            key("account"),
            shard(),
            KeyVersion::new(0),
            b"two".to_vec(),
        )],
    );
    let mut authority = new_authority();
    let other = run(&mut authority, &other_body, &AllowAll);
    assert_ne!(
        other.receipt().receipt_id(),
        outcome.receipt().receipt_id(),
        "a new request digest must change the receipt id"
    );
}

#[test]
fn a_rejected_write_writes_one_receipt_and_one_audit_record() {
    let mut authority = new_authority();
    let request = write_request(0x21, 0x01, 0x21);
    let outcome = run(&mut authority, &request, &RejectPolicy);

    assert_eq!(outcome.state(), ReceiptState::Rejected);
    assert_eq!(authority.receipts().len(), 1);
    assert_eq!(authority.audit_record_count(), 1);

    let receipt = outcome.receipt();
    let record = &authority.audit_records()[0];
    assert_eq!(receipt.audit().shard_id, shard());
    assert_eq!(receipt.audit().audit_sequence, record.sequence());
    assert_eq!(receipt.audit().record_hash, record.record_hash());
    assert_eq!(record.body().receipt_id, receipt.receipt_id());
    assert_eq!(record.body().verdict.name(), "Rejected");
    assert_eq!(record.body().reason, Some("policy.not_total"));
    assert!(record.verify_hash());
    assert!(authority.verify_audit_chain());
}

#[test]
fn a_retry_with_a_new_request_id_returns_the_original_receipt() {
    let mut authority = new_authority();
    let first = write_request(0x31, 0x01, 0x31);
    let original = run(&mut authority, &first, &AllowAll);

    // The retry changes the request id. The client id, the idempotency key,
    // and the body stay the same.
    let retry = write_request(0x32, 0x01, 0x31);
    let duplicate = run(&mut authority, &retry, &AllowAll);

    assert!(duplicate.is_duplicate());
    assert_eq!(duplicate.receipt(), original.receipt());
    assert_eq!(
        duplicate.receipt().receipt_id(),
        original.receipt().receipt_id()
    );
    assert_eq!(
        duplicate.receipt().audit().record_hash,
        original.receipt().audit().record_hash
    );
    assert_eq!(authority.receipts().len(), 1);
    assert_eq!(authority.audit_record_count(), 1);
}

#[test]
fn the_audit_record_carries_every_section_7_19_field() {
    let mut authority = new_authority();
    let request = write_request(0x41, 0x07, 0x41);
    let outcome = run(&mut authority, &request, &FixedPolicy);

    let record = &authority.audit_records()[0];
    let body = record.body();
    assert_eq!(body.request_id, request.request_id());
    assert_eq!(&body.idempotency_key, request.idempotency_key());
    assert_eq!(&body.principal, request.client_id());
    assert_eq!(body.shard_id, shard());
    assert_eq!(
        body.base_versions,
        [(key("account"), Some(KeyVersion::new(0)))]
    );
    assert_eq!(body.policy_version, PolicyVersion::from_bytes([4; 32]));
    assert_eq!(body.manifest_digest, ManifestDigest::from_bytes([5; 32]));
    assert_eq!(body.map_version, MapVersion::new(9));
    assert_eq!(body.projection_epoch, ProjectionEpoch::new(4));
    assert_eq!(
        body.fact_versions.get(&FactSourceId::new("accounts")),
        Some(FactVersion::new(12))
    );
    assert_eq!(
        body.fact_versions.get(&FactSourceId::new("orders")),
        Some(FactVersion::new(3))
    );
    assert_eq!(body.rule_identity.as_str(), "rule.accounts.rename");
    assert_eq!(body.decisions.len(), 1);
    assert_eq!(body.decisions[0].key(), &key("account"));
    assert_eq!(body.decisions[0].rule().as_str(), "rule.accounts.rename");
    assert_eq!(body.decisions[0].verdict(), DecisionVerdict::Allow);
    assert_eq!(body.decisions[0].reason(), None);
    assert_eq!(body.receipt_id, outcome.receipt().receipt_id());
    assert_eq!(body.verdict.name(), "Committed");
    assert_eq!(body.reason, None);
    assert_eq!(body.input_digest, request.body_digest());
    assert_eq!(record.previous_hash(), Hash32::from_bytes([0; 32]));
    assert_eq!(record.record_hash(), outcome.receipt().audit().record_hash);
    assert!(record.verify_hash());
}
