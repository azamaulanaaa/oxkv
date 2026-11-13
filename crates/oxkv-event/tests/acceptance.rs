//! Acceptance tests for the business event stream and the effect consumer.

use oxkv_event::{
    BusinessEvent, CommittedAt, CommittedWrite, Consumer, ConsumerCursor, Delivery, EffectField,
    EffectHandler, EmissionIndex, EventDraft, EventEnvelope, EventEnvelopeParts, EventError,
    EventLog, EventPayload, EventSequence, HandlerOutcome, ObjectReference, ObjectVersion,
    OperationalFeed, OperationalPayload, OperationalSignalKind, OriginRequestId, PayloadSchema,
    PayloadValue, PolicyVersion, RetentionClass, RetentionFloor, RuleId, ShardId, SignalSchema,
    TableName, WriteOutcome, events_for_write,
};
use oxkv_event::{CommitVersion, ContentDigest};

fn sample_payload() -> EventPayload {
    EventPayload::new().with_field("recipient", PayloadValue::Text("a@example.com".to_owned()))
}

fn event(sequence: u64, key: &[u8]) -> EventEnvelope {
    EventEnvelope::new(EventEnvelopeParts {
        shard_id: ShardId::new(vec![1]),
        table: TableName::parse_user("accounts").expect("the table name is valid"),
        key: key.to_vec(),
        key_sequence: EventSequence::new(sequence),
        commit_version: CommitVersion::new(sequence),
        committed_at: CommittedAt::new(1_760_000_000 + sequence),
        rule_id: RuleId::new("welcome_mail"),
        policy_version: PolicyVersion::from_bytes([4; 32]),
        emission_index: EmissionIndex::new(0),
        payload: sample_payload(),
        payload_schema: PayloadSchema::new("welcome_mail/v1"),
        origin_request_id: OriginRequestId::new(vec![9, 9]),
        required_effects: vec![EffectField::new("recipient")],
        retention: RetentionClass::time_and_bytes(3600, 1024),
        payload_bound: oxkv_event::DEFAULT_PAYLOAD_BOUND,
    })
    .expect("the event is valid")
}

fn draft() -> EventDraft {
    EventDraft::new(
        EventSequence::new(1),
        RuleId::new("welcome_mail"),
        PayloadSchema::new("welcome_mail/v1"),
    )
    .with_payload(sample_payload())
    .with_required_effects(vec![EffectField::new("recipient")])
}

fn commit() -> CommittedWrite {
    CommittedWrite::new(
        ShardId::new(vec![1]),
        TableName::parse_user("accounts").expect("the table name is valid"),
        b"accounts/42".to_vec(),
        CommitVersion::new(18),
        CommittedAt::new(1_760_000_000),
        PolicyVersion::from_bytes([4; 32]),
        OriginRequestId::new(vec![9, 9]),
    )
}

struct PoisonKey {
    key: Vec<u8>,
}

impl EffectHandler for PoisonKey {
    fn handle(&mut self, delivery: &Delivery) -> HandlerOutcome {
        if delivery.envelope().key() == self.key.as_slice() {
            HandlerOutcome::Poison
        } else {
            HandlerOutcome::Ack
        }
    }
}

#[test]
fn one_committed_write_emits_one_event_with_a_stable_id() {
    let outcome = WriteOutcome::Committed(commit());
    let first = events_for_write(&outcome, &[draft()]).expect("the event is valid");
    let second = events_for_write(&outcome, &[draft()]).expect("the event is valid");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].event_id(), second[0].event_id());
    assert_eq!(first[0].encode(), second[0].encode());
}

#[test]
fn a_rejected_or_conflict_write_emits_no_business_event() {
    let rejected = events_for_write(
        &WriteOutcome::Rejected {
            reason: "write.stale_base_version",
        },
        &[draft()],
    )
    .expect("the call succeeds");
    assert_eq!(rejected, Vec::<BusinessEvent>::new());
    let conflict = events_for_write(
        &WriteOutcome::Conflict {
            current_key_version: oxkv_event::KeyVersion::new(7),
        },
        &[draft()],
    )
    .expect("the call succeeds");
    assert_eq!(conflict, Vec::<BusinessEvent>::new());
}

#[test]
fn a_poison_key_does_not_block_another_key() {
    let mut log = EventLog::new();
    log.append(event(1, b"a"));
    log.append(event(1, b"b"));
    let mut consumer = Consumer::new(log);
    let mut handler = PoisonKey { key: b"a".to_vec() };
    let first = consumer
        .deliver_once(&mut handler, 0)
        .expect_err("key a is poison");
    assert_eq!(first.code(), "consumer.dead_lettered");
    assert_eq!(consumer.cursor().sequence_for(b"a"), EventSequence::new(1));
    let second = consumer
        .deliver_once(&mut handler, 0)
        .expect("key b is delivered")
        .expect("one delivery");
    assert_eq!(second.envelope().key(), b"b");
    assert_eq!(consumer.cursor().sequence_for(b"b"), EventSequence::new(1));
    assert_eq!(consumer.cursor().sequence_for(b"a"), EventSequence::new(1));
}

