//! At-least-once effect delivery with a per-key cursor.
//!
//! The consumer partitions work by key. One unacknowledged event blocks its
//! key only. A handler must not receive an authority write handle. The core
//! takes an injected time and performs no I/O.
//!
//! The handler owns meaning and idempotence. The consumer owns order and
//! retry. A consumer crash may duplicate work. It must not skip work.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::cursor::EventSequence;
use oxkv_contracts::version::Hash32;

use crate::codec::Encoder;
use crate::cursor::ConsumerCursor;
use crate::envelope::EventEnvelope;
use crate::error::EventError;
use crate::id::{CommittedAt, EventId, PayloadSchema, RuleId};
use crate::log::EventLog;
use crate::retention::RetentionFloor;

/// A source of retained business events.
pub trait EventSource {
    /// Returns the event retention floor.
    fn retention_floor(&self) -> &RetentionFloor;

    /// Reads retained events from one cursor.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::RetentionGap`] when the cursor is behind the
    /// retention floor.
    fn read_from(
        &self,
        cursor: &ConsumerCursor,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, EventError>;

    /// Returns the lag per key.
    fn lag(&self, cursor: &ConsumerCursor) -> BTreeMap<Vec<u8>, u64>;
}

impl EventSource for EventLog {
    fn retention_floor(&self) -> &RetentionFloor {
        EventLog::retention_floor(self)
    }

    fn read_from(
        &self,
        cursor: &ConsumerCursor,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, EventError> {
        EventLog::read_from(self, cursor, limit)
    }

    fn lag(&self, cursor: &ConsumerCursor) -> BTreeMap<Vec<u8>, u64> {
        EventLog::lag(self, cursor)
    }
}

/// A handler outcome after one effect attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandlerOutcome {
    /// The effect succeeded. The consumer advances the key cursor.
    Ack,
    /// The effect failed and may succeed later. The consumer schedules a retry.
    Retry,
    /// The event is poison. The consumer moves it to the dead-letter queue.
    Poison,
}

impl HandlerOutcome {
    /// Returns the canonical outcome name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ack => "ack",
            Self::Retry => "retry",
            Self::Poison => "poison",
        }
    }
}

/// An injected handler deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Deadline(u64);

impl Deadline {
    /// Creates a deadline value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A provider idempotency key for one effect.
///
/// The consumer derives the key from the event id. An effect provider can use
/// the key to suppress a duplicate effect after a redelivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderIdempotencyKey([u8; 32]);

impl ProviderIdempotencyKey {
    /// Derives the provider idempotency key from one event id.
    #[must_use]
    pub fn for_event(event_id: EventId) -> Self {
        let mut encoder = Encoder::new();
        encoder.text("oxkv/provider-idempotency/v1");
        encoder.raw32(event_id.as_bytes());
        Self(*Hash32::of(&encoder.finish()).as_bytes())
    }

    /// Returns the raw key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One delivery to an effect handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    envelope: EventEnvelope,
    attempt: u32,
    provider_idempotency_key: ProviderIdempotencyKey,
    deadline: Deadline,
}

impl Delivery {
    /// Creates one delivery.
    #[must_use]
    pub fn new(envelope: EventEnvelope, attempt: u32, deadline: Deadline) -> Self {
        let provider_idempotency_key = ProviderIdempotencyKey::for_event(envelope.event_id());
        Self {
            envelope,
            attempt,
            provider_idempotency_key,
            deadline,
        }
    }

    /// Returns the event envelope.
    #[must_use]
    pub const fn envelope(&self) -> &EventEnvelope {
        &self.envelope
    }

    /// Returns the attempt number. The first attempt is 1.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Returns the provider idempotency key.
    #[must_use]
    pub const fn provider_idempotency_key(&self) -> ProviderIdempotencyKey {
        self.provider_idempotency_key
    }

    /// Returns the handler deadline.
    #[must_use]
    pub const fn deadline(&self) -> Deadline {
        self.deadline
    }
}

/// A completion record for one acknowledged effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRecord {
    event_id: EventId,
    key: Vec<u8>,
    provider_idempotency_key: ProviderIdempotencyKey,
    completed_at: CommittedAt,
}

impl CompletionRecord {
    /// Creates a completion record.
    #[must_use]
    pub fn new(
        event_id: EventId,
        key: Vec<u8>,
        provider_idempotency_key: ProviderIdempotencyKey,
        completed_at: CommittedAt,
    ) -> Self {
        Self {
            event_id,
            key,
            provider_idempotency_key,
            completed_at,
        }
    }

