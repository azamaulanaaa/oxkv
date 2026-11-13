//! Scenario tests for the reference effect consumer.

mod support;

use oxkv_consumer::handler::Deadline;
use oxkv_consumer::{
    DeadLetterReason, EffectConsumer, EventSequence, HandlerOutcome, ProviderIdempotencyKey,
};

fn config() -> oxkv_consumer::ConsumerConfig {
    support::config(3, 100, 1_000, 10_000, 8)
}

#[test]
fn poison_key_a_does_not_block_key_b() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);

    let poison = support::poison_event(b"a", 1);
    let first = consumer
        .deliver(&poison, 1_000, &mut handler)
        .expect("the poison event dead-letters");
    assert_eq!(first.name(), "dead_lettered");
    assert_eq!(consumer.dead_letter_depth(), 1);
    assert_eq!(consumer.dead_letters()[0].event.key.as_slice(), b"a");
    assert_eq!(
        consumer.dead_letters()[0].reason,
        DeadLetterReason::UnsupportedSchema
    );

    let second = consumer
        .deliver(&support::event(b"b", 1), 1_000, &mut handler)
        .expect("the other key is not blocked");
    assert_eq!(second.name(), "acked");
    assert_eq!(handler.successes().len(), 1);
    assert_eq!(handler.successes()[0].0.as_slice(), b"b");
    assert_eq!(consumer.cursor(b"a"), EventSequence::new(1));
    assert_eq!(consumer.cursor(b"b"), EventSequence::new(1));
}

#[test]
fn a_duplicate_poison_delivery_writes_one_record() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);
    let event = support::poison_event(b"a", 1);

    assert_eq!(
        consumer
            .deliver(&event, 1_000, &mut handler)
            .expect("the poison event dead-letters")
            .name(),
        "dead_lettered"
    );
    assert_eq!(
        consumer
            .deliver(&event, 1_000, &mut handler)
            .expect("the duplicate is resolved")
            .name(),
        "duplicate"
    );
    assert_eq!(consumer.dead_letter_depth(), 1);
    assert_eq!(handler.calls().len(), 0);
}

#[test]
fn a_retrying_key_does_not_block_another_key() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Retryable {
        retry_after_ms: None,
    });

    let first = consumer
        .deliver(&support::event(b"a", 1), 1_000, &mut handler)
        .expect("the first event retries");
    assert_eq!(first.name(), "retried");
    assert_eq!(consumer.pending_depth(), 1);
    assert_eq!(consumer.cursor(b"a"), EventSequence::new(0));

    handler.set_outcome(HandlerOutcome::Success);
    let second = consumer
        .deliver(&support::event(b"b", 1), 1_000, &mut handler)
        .expect("the other key proceeds");
    assert_eq!(second.name(), "acked");
    assert_eq!(consumer.cursor(b"b"), EventSequence::new(1));
    assert_eq!(consumer.pending_depth(), 1);
}

#[test]
fn duplicate_delivery_runs_one_effect() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);
    let event = support::event(b"a", 1);

    for _ in 0..10 {
        consumer
            .deliver(&event, 1_000, &mut handler)
            .expect("the delivery is resolved");
    }

    assert_eq!(handler.calls().len(), 1);
    assert_eq!(handler.successes().len(), 1);
    assert_eq!(consumer.completion_count(b"a"), 1);
    assert_eq!(consumer.cursor(b"a"), EventSequence::new(1));
}

#[test]
fn handler_failure_retries_then_dead_letters() {
    let mut consumer = EffectConsumer::new(support::config(2, 100, 1_000, 10_000, 8));
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Retryable {
        retry_after_ms: None,
    });
    let event = support::event(b"a", 1);

    let first = consumer
        .deliver(&event, 1_000, &mut handler)
        .expect("the first attempt retries");
    assert_eq!(first.name(), "retried");
    assert_eq!(consumer.cursor(b"a"), EventSequence::new(0));

    let second = consumer
        .deliver(&event, 2_000, &mut handler)
        .expect("the second attempt dead-letters");
    assert_eq!(second.name(), "dead_lettered");
    assert_eq!(second.code(), Some("consumer.dead_lettered"));
    assert_eq!(consumer.dead_letter_depth(), 1);
    assert_eq!(
        consumer.dead_letters()[0].reason,
        DeadLetterReason::RetryBudgetExhausted
    );
    assert_eq!(consumer.dead_letters()[0].attempts, 2);
    assert_eq!(consumer.cursor(b"a"), EventSequence::new(1));
    assert_eq!(handler.calls().len(), 2);
}