#[test]
fn one_unacknowledged_event_blocks_only_its_key() {
    let mut log = EventLog::new();
    log.append(event(1, b"a"));
    log.append(event(2, b"a"));
    log.append(event(1, b"b"));
    log.append(event(2, b"b"));
    let mut consumer = Consumer::new(log);
    let first = consumer.poll(0, 10).expect("the poll succeeds");
    assert_eq!(first.len(), 2);
    let key_b = first
        .iter()
        .find(|delivery| delivery.envelope().key() == b"b")
        .expect("key b is in flight");
    consumer
        .settle(key_b, HandlerOutcome::Ack, 1)
        .expect("key b is acknowledged");
    let second = consumer.poll(0, 10).expect("the poll succeeds");
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].envelope().key(), b"b");
    assert_eq!(second[0].envelope().key_sequence(), EventSequence::new(2));
    assert_eq!(consumer.cursor().sequence_for(b"a"), EventSequence::new(0));
    assert_eq!(consumer.cursor().sequence_for(b"b"), EventSequence::new(1));
}

#[test]
fn a_cursor_resumes_without_a_skip() {
    let mut log = EventLog::new();
    log.append(event(1, b"a"));
    log.append(event(2, b"a"));
    log.append(event(3, b"a"));
    let mut first = Consumer::new(log.clone());
    let delivery = first
        .poll(0, 1)
        .expect("the poll succeeds")
        .pop()
        .expect("one delivery");
    first
        .settle(&delivery, HandlerOutcome::Ack, 1)
        .expect("the ack succeeds");
    let mut resumed = Consumer::new(log).with_cursor(first.cursor().clone());
    let second = resumed
        .poll(0, 1)
        .expect("the poll succeeds")
        .pop()
        .expect("one delivery");
    assert_eq!(second.envelope().key_sequence(), EventSequence::new(2));
    resumed
        .settle(&second, HandlerOutcome::Ack, 2)
        .expect("the ack succeeds");
    let third = resumed
        .poll(0, 1)
        .expect("the poll succeeds")
        .pop()
        .expect("one delivery");
    assert_eq!(third.envelope().key_sequence(), EventSequence::new(3));
}

#[test]
fn a_cursor_behind_retention_returns_the_oldest_position() {
    let mut log = EventLog::new();
    log.append(event(1, b"a"));
    log.append(event(2, b"a"));
    let floor = RetentionFloor::new(oxkv_event::EventPosition::Compact(EventSequence::new(2)));
    log.set_retention_floor(floor.clone());
    let error = log
        .read_from(&ConsumerCursor::compact(EventSequence::new(0)), 10)
        .expect_err("the cursor is behind retention");
    assert_eq!(
        error,
        EventError::RetentionGap {
            oldest: floor.position().clone()
        }
    );
    assert_eq!(error.code(), "event.retention_gap");
}

#[test]
fn the_completion_cursor_can_expire() {
    let cursor = ConsumerCursor::compact(EventSequence::new(1));
    let oldest = oxkv_event::EventPosition::Compact(EventSequence::new(2));
    let error = cursor
        .ensure_completion(&oldest)
        .expect_err("the cursor is expired");
    assert_eq!(error.code(), "consumer.cursor_expired");
}

#[test]
fn required_fields_build_from_the_payload() {
    let parts = EventEnvelope::new(EventEnvelopeParts {
        shard_id: ShardId::new(vec![1]),
        table: TableName::parse_user("accounts").expect("the table name is valid"),
        key: b"accounts/42".to_vec(),
        key_sequence: EventSequence::new(1),
        commit_version: CommitVersion::new(18),
        committed_at: CommittedAt::new(1_760_000_000),
        rule_id: RuleId::new("invoice_mail"),
        policy_version: PolicyVersion::from_bytes([4; 32]),
        emission_index: EmissionIndex::new(0),
        payload: sample_payload().with_field(
            "invoice",
            PayloadValue::Reference(ObjectReference::new(
                "invoice-42",
                ContentDigest::from_bytes([7; 32]),
                ObjectVersion::new(2),
            )),
        ),
        payload_schema: PayloadSchema::new("invoice_mail/v1"),
        origin_request_id: OriginRequestId::new(vec![9, 9]),
        required_effects: vec![EffectField::new("recipient")],
        retention: RetentionClass::time(3600),
        payload_bound: oxkv_event::DEFAULT_PAYLOAD_BOUND,
    })
    .expect("the envelope is valid");
    let value = parts
        .effect_value(&EffectField::new("recipient"))
        .expect("the required field is present");
    assert_eq!(value, &PayloadValue::Text("a@example.com".to_owned()));
    assert!(matches!(
        parts.payload().get("invoice"),
        Some(PayloadValue::Reference(_))
    ));
}