    /// Returns the event id.
    #[must_use]
    pub const fn event_id(&self) -> EventId {
        self.event_id
    }

    /// Returns the event key.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Returns the provider idempotency key.
    #[must_use]
    pub const fn provider_idempotency_key(&self) -> ProviderIdempotencyKey {
        self.provider_idempotency_key
    }

    /// Returns the injected completion timestamp.
    #[must_use]
    pub const fn completed_at(&self) -> CommittedAt {
        self.completed_at
    }
}

/// A terminal record for one poison event.
///
/// The record holds identity and diagnostic fields only. It does not hold the
/// payload, the origin request id, or the policy payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterRecord {
    event_id: EventId,
    key: Vec<u8>,
    key_sequence: EventSequence,
    rule_id: RuleId,
    payload_schema: PayloadSchema,
    attempts: u32,
    reason: &'static str,
    dead_lettered_at: CommittedAt,
}

impl DeadLetterRecord {
    /// Creates a dead-letter record from one event.
    #[must_use]
    pub fn new(
        envelope: &EventEnvelope,
        attempts: u32,
        reason: &'static str,
        dead_lettered_at: CommittedAt,
    ) -> Self {
        Self {
            event_id: envelope.event_id(),
            key: envelope.key().to_vec(),
            key_sequence: envelope.key_sequence(),
            rule_id: envelope.rule_id().clone(),
            payload_schema: envelope.payload_schema().clone(),
            attempts,
            reason,
            dead_lettered_at,
        }
    }

    /// Returns the event id.
    #[must_use]
    pub const fn event_id(&self) -> EventId {
        self.event_id
    }

    /// Returns the event key.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Returns the per-key sequence.
    #[must_use]
    pub const fn key_sequence(&self) -> EventSequence {
        self.key_sequence
    }

    /// Returns the rule identity.
    #[must_use]
    pub const fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    /// Returns the payload schema.
    #[must_use]
    pub const fn payload_schema(&self) -> &PayloadSchema {
        &self.payload_schema
    }

    /// Returns the attempt count.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Returns the terminal reason code.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }

    /// Returns the injected dead-letter timestamp.
    #[must_use]
    pub const fn dead_lettered_at(&self) -> CommittedAt {
        self.dead_lettered_at
    }

    /// Encodes the record in a canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.raw32(self.event_id.as_bytes());
        encoder.bytes(&self.key);
        encoder.u64(self.key_sequence.get());
        encoder.text(self.rule_id.as_str());
        encoder.text(self.payload_schema.as_str());
        encoder.u64(u64::from(self.attempts));
        encoder.text(self.reason);
        encoder.u64(self.dead_lettered_at.get());
        encoder.finish()
    }
}

/// A jittered exponential retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RetryPolicy {
    base_delay: u64,
    max_delay: u64,
    attempt_cap: u32,
    time_cap: u64,
}

impl RetryPolicy {
    /// Creates a retry policy.
    #[must_use]
    pub const fn new(base_delay: u64, max_delay: u64, attempt_cap: u32, time_cap: u64) -> Self {
        Self {
            base_delay,
            max_delay,
            attempt_cap,
            time_cap,
        }
    }

    /// Returns the base delay in milliseconds.
    #[must_use]
    pub const fn base_delay(self) -> u64 {
        self.base_delay
    }

    /// Returns the maximum delay in milliseconds.
    #[must_use]
    pub const fn max_delay(self) -> u64 {
        self.max_delay
    }

    /// Returns the attempt cap.
    #[must_use]
    pub const fn attempt_cap(self) -> u32 {
        self.attempt_cap
    }

    /// Returns the time cap in milliseconds.
    #[must_use]
    pub const fn time_cap(self) -> u64 {
        self.time_cap
    }

    /// Returns true when the attempt may run again.
    #[must_use]
    pub const fn should_retry(self, attempt: u32, elapsed: u64) -> bool {
        attempt < self.attempt_cap && elapsed < self.time_cap
    }