#[test]
fn handler_failure_retries_then_succeeds() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Retryable {
        retry_after_ms: None,
    });
    let event = support::event(b"a", 1);

    assert_eq!(
        consumer
            .deliver(&event, 1_000, &mut handler)
            .expect("the first attempt retries")
            .name(),
        "retried"
    );
    handler.set_outcome(HandlerOutcome::Success);
    assert_eq!(
        consumer
            .deliver(&event, 2_000, &mut handler)
            .expect("the second attempt succeeds")
            .name(),
        "acked"
    );
    assert_eq!(handler.calls().len(), 2);
    assert_eq!(handler.successes().len(), 1);
    assert_eq!(consumer.attempts(b"a"), 2);
    assert_eq!(consumer.cursor(b"a"), EventSequence::new(1));
}

#[test]
fn backlog_over_budget_returns_too_slow() {
    let mut consumer = EffectConsumer::new(support::config(3, 100, 1_000, 10_000, 1));
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Retryable {
        retry_after_ms: None,
    });

    assert_eq!(
        consumer
            .deliver(&support::event(b"a", 1), 1_000, &mut handler)
            .expect("the first event retries")
            .name(),
        "retried"
    );
    let error = consumer
        .deliver(&support::event(b"b", 1), 1_000, &mut handler)
        .expect_err("the backlog is over budget");
    assert_eq!(error.code(), "consumer.too_slow");
    assert_eq!(consumer.cursor(b"b"), EventSequence::new(0));

    handler.set_outcome(HandlerOutcome::Success);
    assert_eq!(
        consumer
            .deliver(&support::event(b"a", 1), 2_000, &mut handler)
            .expect("the retry succeeds")
            .name(),
        "acked"
    );
    assert_eq!(
        consumer
            .deliver(&support::event(b"b", 1), 2_000, &mut handler)
            .expect("the other key proceeds")
            .name(),
        "acked"
    );
}

#[test]
fn cursor_past_completion_retention_returns_cursor_expired() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);
    consumer.set_retention_floor(EventSequence::new(5));

    let error = consumer
        .deliver(&support::event(b"a", 1), 1_000, &mut handler)
        .expect_err("the cursor is past retention");
    assert_eq!(error.code(), "consumer.cursor_expired");
    assert_eq!(handler.calls().len(), 0);
    assert_eq!(
        error.to_string(),
        "The cursor is past completion retention. Oldest 5, cursor 0."
    );
}

#[test]
fn required_effect_fields_are_self_contained() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);
    let event = support::event(b"a", 1);

    assert_eq!(
        consumer
            .deliver(&event, 1_000, &mut handler)
            .expect("the effect runs")
            .name(),
        "acked"
    );

    let (key, effect) = &handler.successes()[0];
    assert_eq!(key.as_slice(), b"a");
    assert_eq!(effect.recipient, event.payload.recipient);
    assert_eq!(effect.subject, event.payload.subject);
    assert_eq!(effect.body, event.payload.body);
    assert_eq!(effect.enrichment, None);
    assert_eq!(
        handler.keys()[0],
        ProviderIdempotencyKey::new("invoice.approved", event.event_id)
    );
    assert_eq!(handler.deadlines()[0], Deadline::new(1_500));
}

#[test]
fn optional_enrichment_does_not_block_the_effect() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);
    let event = support::event_with_enrichment(b"a", 1);

    assert_eq!(
        consumer
            .deliver(&event, 1_000, &mut handler)
            .expect("the effect runs without the object")
            .name(),
        "acked"
    );
    assert_eq!(handler.successes().len(), 1);
    assert_eq!(
        handler.successes()[0].1.enrichment,
        Some(support::enrichment())
    );
    assert_eq!(consumer.dead_letter_depth(), 0);
}

