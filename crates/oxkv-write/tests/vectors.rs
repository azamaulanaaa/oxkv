//! Golden vectors for receipt envelopes and commit outcomes.
//!
//! A vector case is append-only for one crate version. A later change adds a
//! case. A released case does not change.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptEnvelope, RequestId};
use oxkv_contracts::version::{CommitVersion, KeyVersion, ManifestDigest, PolicyVersion, ShardId};
use oxkv_write::{
    AllowAll, Authority, Key, KeyRecord, Mutation, Policy, PolicyRow, PolicyVerdict, SubmitOutcome,
    WriteRequest,
};
use serde_json::{Value, json};

struct GoldenFile {
    contract: String,
    cases: Vec<GoldenCase>,
}

struct GoldenCase {
    name: String,
    expected: Value,
}

fn load(text: &str) -> GoldenFile {
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    let contract = value["contract"]
        .as_str()
        .expect("the golden file names a contract")
        .to_owned();
    let cases = value["cases"]
        .as_array()
        .expect("the golden file has a case list")
        .iter()
        .map(|case| GoldenCase {
            name: case["name"]
                .as_str()
                .expect("the case names a vector")
                .to_owned(),
            expected: case["expected"].clone(),
        })
        .collect();
    GoldenFile { contract, cases }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
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
    Authority::new(
        shard(),
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
    )
}

fn put_request(id: u8, name: &str, base: u64, value: &str) -> WriteRequest {
    WriteRequest::new(
        RequestId::from_bytes([id; 32]),
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
        IdempotencyKey::new(vec![id]),
        [Mutation::delete(key(name), shard(), KeyVersion::new(base))],
    )
}

fn receipt_json(receipt: &ReceiptEnvelope) -> Value {
    json!({
        "state": receipt.state().name(),
        "reason": receipt.reason_code(),
        "commit_version": receipt.commit_version().map(CommitVersion::get),
        "current_key_version": receipt.current_key_version().map(KeyVersion::get),
        "idempotency_key_hex": hex(receipt.idempotency_key().as_bytes()),
        "request_id_hex": hex(receipt.request_id().as_bytes()),
        "receipt_id_hex": hex(receipt.receipt_id().as_bytes()),
        "audit_sequence": receipt.audit().audit_sequence.get(),
        "policy_version_hex": hex(receipt.policy_version().as_bytes()),
        "manifest_digest_hex": hex(receipt.manifest_digest().as_bytes()),
    })
}

fn receipt_case(
    outcome: Option<&SubmitOutcome>,
    error: Option<&str>,
    authority: &Authority,
) -> Value {
    json!({
        "receipt": outcome.map(|value| receipt_json(value.receipt())),
        "duplicate": outcome.is_some_and(SubmitOutcome::is_duplicate),
        "error": error,
        "receipt_count": authority.receipts().len(),
        "key_version": authority.key_version(&key("account")).get(),
        "shard_commit_version": authority.commit_version().get(),
    })
}

struct RejectPolicy;