    /// Returns the deterministic jittered delay for one attempt.
    ///
    /// The caller supplies a stable seed. The function does not draw
    /// randomness.
    #[must_use]
    pub fn delay(self, attempt: u32, seed: u64) -> u64 {
        let exponent = attempt.min(63);
        let factor = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
        let base = self.base_delay.saturating_mul(factor).min(self.max_delay);
        let span = (self.base_delay / 2).max(1);
        let jitter = seed
            .wrapping_add(u64::from(attempt))
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            % span;
        base.saturating_add(jitter).min(self.max_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(1_000, 60_000, 8, 86_400_000)
    }
}

/// An effect handler. The handler owns meaning and idempotence.
pub trait EffectHandler {
    /// Handles one delivery and returns one outcome.
    fn handle(&mut self, delivery: &Delivery) -> HandlerOutcome;
}

#[derive(Debug, Clone)]
struct InFlight {
    envelope: EventEnvelope,
    attempt: u32,
    first_seen_at: u64,
    next_attempt_at: u64,
}

/// An at-least-once effect consumer.
#[derive(Debug)]
pub struct Consumer<S> {
    source: S,
    cursor: ConsumerCursor,
    in_flight: BTreeMap<Vec<u8>, InFlight>,
    dead_letters: Vec<DeadLetterRecord>,
    completions: Vec<CompletionRecord>,
    retry: RetryPolicy,
    handler_timeout: u64,
    backlog_budget: u64,
}

impl<S: EventSource> Consumer<S> {
    /// Creates a consumer with an empty per-key cursor.
    #[must_use]
    pub fn new(source: S) -> Self {
        Self {
            source,
            cursor: ConsumerCursor::new(),
            in_flight: BTreeMap::new(),
            dead_letters: Vec::new(),
            completions: Vec::new(),
            retry: RetryPolicy::default(),
            handler_timeout: 30_000,
            backlog_budget: 10_000,
        }
    }

    /// Sets the retry policy.
    #[must_use]
    pub const fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Sets the handler timeout in milliseconds.
    #[must_use]
    pub const fn with_handler_timeout(mut self, handler_timeout: u64) -> Self {
        self.handler_timeout = handler_timeout;
        self
    }

    /// Sets the backlog budget.
    #[must_use]
    pub const fn with_backlog_budget(mut self, backlog_budget: u64) -> Self {
        self.backlog_budget = backlog_budget;
        self
    }

    /// Sets the initial cursor.
    #[must_use]
    pub fn with_cursor(mut self, cursor: ConsumerCursor) -> Self {
        self.cursor = cursor;
        self
    }

    /// Returns the consumer cursor.
    #[must_use]
    pub const fn cursor(&self) -> &ConsumerCursor {
        &self.cursor
    }

    /// Returns the source.
    #[must_use]
    pub const fn source(&self) -> &S {
        &self.source
    }

    /// Returns the dead-letter records.
    #[must_use]
    pub fn dead_letters(&self) -> &[DeadLetterRecord] {
        &self.dead_letters
    }

    /// Returns the completion records.
    #[must_use]
    pub fn completions(&self) -> &[CompletionRecord] {
        &self.completions
    }

    /// Returns the in-flight event count.
    #[must_use]
    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    /// Returns the lag per key.
    #[must_use]
    pub fn lag(&self) -> BTreeMap<Vec<u8>, u64> {
        self.source.lag(&self.cursor)
    }

    /// Returns the attempt count per in-flight key.
    #[must_use]
    pub fn attempts(&self) -> BTreeMap<Vec<u8>, u32> {
        self.in_flight
            .iter()
            .map(|(key, in_flight)| (key.clone(), in_flight.attempt))
            .collect()
    }

    /// Returns the dead-letter depth.
    #[must_use]
    pub fn dead_letter_depth(&self) -> usize {
        self.dead_letters.len()
    }

    /// Reads up to one credit of deliveries.
    ///
    /// The method returns due retries first. It then reads new events for keys
    /// with no in-flight event. A key with an unacknowledged event is not read
    /// again.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::RetentionGap`] when the cursor is behind
    /// retention. Returns [`EventError::TooSlow`] when the total lag is above
    /// the backlog budget.
    pub fn poll(&mut self, now: u64, credit: usize) -> Result<Vec<Delivery>, EventError> {
        if credit == 0 {
            return Ok(Vec::new());
        }
        self.cursor.ensure_readable(self.source.retention_floor())?;
        let lag = self
            .source
            .lag(&self.cursor)
            .values()
            .fold(0_u64, |total, value| total.saturating_add(*value));
        if lag > self.backlog_budget {
            return Err(EventError::TooSlow { lag });
        }
        let mut deliveries = Vec::new();
        self.collect_retries(now, credit, &mut deliveries);
        let remaining = credit.saturating_sub(deliveries.len());
        if remaining > 0 {
            let candidates = self
                .source
                .read_from(&self.cursor, remaining.saturating_mul(4).max(remaining))?;
            self.collect_new(now, remaining, &candidates, &mut deliveries);
        }
        Ok(deliveries)
    }