#[test]
fn per_key_cursor_resume_does_not_skip() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);

    for sequence in 1..=2 {
        let now = 1_000 + sequence;
        assert_eq!(
            consumer
                .deliver(&support::event(b"a", sequence), now, &mut handler)
                .expect("the event succeeds")
                .name(),
            "acked"
        );
    }
    let checkpoint = consumer.checkpoint();
    let mut restored = EffectConsumer::restore(config(), checkpoint);
    assert_eq!(restored.cursor(b"a"), EventSequence::new(2));

    let duplicate = restored
        .deliver(&support::event(b"a", 2), 2_000, &mut handler)
        .expect("the duplicate is resolved");
    assert_eq!(duplicate.name(), "duplicate");
    let next = restored
        .deliver(&support::event(b"a", 3), 2_100, &mut handler)
        .expect("the next event succeeds");
    assert_eq!(next.name(), "acked");
    assert_eq!(restored.cursor(b"a"), EventSequence::new(3));
    assert_eq!(restored.completion_count(b"a"), 3);
    assert_eq!(handler.successes().len(), 3);
}

#[test]
fn per_key_cursors_advance_independently() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);

    for (key, sequence, now) in [
        (b"a".as_slice(), 1, 1_000),
        (b"b".as_slice(), 1, 1_001),
        (b"a".as_slice(), 2, 1_002),
    ] {
        assert_eq!(
            consumer
                .deliver(&support::event(key, sequence), now, &mut handler)
                .expect("the event succeeds")
                .name(),
            "acked"
        );
    }

    assert_eq!(consumer.cursor(b"a"), EventSequence::new(2));
    assert_eq!(consumer.cursor(b"b"), EventSequence::new(1));
    assert_eq!(consumer.completion_count(b"a"), 2);
    assert_eq!(consumer.completion_count(b"b"), 1);
    assert_eq!(handler.successes().len(), 3);
}

#[test]
fn a_permanent_failure_dead_letters_on_the_first_attempt() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Permanent);

    let delivery = consumer
        .deliver(&support::event(b"a", 1), 1_000, &mut handler)
        .expect("the event dead-letters");
    assert_eq!(delivery.name(), "dead_lettered");
    assert_eq!(
        consumer.dead_letters()[0].reason,
        DeadLetterReason::PermanentFailure
    );
    assert_eq!(consumer.dead_letters()[0].attempts, 1);
    assert_eq!(handler.calls().len(), 1);
}

#[test]
fn a_panic_dead_letters_on_the_first_attempt() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Panic);

    let delivery = consumer
        .deliver(&support::event(b"a", 1), 1_000, &mut handler)
        .expect("the event dead-letters");
    assert_eq!(delivery.name(), "dead_lettered");
    assert_eq!(consumer.dead_letters()[0].reason, DeadLetterReason::Panic);
}

#[test]
fn an_unknown_event_version_dead_letters() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);
    let mut event = support::event(b"a", 1);
    event.envelope_version = oxkv_contracts::version::EventVersion::new(2);

    let delivery = consumer
        .deliver(&event, 1_000, &mut handler)
        .expect("the event dead-letters");
    assert_eq!(delivery.name(), "dead_lettered");
    assert_eq!(
        consumer.dead_letters()[0].reason,
        DeadLetterReason::UnknownEventVersion
    );
    assert_eq!(handler.calls().len(), 0);
}

#[test]
fn a_malformed_payload_dead_letters_before_the_handler() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);

    let delivery = consumer
        .deliver(&support::malformed_event(b"a", 1), 1_000, &mut handler)
        .expect("the event dead-letters");
    assert_eq!(delivery.name(), "dead_lettered");
    assert_eq!(
        consumer.dead_letters()[0].reason,
        DeadLetterReason::MissingRequiredField { field: "subject" }
    );
    assert_eq!(handler.calls().len(), 0);
}