impl Policy for RejectPolicy {
    fn evaluate(&self, _request: &WriteRequest, _rows: &[PolicyRow<'_>]) -> PolicyVerdict {
        PolicyVerdict::Reject {
            reason: "policy.budget_exhausted",
        }
    }
}

fn receipt_scenario() -> BTreeMap<String, Value> {
    let mut authority = authority();
    let mut cases = BTreeMap::new();

    let create = put_request(1, "account", 0, "one");
    let outcome = authority
        .submit(&create, &AllowAll, &[])
        .expect("the create commits");
    cases.insert(
        "create".to_owned(),
        receipt_case(Some(&outcome), None, &authority),
    );

    let duplicate = authority
        .submit(&create, &AllowAll, &[])
        .expect("the retry returns the receipt");
    cases.insert(
        "duplicate".to_owned(),
        receipt_case(Some(&duplicate), None, &authority),
    );

    let conflict = put_request(2, "account", 0, "other");
    let outcome = authority
        .submit(&conflict, &AllowAll, &[])
        .expect("the conflict returns an outcome");
    cases.insert(
        "conflict".to_owned(),
        receipt_case(Some(&outcome), None, &authority),
    );

    let stale = put_request(3, "account", 9, "other");
    let outcome = authority
        .submit(&stale, &AllowAll, &[])
        .expect("the stale base returns an outcome");
    cases.insert(
        "stale".to_owned(),
        receipt_case(Some(&outcome), None, &authority),
    );

    let rejected = put_request(4, "account", 1, "bad");
    let outcome = authority
        .submit(&rejected, &RejectPolicy, &[])
        .expect("the rejection returns an outcome");
    cases.insert(
        "policy_reject".to_owned(),
        receipt_case(Some(&outcome), None, &authority),
    );

    let cross = WriteRequest::new(
        RequestId::from_bytes([5; 32]),
        IdempotencyKey::new(vec![5]),
        [Mutation::put(
            key("account"),
            other_shard(),
            KeyVersion::new(0),
            b"cross".to_vec(),
        )],
    );
    let outcome = authority
        .submit(&cross, &AllowAll, &[])
        .expect("the cross-shard write returns an outcome");
    cases.insert(
        "cross_shard".to_owned(),
        receipt_case(Some(&outcome), None, &authority),
    );

    let missing = WriteRequest::new(
        RequestId::from_bytes([6; 32]),
        IdempotencyKey::new(vec![6]),
        [Mutation::without_base(
            key("account"),
            shard(),
            oxkv_write::WriteOperation::put(b"one".to_vec()),
        )],
    );
    let error = authority
        .submit(&missing, &AllowAll, &[])
        .expect_err("the base is missing");
    cases.insert(
        "missing_base".to_owned(),
        receipt_case(None, Some(error.code()), &authority),
    );

    cases
}

fn commit_case(outcome: &SubmitOutcome, name: &str, authority: &Authority) -> Value {
    let record = authority.record(&key(name));
    json!({
        "state": outcome.state().name(),
        "reason": outcome.receipt().reason_code(),
        "duplicate": outcome.is_duplicate(),
        "commit_version": outcome.commit().map(|commit| commit.commit_version().get()),
        "key_version": authority.key_version(&key(name)).get(),
        "value_hex": record.and_then(|value| value.as_value()).map(hex),
        "tombstone": record.is_some_and(KeyRecord::is_tombstone),
        "receipt_count": authority.receipts().len(),
        "shard_commit_version": authority.commit_version().get(),
    })
}

fn commit_scenario() -> BTreeMap<String, Value> {
    let mut authority = authority();
    let mut cases = BTreeMap::new();

    let create = put_request(1, "account", 0, "one");
    let outcome = authority
        .submit(&create, &AllowAll, &[])
        .expect("the create commits");
    cases.insert(
        "create".to_owned(),
        commit_case(&outcome, "account", &authority),
    );

    let rename = put_request(2, "account", 1, "two");
    let outcome = authority
        .submit(&rename, &AllowAll, &[])
        .expect("the rename commits");
    cases.insert(
        "rename".to_owned(),
        commit_case(&outcome, "account", &authority),
    );

    let delete = delete_request(3, "account", 2);
    let outcome = authority
        .submit(&delete, &AllowAll, &[])
        .expect("the delete commits");
    cases.insert(
        "delete".to_owned(),
        commit_case(&outcome, "account", &authority),
    );

    let recreate = put_request(4, "account", 3, "three");
    let outcome = authority
        .submit(&recreate, &AllowAll, &[])
        .expect("the recreate commits");
    cases.insert(
        "recreate".to_owned(),
        commit_case(&outcome, "account", &authority),
    );

    let second_key = put_request(5, "beta", 0, "b");
    let outcome = authority
        .submit(&second_key, &AllowAll, &[])
        .expect("the second key commits");
    cases.insert(
        "create_second_key".to_owned(),
        commit_case(&outcome, "beta", &authority),
    );

    let conflict = put_request(6, "account", 2, "x");
    let outcome = authority
        .submit(&conflict, &AllowAll, &[])
        .expect("the conflict returns an outcome");
    cases.insert(
        "conflict".to_owned(),
        commit_case(&outcome, "account", &authority),
    );

    cases
}

#[test]
fn receipt_envelope_golden_vectors_are_pinned() {
    let file = load(include_str!("vectors/receipt-envelopes.json"));
    assert_eq!(file.contract, "write/receipt-envelope/v1");
    let actual = receipt_scenario();
    assert_eq!(file.cases.len(), actual.len());
    for case in &file.cases {
        let value = actual
            .get(&case.name)
            .unwrap_or_else(|| panic!("the golden case {} is unknown", case.name));
        assert_eq!(case.expected, *value, "the case {} changed", case.name);
    }
}

#[test]
fn commit_outcome_golden_vectors_are_pinned() {
    let file = load(include_str!("vectors/commit-outcomes.json"));
    assert_eq!(file.contract, "write/commit-outcome/v1");
    let actual = commit_scenario();
    assert_eq!(file.cases.len(), actual.len());
    for case in &file.cases {
        let value = actual
            .get(&case.name)
            .unwrap_or_else(|| panic!("the golden case {} is unknown", case.name));
        assert_eq!(case.expected, *value, "the case {} changed", case.name);
    }
}

#[test]
fn every_golden_file_names_a_contract() {
    for text in [
        include_str!("vectors/receipt-envelopes.json"),
        include_str!("vectors/commit-outcomes.json"),
    ] {
        let file = load(text);
        assert_ne!(file.contract, "");
        assert_ne!(file.cases.len(), 0);
    }
}