    /// Delivers one event to a handler and settles the outcome.
    ///
    /// The handler runs before the acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns any [`EventError`] from [`Consumer::poll`] or
    /// [`Consumer::settle`].
    pub fn deliver_once<H: EffectHandler>(
        &mut self,
        handler: &mut H,
        now: u64,
    ) -> Result<Option<Delivery>, EventError> {
        let mut deliveries = self.poll(now, 1)?;
        let Some(delivery) = deliveries.pop() else {
            return Ok(None);
        };
        let outcome = handler.handle(&delivery);
        self.settle(&delivery, outcome, now)?;
        Ok(Some(delivery))
    }

    /// Settles one delivery.
    ///
    /// An acknowledgement advances the cursor after the effect. A retry does
    /// not advance the cursor and schedules a later attempt. A poison event
    /// commits its dead-letter record and its cursor advance together.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::HandlerFailed`] when the delivery is not in
    /// flight. Returns [`EventError::DeadLettered`] after a terminal failure.
    pub fn settle(
        &mut self,
        delivery: &Delivery,
        outcome: HandlerOutcome,
        now: u64,
    ) -> Result<(), EventError> {
        let key = delivery.envelope().key().to_vec();
        let event_id = delivery.envelope().event_id();
        let Some(mut in_flight) = self.in_flight.remove(&key) else {
            return Err(EventError::HandlerFailed {
                event_id,
                attempt: delivery.attempt(),
            });
        };
        if in_flight.envelope.event_id() != event_id {
            self.in_flight.insert(key, in_flight);
            return Err(EventError::HandlerFailed {
                event_id,
                attempt: delivery.attempt(),
            });
        }
        match outcome {
            HandlerOutcome::Ack => {
                self.completions.push(CompletionRecord::new(
                    event_id,
                    key,
                    delivery.provider_idempotency_key(),
                    CommittedAt::new(now),
                ));
                let _ = self
                    .cursor
                    .acknowledge(in_flight.envelope.key(), in_flight.envelope.key_sequence());
                Ok(())
            }
            HandlerOutcome::Retry => {
                let elapsed = now.saturating_sub(in_flight.first_seen_at);
                let next_attempt = in_flight.attempt.saturating_add(1);
                if !self.retry.should_retry(next_attempt, elapsed) {
                    self.dead_letter(&in_flight, next_attempt, "consumer.handler_failed", now);
                    return Err(EventError::DeadLettered { event_id });
                }
                in_flight.attempt = next_attempt;
                in_flight.next_attempt_at = now.saturating_add(
                    self.retry
                        .delay(next_attempt, key_seed(in_flight.envelope.key())),
                );
                self.in_flight.insert(key, in_flight);
                Ok(())
            }
            HandlerOutcome::Poison => {
                self.dead_letter(&in_flight, in_flight.attempt, "event.poison", now);
                Err(EventError::DeadLettered { event_id })
            }
        }
    }

    fn collect_retries(&self, now: u64, credit: usize, deliveries: &mut Vec<Delivery>) {
        for in_flight in self.in_flight.values() {
            if deliveries.len() >= credit {
                break;
            }
            if now >= in_flight.next_attempt_at {
                deliveries.push(self.delivery(in_flight, now));
            }
        }
    }

    fn collect_new(
        &mut self,
        now: u64,
        credit: usize,
        candidates: &[EventEnvelope],
        deliveries: &mut Vec<Delivery>,
    ) {
        let mut seen = BTreeSet::new();
        for envelope in candidates {
            if deliveries.len() >= credit {
                break;
            }
            let key = envelope.key().to_vec();
            if self.in_flight.contains_key(&key) || !seen.insert(key.clone()) {
                continue;
            }
            if envelope.key_sequence() != self.cursor.next_sequence(&key) {
                continue;
            }
            if self
                .dead_letters
                .iter()
                .any(|record| record.event_id() == envelope.event_id())
            {
                let _ = self.cursor.acknowledge(&key, envelope.key_sequence());
                continue;
            }
            let in_flight = InFlight {
                envelope: envelope.clone(),
                attempt: 1,
                first_seen_at: now,
                next_attempt_at: u64::MAX,
            };
            let delivery = self.delivery(&in_flight, now);
            self.in_flight.insert(key, in_flight);
            deliveries.push(delivery);
        }
    }

    fn delivery(&self, in_flight: &InFlight, now: u64) -> Delivery {
        Delivery::new(
            in_flight.envelope.clone(),
            in_flight.attempt,
            Deadline::new(now.saturating_add(self.handler_timeout)),
        )
    }