#[test]
fn the_time_cap_dead_letters_the_event() {
    let mut consumer = EffectConsumer::new(support::config(8, 100, 1_000, 100, 8));
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Retryable {
        retry_after_ms: None,
    });
    let event = support::event(b"a", 1);

    assert_eq!(
        consumer
            .deliver(&event, 1_000, &mut handler)
            .expect("the first attempt retries")
            .name(),
        "retried"
    );
    assert_eq!(
        consumer
            .deliver(&event, 1_200, &mut handler)
            .expect("the time cap dead-letters")
            .name(),
        "dead_lettered"
    );
    assert_eq!(
        consumer.dead_letters()[0].reason,
        DeadLetterReason::RetryTimeExhausted
    );
}

#[test]
fn a_provider_hint_raises_the_retry_delay() {
    let mut consumer = EffectConsumer::new(support::config(3, 100, 1_000, 10_000, 8));
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Retryable {
        retry_after_ms: Some(5_000),
    });
    let delivery = consumer
        .deliver(&support::event(b"a", 1), 1_000, &mut handler)
        .expect("the first attempt retries");
    assert_eq!(
        delivery,
        oxkv_consumer::Delivery::Retried {
            key: b"a".to_vec(),
            sequence: EventSequence::new(1),
            attempt: 1,
            retry_at: 2_000,
        }
    );
}

#[test]
fn a_redrive_returns_an_audit_record() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Permanent);
    let event = support::event(b"a", 1);
    consumer
        .deliver(&event, 1_000, &mut handler)
        .expect("the event dead-letters");
    let record = consumer.dead_letters()[0].clone();

    handler.set_outcome(HandlerOutcome::Success);
    let outcome = consumer
        .redrive(record, 2_000, "operator-7", &mut handler)
        .expect("the redrive succeeds");
    assert_eq!(outcome.delivery.name(), "acked");
    assert_eq!(outcome.audit.event_id, event.event_id);
    assert_eq!(outcome.audit.key.as_slice(), b"a");
    assert_eq!(outcome.audit.operator, "operator-7");
    assert_eq!(outcome.audit.redriven_at, 2_000);
    assert_eq!(outcome.audit.attempt, 1);
    assert_eq!(consumer.dead_letter_depth(), 0);
}

#[test]
fn lag_is_visible_per_key() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);
    consumer
        .deliver(&support::event(b"a", 1), 1_000, &mut handler)
        .expect("the event succeeds");
    handler.set_outcome(HandlerOutcome::Retryable {
        retry_after_ms: None,
    });
    consumer
        .deliver(&support::event(b"b", 1), 1_000, &mut handler)
        .expect("the event retries");

    let lag_a = consumer.lag(b"a", EventSequence::new(5));
    assert_eq!(lag_a.cursor, EventSequence::new(1));
    assert_eq!(lag_a.events, 4);
    assert!(!lag_a.is_current());
    let lag_b = consumer.lag(b"b", EventSequence::new(1));
    assert_eq!(lag_b.cursor, EventSequence::new(0));
    assert_eq!(lag_b.events, 1);
    assert_eq!(consumer.attempts(b"b"), 1);
    assert_eq!(consumer.pending_depth(), 1);
    assert_eq!(consumer.dead_letter_depth(), 0);
}

#[test]
fn a_dead_letter_record_does_not_expose_the_payload() {
    let mut consumer = EffectConsumer::new(config());
    let mut handler = support::ScriptedHandler::new(HandlerOutcome::Success);
    consumer
        .deliver(&support::event(b"a", 1), 1_000, &mut handler)
        .expect("the event succeeds");
    let record = oxkv_consumer::DeadLetterRecord {
        record_version: oxkv_consumer::DEAD_LETTER_RECORD_VERSION,
        event: support::event(b"a", 2),
        attempts: 1,
        first_attempt_at: 1_000,
        last_attempt_at: 1_000,
        dead_lettered_at: 1_000,
        reason: DeadLetterReason::PermanentFailure,
    };
    let text = format!("{record:?}");
    assert!(!text.contains("The invoice is approved."));
    assert!(!text.contains("ada@example.test"));
}
