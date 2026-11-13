//! Golden vectors for the business event envelope and event identity.
//!
//! The vector pins the canonical bytes and the event id. A reader on native
//! and `wasm32` must reproduce the same values.

use oxkv_event::{
    CommitVersion, CommittedAt, ContentDigest, DEFAULT_PAYLOAD_BOUND, EffectField, EmissionIndex,
    EventEnvelope, EventEnvelopeParts, EventPayload, EventSequence, ObjectReference, ObjectVersion,
    OriginRequestId, PayloadSchema, PayloadValue, PolicyVersion, RetentionClass, RuleId, ShardId,
    TableName,
};
use serde_json::Value;

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

fn expected_str<'a>(case: &'a Value, field: &str) -> &'a str {
    case[field]
        .as_str()
        .unwrap_or_else(|| panic!("the case has no {field} field"))
}

fn expected_u64(case: &Value, field: &str) -> u64 {
    case[field]
        .as_u64()
        .unwrap_or_else(|| panic!("the case has no {field} field"))
}

fn sample_envelope() -> EventEnvelope {
    EventEnvelope::new(EventEnvelopeParts {
        shard_id: ShardId::new(vec![1, 2]),
        table: TableName::parse_user("accounts").expect("the table name is valid"),
        key: b"accounts/42".to_vec(),
        key_sequence: EventSequence::new(1),
        commit_version: CommitVersion::new(18),
        committed_at: CommittedAt::new(1_760_000_000),
        rule_id: RuleId::new("welcome_mail"),
        policy_version: PolicyVersion::from_bytes([4; 32]),
        emission_index: EmissionIndex::new(0),
        payload: EventPayload::new()
            .with_field("recipient", PayloadValue::Text("a@example.com".to_owned()))
            .with_field(
                "invoice",
                PayloadValue::Reference(ObjectReference::new(
                    "invoice-42",
                    ContentDigest::from_bytes([7; 32]),
                    ObjectVersion::new(2),
                )),
            ),
        payload_schema: PayloadSchema::new("welcome_mail/v1"),
        origin_request_id: OriginRequestId::new(vec![9, 9]),
        required_effects: vec![EffectField::new("recipient")],
        retention: RetentionClass::time_and_bytes(3600, 1024),
        payload_bound: DEFAULT_PAYLOAD_BOUND,
    })
    .expect("the envelope is valid")
}

#[test]
fn the_event_golden_vector_is_pinned() {
    let file: Value = serde_json::from_str(include_str!("vectors/event-envelope.json"))
        .expect("the vector parses");
    assert_eq!(expected_str(&file, "contract"), "event/v1");
    let cases = file["cases"].as_array().expect("the cases are a list");
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    let envelope = sample_envelope();
    assert_eq!(expected_str(case, "name"), "welcome_mail");
    assert_eq!(
        expected_str(case, "envelope_hex"),
        hex(&envelope.encode()),
        "the canonical envelope bytes changed"
    );
    assert_eq!(
        expected_str(case, "event_id_hex"),
        envelope.event_id().to_hex(),
        "the event id changed"
    );
    assert_eq!(
        expected_str(case, "envelope_version"),
        envelope.envelope_version().to_string()
    );
    assert_eq!(
        expected_u64(case, "key_sequence"),
        envelope.key_sequence().get()
    );
    assert_eq!(
        expected_u64(case, "commit_version"),
        envelope.commit_version().get()
    );
    assert_eq!(
        expected_u64(case, "committed_at"),
        envelope.committed_at().get()
    );
    assert_eq!(expected_str(case, "rule_id"), envelope.rule_id().as_str());
    assert_eq!(
        expected_str(case, "policy_version_hex"),
        hex(envelope.policy_version().as_bytes())
    );
    assert_eq!(
        expected_u64(case, "emission_index"),
        envelope.emission_index().get()
    );
    assert_eq!(
        expected_str(case, "payload_schema"),
        envelope.payload_schema().as_str()
    );
    assert_eq!(
        expected_str(case, "origin_request_id_hex"),
        hex(envelope.origin_request_id().as_bytes())
    );
    assert_eq!(
        expected_u64(case, "retention_seconds"),
        envelope.retention().time_bound().expect("the time bound")
    );
    assert_eq!(
        expected_u64(case, "retention_bytes"),
        envelope.retention().byte_bound().expect("the byte bound")
    );
    assert_eq!(
        expected_u64(case, "payload_bound"),
        u64::try_from(envelope.payload_bound().max_bytes()).expect("the bound fits")
    );
}

#[test]
fn the_golden_envelope_still_decodes() {
    let file: Value = serde_json::from_str(include_str!("vectors/event-envelope.json"))
        .expect("the vector parses");
    let case = &file["cases"][0];
    let bytes = hex_decode(expected_str(case, "envelope_hex"));
    let envelope = EventEnvelope::decode(&bytes).expect("the golden bytes decode");
    assert_eq!(
        envelope.event_id().to_hex(),
        expected_str(case, "event_id_hex")
    );
    assert_eq!(envelope.encode(), bytes);
}

fn hex_decode(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let raw = text.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        let high = raw[index];
        let low = raw[index + 1];
        bytes.push((hex_value(high) << 4) | hex_value(low));
        index += 2;
    }
    bytes
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => panic!("the hex text is invalid"),
    }
}