    fn dead_letter(&mut self, in_flight: &InFlight, attempts: u32, reason: &'static str, now: u64) {
        self.dead_letters.push(DeadLetterRecord::new(
            &in_flight.envelope,
            attempts,
            reason,
            CommittedAt::new(now),
        ));
        let _ = self
            .cursor
            .acknowledge(in_flight.envelope.key(), in_flight.envelope.key_sequence());
    }
}

fn key_seed(key: &[u8]) -> u64 {
    let digest = Hash32::of(key);
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_be_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{DEFAULT_PAYLOAD_BOUND, EventEnvelopeParts, EventPayload, PayloadValue};
    use crate::id::{EmissionIndex, OriginRequestId};
    use crate::retention::RetentionClass;
    use oxkv_contracts::keyspace::TableName;
    use oxkv_contracts::version::{CommitVersion, PolicyVersion, ShardId};

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
            payload: EventPayload::new()
                .with_field("recipient", PayloadValue::Text("a@example.com".to_owned())),
            payload_schema: PayloadSchema::new("welcome_mail/v1"),
            origin_request_id: OriginRequestId::new(vec![9]),
            required_effects: vec![crate::id::EffectField::new("recipient")],
            retention: RetentionClass::time(3600),
            payload_bound: DEFAULT_PAYLOAD_BOUND,
        })
        .expect("the event is valid")
    }

    fn log() -> EventLog {
        let mut log = EventLog::new();
        log.append(event(1, b"a"));
        log.append(event(1, b"b"));
        log
    }

    fn log_one_key() -> EventLog {
        let mut log = EventLog::new();
        log.append(event(1, b"a"));
        log
    }

    #[test]
    fn a_retry_does_not_advance_the_cursor() {
        let mut consumer = Consumer::new(log());
        let deliveries = consumer.poll(0, 10).expect("the poll succeeds");
        assert_eq!(deliveries.len(), 2);
        let first = &deliveries[0];
        consumer
            .settle(first, HandlerOutcome::Retry, 0)
            .expect("the retry is scheduled");
        assert_eq!(
            consumer.cursor().sequence_for(first.envelope().key()),
            EventSequence::new(0)
        );
        assert_eq!(
            consumer.cursor().next_sequence(first.envelope().key()),
            EventSequence::new(1)
        );
    }

    #[test]
    fn an_ack_advances_only_its_key() {
        let mut consumer = Consumer::new(log());
        let deliveries = consumer.poll(0, 10).expect("the poll succeeds");
        let first = &deliveries[0];
        consumer
            .settle(first, HandlerOutcome::Ack, 1)
            .expect("the ack succeeds");
        assert_eq!(
            consumer.cursor().sequence_for(first.envelope().key()),
            EventSequence::new(1)
        );
        assert_eq!(
            consumer.cursor().sequence_for(b"other"),
            EventSequence::new(0)
        );
        assert_eq!(consumer.completions().len(), 1);
    }

    #[test]
    fn a_retry_is_redelivered_with_the_same_id() {
        let mut consumer = Consumer::new(log_one_key()).with_retry(RetryPolicy::new(1, 1, 4, 1000));
        let deliveries = consumer.poll(0, 1).expect("the poll succeeds");
        let first = &deliveries[0];
        let event_id = first.envelope().event_id();
        consumer
            .settle(first, HandlerOutcome::Retry, 0)
            .expect("the retry is scheduled");
        assert_eq!(
            consumer.poll(0, 1).expect("the poll succeeds"),
            Vec::<Delivery>::new()
        );
        let retries = consumer.poll(1, 1).expect("the poll succeeds");
        assert_eq!(retries.len(), 1);
        assert_eq!(retries[0].envelope().event_id(), event_id);
        assert_eq!(retries[0].attempt(), 2);
    }

    #[test]
    fn the_retry_cap_dead_letters_and_advances() {
        let mut consumer = Consumer::new(log_one_key()).with_retry(RetryPolicy::new(1, 1, 3, 1000));
        let deliveries = consumer.poll(0, 1).expect("the poll succeeds");
        let first = deliveries[0].clone();
        consumer
            .settle(&first, HandlerOutcome::Retry, 0)
            .expect("the first retry is scheduled");
        let retries = consumer.poll(1, 1).expect("the poll succeeds");
        assert_eq!(retries.len(), 1);
        consumer
            .settle(&retries[0], HandlerOutcome::Retry, 1)
            .expect_err("the retry cap is reached");
        assert_eq!(consumer.dead_letters().len(), 1);
        assert_eq!(
            consumer.cursor().sequence_for(first.envelope().key()),
            EventSequence::new(1)
        );
    }
}
