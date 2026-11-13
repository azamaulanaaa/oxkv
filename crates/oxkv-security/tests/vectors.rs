//! Golden vector runner for the security boundary.

mod support;

use oxkv_contracts::error::lookup;
use oxkv_contracts::receipt::{AuditSequence, ReceiptState};
use oxkv_security::audit::{AuditChain, AuditRecord, GENESIS_HASH, StructuralSigner};
use oxkv_security::error::SecurityError;
use oxkv_security::secret::KeyIdentifier;
use serde_json::Value;

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

fn load(text: &str) -> Value {
    serde_json::from_str(text).expect("the golden file is valid JSON")
}

#[test]
fn security_error_codes_are_pinned() {
    let file = load(include_str!("vectors/security.json"));
    assert_eq!(file["contract"].as_str(), Some("security/v1"));
    let cases = file["errors"]
        .as_array()
        .expect("the golden file has an error list");
    assert_eq!(cases.len(), SecurityError::ALL.len());
    for case in cases {
        let code = case["code"].as_str().expect("the case names a code");
        let error = SecurityError::ALL
            .iter()
            .copied()
            .find(|error| error.code() == code)
            .unwrap_or_else(|| panic!("the golden code {code} is unknown"));
        assert_eq!(
            case["receipt"].as_str(),
            error.receipt_state().map(ReceiptState::name)
        );
        let spec = lookup(error.code()).expect("the code is in the contract registry");
        assert_eq!(spec.receipt, error.receipt_state());
    }
}

#[test]
fn request_signature_golden_vector_is_pinned() {
    let file = load(include_str!("vectors/security.json"));
    let request = support::sample_request();
    let expected = &file["request"];
    assert_eq!(
        expected["body_digest_hex"].as_str(),
        Some(hex(request.body_digest().as_bytes()).as_str())
    );
    assert_eq!(
        expected["input_digest_hex"].as_str(),
        Some(hex(request.input_digest().as_bytes()).as_str())
    );
    assert_eq!(
        expected["signature_hex"].as_str(),
        Some(hex(request.signature().as_bytes()).as_str())
    );
}

#[test]
fn audit_record_golden_vector_is_pinned() {
    let file = load(include_str!("vectors/security.json"));
    let record = AuditRecord::new(AuditSequence::new(1), support::sample_entry(), GENESIS_HASH);
    assert_eq!(
        file["audit_record"]["record_hash_hex"].as_str(),
        Some(hex(record.record_hash.as_bytes()).as_str())
    );
}

#[test]
fn audit_checkpoint_golden_vector_is_pinned() {
    let file = load(include_str!("vectors/security.json"));
    let mut chain = AuditChain::new(oxkv_contracts::version::ShardId::new(vec![1]));
    chain
        .append(support::sample_entry())
        .expect("the sample record appends");
    let signer = StructuralSigner::new(KeyIdentifier::new("witness-key-1"));
    let checkpoint = chain.checkpoint(&signer).expect("the chain has a record");
    assert_eq!(
        file["checkpoint"]["checkpoint_hash_hex"].as_str(),
        Some(hex(checkpoint.checkpoint_hash.as_bytes()).as_str())
    );
    assert_eq!(
        file["checkpoint"]["signature_hex"].as_str(),
        Some(hex(checkpoint.signature.as_bytes()).as_str())
    );
}