#[test]
fn an_optional_reference_does_not_block_the_effect() {
    let envelope = EventEnvelope::new(EventEnvelopeParts {
        shard_id: ShardId::new(vec![1]),
        table: TableName::parse_user("accounts").expect("the table name is valid"),
        key: b"accounts/42".to_vec(),
        key_sequence: EventSequence::new(1),
        commit_version: CommitVersion::new(18),
        committed_at: CommittedAt::new(1_760_000_000),
        rule_id: RuleId::new("invoice_mail"),
        policy_version: PolicyVersion::from_bytes([4; 32]),
        emission_index: EmissionIndex::new(0),
        payload: sample_payload().with_field(
            "enrichment",
            PayloadValue::Reference(ObjectReference::new(
                "report-42",
                ContentDigest::from_bytes([8; 32]),
                ObjectVersion::new(3),
            )),
        ),
        payload_schema: PayloadSchema::new("invoice_mail/v1"),
        origin_request_id: OriginRequestId::new(vec![9, 9]),
        required_effects: vec![EffectField::new("recipient")],
        retention: RetentionClass::time(3600),
        payload_bound: oxkv_event::DEFAULT_PAYLOAD_BOUND,
    })
    .expect("the envelope is valid");
    assert!(
        envelope
            .effect_value(&EffectField::new("recipient"))
            .is_ok()
    );
    assert!(matches!(
        envelope.payload().get("enrichment"),
        Some(PayloadValue::Reference(_))
    ));
}

#[test]
fn the_dead_letter_record_has_no_secret_payload() {
    let secret = "super-secret-token";
    let mut log = EventLog::new();
    let envelope = EventEnvelope::new(EventEnvelopeParts {
        shard_id: ShardId::new(vec![1]),
        table: TableName::parse_user("accounts").expect("the table name is valid"),
        key: b"a".to_vec(),
        key_sequence: EventSequence::new(1),
        commit_version: CommitVersion::new(1),
        committed_at: CommittedAt::new(1_760_000_001),
        rule_id: RuleId::new("welcome_mail"),
        policy_version: PolicyVersion::from_bytes([4; 32]),
        emission_index: EmissionIndex::new(0),
        payload: sample_payload().with_field("secret", PayloadValue::Text(secret.to_owned())),
        payload_schema: PayloadSchema::new("welcome_mail/v1"),
        origin_request_id: OriginRequestId::new(vec![7, 7]),
        required_effects: vec![EffectField::new("recipient")],
        retention: RetentionClass::time(3600),
        payload_bound: oxkv_event::DEFAULT_PAYLOAD_BOUND,
    })
    .expect("the envelope is valid");
    log.append(envelope);
    let mut consumer = Consumer::new(log);
    let mut handler = PoisonKey { key: b"a".to_vec() };
    consumer
        .deliver_once(&mut handler, 0)
        .expect_err("the event is poison");
    let record = &consumer.dead_letters()[0];
    let encoded = record.encode();
    assert!(!String::from_utf8_lossy(&encoded).contains(secret));
    assert_eq!(record.attempts(), 1);
    assert_eq!(record.reason(), "event.poison");
}

#[test]
fn audit_export_uses_the_operational_feed() {
    let mut feed = OperationalFeed::new(RetentionClass::time(3600));
    let signal = feed.append(
        OperationalSignalKind::AuditExport,
        ShardId::new(vec![1]),
        CommittedAt::new(1_760_000_000),
        SignalSchema::new("audit-export/v1"),
        OperationalPayload::new().with_field("export_id", "exp-1"),
    );
    assert_eq!(signal.kind(), OperationalSignalKind::AuditExport);
    assert_eq!(signal.kind().name(), "audit_export");
    assert_eq!(feed.len(), 1);
    assert_eq!(feed.retention(), RetentionClass::time(3600));
}

#[test]
fn a_budget_signal_is_not_a_business_event() {
    let mut feed = OperationalFeed::new(RetentionClass::bytes(4096));
    let signal = feed.append(
        OperationalSignalKind::BudgetSignal,
        ShardId::new(vec![1]),
        CommittedAt::new(1_760_000_000),
        SignalSchema::new("budget/v1"),
        OperationalPayload::new().with_field("budget", "policy_step_budget"),
    );
    assert!(!signal.kind().is_business_side_effect());
    assert_eq!(signal.kind().name(), "budget_signal");
}
