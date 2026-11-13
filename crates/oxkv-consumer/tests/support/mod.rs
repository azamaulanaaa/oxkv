#![allow(missing_docs)]
#![allow(dead_code)]

//! Shared test support for the consumer tests.

use oxkv_consumer::RetryPolicy;
use oxkv_consumer::event::{Effect, EffectPayload, EventEnvelope, EventId, ObjectReference};
use oxkv_consumer::handler::{
    Deadline, EffectHandler, EffectRequest, HandlerOutcome, ProviderIdempotencyKey,
};
use oxkv_contracts::cursor::EventSequence;
use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::receipt::RequestId;
use oxkv_contracts::version::{CommitVersion, ContentDigest, ShardId};

/// Returns one valid effect payload.
pub(crate) fn payload() -> EffectPayload {
    EffectPayload::new(
        "ada@example.test".to_owned(),
        "Invoice 1042 approved".to_owned(),
        "The invoice is approved.".to_owned(),
    )
}

/// Returns one enrichment reference for tests.
pub(crate) fn enrichment() -> ObjectReference {
    ObjectReference::new(ContentDigest::from_bytes([7; 32]), 4)
}

/// Returns one event for a key and a sequence.
///
/// The event id derives from the shard, the key, the commit version, the rule
/// identity, and the emission index. The helper varies the commit version and
/// the emission index with the sequence, so two sequences have two ids.
pub(crate) fn event(key: &[u8], sequence: u64) -> EventEnvelope {
    let shard = ShardId::new(b"invoice/".to_vec());
    let emission_index = u32::try_from(sequence).expect("the sequence fits in u32");
    let commit_version = CommitVersion::new(90 + sequence);
    let event_id = EventId::derive(
        &shard,
        key,
        commit_version,
        "invoice.approved",
        emission_index,
    );
    EventEnvelope {
        shard_id: shard,
        rule_id: "invoice.approved".to_owned(),
        commit_version,
        committed_at: 1_000,
        emission_index,
        origin_request_id: RequestId::from_bytes([0x42; 32]),
        ..EventEnvelope::new(
            event_id,
            ShardId::new(b"invoice/".to_vec()),
            TableName::parse_user("invoice").expect("the table name is valid"),
            key.to_vec(),
            EventSequence::new(sequence),
            payload(),
        )
    }
}

/// Returns one event with an unsupported payload schema.
pub(crate) fn poison_event(key: &[u8], sequence: u64) -> EventEnvelope {
    EventEnvelope {
        payload_schema: "unknown/v9".to_owned(),
        ..event(key, sequence)
    }
}

/// Returns one event with a missing required field.
pub(crate) fn malformed_event(key: &[u8], sequence: u64) -> EventEnvelope {
    let mut envelope = event(key, sequence);
    envelope.payload.subject.clear();
    envelope
}

/// Returns one event with the supported schema.
pub(crate) fn event_with_enrichment(key: &[u8], sequence: u64) -> EventEnvelope {
    let mut envelope = event(key, sequence);
    envelope.payload = envelope.payload.with_enrichment(enrichment());
    envelope
}

/// Returns one consumer configuration for tests.
pub(crate) fn config(
    max_attempts: u32,
    base_delay_ms: u64,
    cap_delay_ms: u64,
    deadline_ms: u64,
    backlog_budget: u64,
) -> oxkv_consumer::ConsumerConfig {
    oxkv_consumer::ConsumerConfig::new(
        RetryPolicy::new(max_attempts, base_delay_ms, cap_delay_ms, deadline_ms),
        500,
        backlog_budget,
    )
}

/// One scripted handler that records every call and every success.
pub(crate) struct ScriptedHandler {
    outcome: HandlerOutcome,
    calls: Vec<(Vec<u8>, Effect)>,
    successes: Vec<(Vec<u8>, Effect)>,
    keys: Vec<ProviderIdempotencyKey>,
    deadlines: Vec<Deadline>,
}

impl ScriptedHandler {
    /// Creates a handler with one outcome.
    pub(crate) fn new(outcome: HandlerOutcome) -> Self {
        Self {
            outcome,
            calls: Vec::new(),
            successes: Vec::new(),
            keys: Vec::new(),
            deadlines: Vec::new(),
        }
    }

    /// Sets the outcome of the next call.
    pub(crate) fn set_outcome(&mut self, outcome: HandlerOutcome) {
        self.outcome = outcome;
    }

    /// Returns every call key and effect.
    pub(crate) fn calls(&self) -> &[(Vec<u8>, Effect)] {
        &self.calls
    }

    /// Returns every successful call key and effect.
    pub(crate) fn successes(&self) -> &[(Vec<u8>, Effect)] {
        &self.successes
    }

    /// Returns the provider key of every call.
    pub(crate) fn keys(&self) -> &[ProviderIdempotencyKey] {
        &self.keys
    }

    /// Returns the deadline of every call.
    pub(crate) fn deadlines(&self) -> &[Deadline] {
        &self.deadlines
    }
}

impl EffectHandler for ScriptedHandler {
    fn handle(&mut self, request: &EffectRequest<'_>) -> HandlerOutcome {
        self.calls
            .push((request.event.key.clone(), request.effect.clone()));
        self.keys.push(request.idempotency_key.clone());
        self.deadlines.push(request.deadline);
        if self.outcome == HandlerOutcome::Success {
            self.successes
                .push((request.event.key.clone(), request.effect.clone()));
        }
        self.outcome
    }
}
