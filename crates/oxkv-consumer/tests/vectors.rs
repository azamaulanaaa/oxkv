//! Golden vector runner for the reference effect consumer.
//!
//! Every case pins one consumer scenario. The vector file is append-only for
//! one contract version. A released case does not change.

mod support;

use oxkv_consumer::{ConsumerConfig, Effect, EffectConsumer, EventSequence, HandlerOutcome};
use serde_json::{Value, json};

use support::ScriptedHandler;

#[test]
fn consumer_golden_vectors_are_pinned() {
    let root: Value = serde_json::from_str(include_str!("vectors/consumer.json"))
        .expect("the vector file is JSON");
    assert_eq!(
        root["contract"].as_str(),
        Some("consumer/v1"),
        "the vector contract changed"
    );
    let cases = root["cases"]
        .as_array()
        .expect("the vector file has a case list");
    assert_eq!(cases.len(), 10);
    for case in cases {
        run_case(case);
    }
}

fn run_case(case: &Value) {
    let name = case["name"].as_str().expect("the case names a vector");
    let config = parse_config(&case["config"]);
    let mut consumer = EffectConsumer::new(config.clone());
    let mut handler = ScriptedHandler::new(HandlerOutcome::Success);
    let mut outcomes = Vec::new();

    for step in case["steps"].as_array().expect("the case has a step list") {
        let repeat = step.get("repeat").and_then(Value::as_u64).unwrap_or(1);
        for _ in 0..repeat {
            if let Some(deliver) = step.get("deliver") {
                apply_deliver(deliver, &mut consumer, &mut handler, &mut outcomes);
            } else if let Some(floor) = step.get("set_retention_floor").and_then(Value::as_u64) {
                consumer.set_retention_floor(EventSequence::new(floor));
            } else if step
                .get("checkpoint")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                consumer = EffectConsumer::restore(config.clone(), consumer.checkpoint());
            }
        }
    }

    check_expectations(name, case, &consumer, &handler, &outcomes);
}

fn apply_deliver(
    deliver: &Value,
    consumer: &mut EffectConsumer,
    handler: &mut ScriptedHandler,
    outcomes: &mut Vec<String>,
) {
    let key = deliver["key"]
        .as_str()
        .expect("the deliver step names a key");
    let sequence = deliver["sequence"]
        .as_u64()
        .expect("the deliver step names a sequence");
    let now = deliver["now"]
        .as_u64()
        .expect("the deliver step names a time");
    let mut event = support::event(key.as_bytes(), sequence);

    if let Some(schema) = deliver.get("schema").and_then(Value::as_str) {
        schema.clone_into(&mut event.payload_schema);
    }
    if deliver.get("payload").and_then(Value::as_str) == Some("missing_subject") {
        event.payload.subject.clear();
    }
    if deliver
        .get("enrichment")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        event.payload = event.payload.with_enrichment(support::enrichment());
    }

    let outcome = match deliver["handler"].as_str().unwrap_or("success") {
        "retry" => HandlerOutcome::Retryable {
            retry_after_ms: deliver.get("retry_after_ms").and_then(Value::as_u64),
        },
        "permanent" => HandlerOutcome::Permanent,
        "panic" => HandlerOutcome::Panic,
        _ => HandlerOutcome::Success,
    };
    handler.set_outcome(outcome);
    match consumer.deliver(&event, now, handler) {
        Ok(delivery) => outcomes.push(delivery.name().to_owned()),
        Err(error) => outcomes.push(error.code().to_owned()),
    }
}

fn check_expectations(
    name: &str,
    case: &Value,
    consumer: &EffectConsumer,
    handler: &ScriptedHandler,
    outcomes: &[String],
) {
    let expected = &case["expect"];
    assert_eq!(
        outcomes,
        strings(&expected["outcomes"]),
        "case {name}: the outcomes changed"
    );
    assert_eq!(
        keys(handler.successes()),
        strings(&expected["effect_keys"]),
        "case {name}: the effect keys changed"
    );
    assert_eq!(
        keys(handler.calls()),
        strings(&expected["attempt_keys"]),
        "case {name}: the attempt keys changed"
    );

    if let Some(cursors) = expected.get("cursors").and_then(Value::as_object) {
        for (key, value) in cursors {
            let expected_cursor = value.as_u64().expect("the cursor value is a number");
            assert_eq!(
                consumer.cursor(key.as_bytes()).get(),
                expected_cursor,
                "case {name}: the cursor changed for {key}"
            );
        }
    }
    if let Some(attempts) = expected.get("attempts").and_then(Value::as_object) {
        for (key, value) in attempts {
            let expected_attempts =
                u32::try_from(value.as_u64().expect("the attempt value is a number"))
                    .expect("the attempt value fits in u32");
            assert_eq!(
                consumer.attempts(key.as_bytes()),
                expected_attempts,
                "case {name}: the attempt count changed for {key}"
            );
        }
    }
    if let Some(depth) = expected.get("dead_letter_depth").and_then(Value::as_u64) {
        assert_eq!(
            u64::try_from(consumer.dead_letter_depth()).expect("the depth fits in u64"),
            depth,
            "case {name}: the dead-letter depth changed"
        );
    }
    if expected.get("reasons").is_some() {
        let reasons: Vec<String> = consumer
            .dead_letters()
            .iter()
            .map(|record| record.reason.code().to_owned())
            .collect();
        assert_eq!(
            reasons,
            strings(&expected["reasons"]),
            "case {name}: the dead-letter reasons changed"
        );
    }
    if let Some(expected_effect) = expected.get("effect") {
        let effect = handler
            .successes()
            .last()
            .map(|(_, effect)| effect_json(effect))
            .expect("the case has one completed effect");
        assert_eq!(
            &effect, expected_effect,
            "case {name}: the effect fields changed"
        );
    }
}

fn parse_config(value: &Value) -> ConsumerConfig {
    let max_attempts = u32::try_from(
        value
            .get("max_attempts")
            .and_then(Value::as_u64)
            .unwrap_or(3),
    )
    .expect("the attempt cap fits in u32");
    let base_delay_ms = value
        .get("base_delay_ms")
        .and_then(Value::as_u64)
        .unwrap_or(100);
    let cap_delay_ms = value
        .get("cap_delay_ms")
        .and_then(Value::as_u64)
        .unwrap_or(1_000);
    let deadline_ms = value
        .get("deadline_ms")
        .and_then(Value::as_u64)
        .unwrap_or(10_000);
    let backlog_budget = value
        .get("backlog_budget")
        .and_then(Value::as_u64)
        .unwrap_or(8);
    support::config(
        max_attempts,
        base_delay_ms,
        cap_delay_ms,
        deadline_ms,
        backlog_budget,
    )
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn keys(entries: &[(Vec<u8>, Effect)]) -> Vec<String> {
    entries
        .iter()
        .map(|(key, _)| String::from_utf8_lossy(key).into_owned())
        .collect()
}

fn effect_json(effect: &Effect) -> Value {
    json!({
        "recipient": &effect.recipient,
        "subject": &effect.subject,
        "body": &effect.body,
        "enrichment": effect.enrichment.as_ref().map(|reference| json!({
            "digest": reference.digest.to_hex(),
            "version": reference.version,
        })),
    })
}
