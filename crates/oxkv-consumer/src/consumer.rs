//! The per-key effect consumer.
//!
//! The consumer partitions work by key. Each key has one cursor and one
//! in-flight event. One unacknowledged event blocks only its key.
//!
//! The delivery loop is:
//!
//! 1. [`EffectConsumer::begin`] validates the event and reserves the key.
//! 2. The deployment runs the effect.
//! 3. [`EffectConsumer::ack`] records the completion, or
//!    [`EffectConsumer::nack`] schedules a retry or a dead letter.
//!
//! [`EffectConsumer::deliver`] runs the three steps with one in-process
//! handler.

use std::collections::BTreeMap;

use oxkv_contracts::cursor::EventSequence;

use crate::backoff::RetryPolicy;
use crate::dlq::{DEAD_LETTER_RECORD_VERSION, DeadLetterReason, DeadLetterRecord, RedriveAudit};
use crate::event::{
    EFFECT_ENVELOPE_VERSION, EFFECT_PAYLOAD_SCHEMA, Effect, EventEnvelope, EventId,
};
use crate::handler::{
    Deadline, EffectHandler, EffectRequest, HandlerOutcome, ProviderIdempotencyKey,
};

/// The key of one completion record.
type CompletionKey = (Vec<u8>, EventId);

/// One consumer configuration.
///
/// The caller injects every limit. The crate reads no clock and draws no
/// randomness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerConfig {
    /// The retry attempt cap, time cap, and delay schedule.
    pub retry: RetryPolicy,
    /// The handler deadline in milliseconds, relative to the attempt time.
    pub handler_deadline_ms: u64,
    /// The maximum number of in-flight events. The budget is per shard.
    pub backlog_budget: u64,
    /// The supported effect payload schema.
    pub effect_schema: String,
}

impl ConsumerConfig {
    /// Creates a configuration with the default effect schema.
    #[must_use]
    pub fn new(retry: RetryPolicy, handler_deadline_ms: u64, backlog_budget: u64) -> Self {
        Self {
            retry,
            handler_deadline_ms,
            backlog_budget,
            effect_schema: EFFECT_PAYLOAD_SCHEMA.to_owned(),
        }
    }

    /// Sets the supported effect payload schema.
    #[must_use]
    pub fn with_effect_schema(mut self, schema: String) -> Self {
        self.effect_schema = schema;
        self
    }
}

impl Default for ConsumerConfig {
    fn default() -> Self {
        Self::new(RetryPolicy::DEFAULT, 30_000, 1_024)
    }
}

/// One completion record.
///
/// The record names the key and the event identity. A duplicate delivery
/// finds the record and runs no second effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRecord {
    /// The event key.
    pub key: Vec<u8>,
    /// The event identity.
    pub event_id: EventId,
    /// The acknowledged per-key sequence.
    pub sequence: EventSequence,
    /// The completion time in milliseconds.
    pub completed_at: u64,
}

/// One prepared handler attempt.
///
/// The attempt carries the effect that the consumer built from the event
/// payload, the provider idempotency key, and the deadline. It carries no
/// authority handle.
#[derive(Debug)]
pub struct Attempt {
    /// The attempt number. It starts at one for each event.
    pub number: u32,
    /// The required effect fields, copied from the event payload.
    pub effect: Effect,
    /// The provider idempotency key.
    pub idempotency_key: ProviderIdempotencyKey,
    /// The absolute handler deadline.
    pub deadline: Deadline,
}

/// The result of one reservation.
#[derive(Debug)]
pub enum Begin {
    /// The key is free. Run the effect now.
    Ready(Attempt),
    /// The delivery is resolved without an effect.
    Done(Delivery),
}

/// The reason for one duplicate delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DuplicateCause {
    /// The completion record exists.
    CompletedEvent,
    /// The per-key cursor is at or past the event sequence.
    CursorAtOrPast,
    /// The event is already in the dead-letter queue.
    DeadLetteredEvent,
}

impl DuplicateCause {
    /// Returns the canonical cause name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CompletedEvent => "completed_event",
            Self::CursorAtOrPast => "cursor_at_or_past",
            Self::DeadLetteredEvent => "dead_lettered_event",
        }
    }
}

/// The result of one delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The effect succeeded and the completion record committed.
    Acked {
        /// The event key.
        key: Vec<u8>,
        /// The acknowledged sequence.
        sequence: EventSequence,
    },
    /// The event was already resolved. No effect ran.
    Duplicate {
        /// The event key.
        key: Vec<u8>,
        /// The resolved sequence.
        sequence: EventSequence,
        /// The duplicate cause.
        cause: DuplicateCause,
    },
    /// Another event on the key is not acknowledged. The key is blocked.
    Blocked {
        /// The event key.
        key: Vec<u8>,
        /// The offered sequence.
        sequence: EventSequence,
        /// The sequence of the in-flight event.
        pending_sequence: EventSequence,
    },
    /// The handler failed. A retry is scheduled.
    Retried {
        /// The event key.
        key: Vec<u8>,
        /// The event sequence.
        sequence: EventSequence,
        /// The failed attempt number.
        attempt: u32,
        /// The next attempt time in milliseconds.
        retry_at: u64,
    },
    /// The retry is scheduled for a later time.
    RetryNotDue {
        /// The event key.
        key: Vec<u8>,
        /// The event sequence.
        sequence: EventSequence,
        /// The next attempt time in milliseconds.
        retry_at: u64,
    },
    /// The event is in the dead-letter queue. The key cursor advanced.
    DeadLettered {
        /// The event key.
        key: Vec<u8>,
        /// The event sequence.
        sequence: EventSequence,
        /// The terminal cause.
        reason: DeadLetterReason,
    },
}

impl Delivery {
    /// Returns the canonical delivery name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Acked { .. } => "acked",
            Self::Duplicate { .. } => "duplicate",
            Self::Blocked { .. } => "blocked",
            Self::Retried { .. } => "retried",
            Self::RetryNotDue { .. } => "retry_not_due",
            Self::DeadLettered { .. } => "dead_lettered",
        }
    }

    /// Returns the canonical error code for a failed or scheduled delivery.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::Acked { .. } | Self::Duplicate { .. } | Self::Blocked { .. } => None,
            Self::Retried { .. } | Self::RetryNotDue { .. } => Some("consumer.handler_failed"),
            Self::DeadLettered { .. } => Some("consumer.dead_lettered"),
        }
    }
}

/// The lag of one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lag {
    /// The event key.
    pub key: Vec<u8>,
    /// The acknowledged per-key cursor.
    pub cursor: EventSequence,
    /// The delivered high watermark.
    pub high_watermark: EventSequence,
    /// The number of events behind the high watermark.
    pub events: u64,
}

impl Lag {
    /// Returns true when the cursor equals the high watermark.
    #[must_use]
    pub const fn is_current(&self) -> bool {
        self.events == 0
    }
}

/// One consumer error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConsumerError {
    /// The backlog is over budget.
    #[error("The backlog exceeds the budget. Backlog {backlog}, budget {budget}.")]
    TooSlow {
        /// The current backlog.
        backlog: u64,
        /// The configured budget.
        budget: u64,
    },
    /// The per-key cursor is past completion retention.
    #[error(
        "The cursor is past completion retention. Oldest {}, cursor {}.",
        .oldest.get(),
        .cursor.get()
    )]
    CursorExpired {
        /// The event key.
        key: Vec<u8>,
        /// The current cursor.
        cursor: EventSequence,
        /// The oldest retained sequence.
        oldest: EventSequence,
    },
    /// The per-key sequence has a gap.
    #[error(
        "The event sequence has a gap. Expected {}, found {}.",
        .expected.get(),
        .found.get()
    )]
    SequenceGap {
        /// The event key.
        key: Vec<u8>,
        /// The expected next sequence.
        expected: EventSequence,
        /// The offered sequence.
        found: EventSequence,
    },
    /// The event is not in flight.
    #[error("The event is not in flight.")]
    NotPending {
        /// The event key.
        key: Vec<u8>,
    },
    /// The payload is malformed.
    ///
    /// The delivery path turns a malformed payload into a dead letter before
    /// this error can occur. The error is defensive.
    #[error("The payload is missing the required field {field}.")]
    MalformedPayload {
        /// The missing field name.
        field: &'static str,
    },
    /// The call is not valid in the current state.
    #[error("The call is not valid in the current state.")]
    InvalidTransition {
        /// The event key.
        key: Vec<u8>,
    },
}

impl ConsumerError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooSlow { .. } => "consumer.too_slow",
            Self::CursorExpired { .. } => "consumer.cursor_expired",
            Self::SequenceGap { .. } => "event.retention_gap",
            Self::MalformedPayload { .. } => "event.poison",
            Self::NotPending { .. } | Self::InvalidTransition { .. } => {
                "internal.invariant_violation"
            }
        }
    }
}

/// One durable consumer checkpoint.
///
/// The checkpoint holds the acknowledged state only. In-flight events are
/// absent, so a restored consumer receives them again and runs no second
/// effect for a completed event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerCheckpoint {
    /// The per-key cursors.
    pub cursors: BTreeMap<Vec<u8>, EventSequence>,
    /// The completion records.
    pub completions: Vec<CompletionRecord>,
    /// The dead-letter records.
    pub dead_letters: Vec<DeadLetterRecord>,
    /// The completion retention floor.
    pub retention_floor: EventSequence,
}

impl ConsumerCheckpoint {
    /// Creates an empty checkpoint.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            cursors: BTreeMap::new(),
            completions: Vec::new(),
            dead_letters: Vec::new(),
            retention_floor: EventSequence::new(0),
        }
    }
}

/// The result of one operator redrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedriveOutcome {
    /// The delivery result.
    pub delivery: Delivery,
    /// The audit record for the operator.
    pub audit: RedriveAudit,
}

/// One in-flight event on one key.
#[derive(Debug)]
struct Pending {
    event_id: EventId,
    sequence: EventSequence,
    attempts: u32,
    first_attempt_at: u64,
    last_attempt_at: u64,
    next_attempt_at: u64,
}

/// The reference effect consumer.
///
/// The type is sans-IO. The caller supplies the current time and the handler.
#[derive(Debug)]
pub struct EffectConsumer {
    config: ConsumerConfig,
    cursors: BTreeMap<Vec<u8>, EventSequence>,
    completions: BTreeMap<CompletionKey, CompletionRecord>,
    pending: BTreeMap<Vec<u8>, Pending>,
    last_attempts: BTreeMap<Vec<u8>, u32>,
    dead_letters: Vec<DeadLetterRecord>,
    retention_floor: EventSequence,
}

impl EffectConsumer {
    /// Creates an empty consumer.
    #[must_use]
    pub fn new(config: ConsumerConfig) -> Self {
        Self {
            config,
            cursors: BTreeMap::new(),
            completions: BTreeMap::new(),
            pending: BTreeMap::new(),
            last_attempts: BTreeMap::new(),
            dead_letters: Vec::new(),
            retention_floor: EventSequence::new(0),
        }
    }

    /// Returns the configuration.
    #[must_use]
    pub const fn config(&self) -> &ConsumerConfig {
        &self.config
    }

    /// Returns the completion retention floor.
    #[must_use]
    pub const fn retention_floor(&self) -> EventSequence {
        self.retention_floor
    }

    /// Sets the completion retention floor and drops older completions.
    pub fn set_retention_floor(&mut self, floor: EventSequence) {
        self.retention_floor = floor;
        self.completions
            .retain(|_, record| record.sequence >= floor);
    }

    /// Returns the acknowledged cursor of one key.
    #[must_use]
    pub fn cursor(&self, key: &[u8]) -> EventSequence {
        self.cursors
            .get(key)
            .copied()
            .unwrap_or(EventSequence::new(0))
    }

    /// Returns one completion record.
    #[must_use]
    pub fn completion(&self, key: &[u8], event_id: EventId) -> Option<&CompletionRecord> {
        self.completions.get(&(key.to_vec(), event_id))
    }

    /// Returns the number of completion records for one key.
    #[must_use]
    pub fn completion_count(&self, key: &[u8]) -> usize {
        self.completions
            .keys()
            .filter(|(record_key, _)| record_key.as_slice() == key)
            .count()
    }

    /// Returns the last attempt count of one key.
    #[must_use]
    pub fn attempts(&self, key: &[u8]) -> u32 {
        self.last_attempts.get(key).copied().unwrap_or(0)
    }

    /// Returns the number of in-flight events.
    #[must_use]
    pub fn pending_depth(&self) -> usize {
        self.pending.len()
    }

    /// Returns the number of dead-letter records.
    #[must_use]
    pub fn dead_letter_depth(&self) -> usize {
        self.dead_letters.len()
    }

    /// Returns the dead-letter records in write order.
    #[must_use]
    pub fn dead_letters(&self) -> &[DeadLetterRecord] {
        &self.dead_letters
    }

    /// Removes one dead-letter record for an operator redrive.
    pub fn take_dead_letter(&mut self, event_id: EventId) -> Option<DeadLetterRecord> {
        let index = self
            .dead_letters
            .iter()
            .position(|record| record.event.event_id == event_id)?;
        Some(self.dead_letters.remove(index))
    }

    /// Returns the lag of one key against a delivered high watermark.
    #[must_use]
    pub fn lag(&self, key: &[u8], high_watermark: EventSequence) -> Lag {
        let cursor = self.cursor(key);
        Lag {
            key: key.to_vec(),
            cursor,
            high_watermark,
            events: high_watermark.get().saturating_sub(cursor.get()),
        }
    }

    /// Returns one durable checkpoint of the acknowledged state.
    #[must_use]
    pub fn checkpoint(&self) -> ConsumerCheckpoint {
        ConsumerCheckpoint {
            cursors: self.cursors.clone(),
            completions: self.completions.values().cloned().collect(),
            dead_letters: self.dead_letters.clone(),
            retention_floor: self.retention_floor,
        }
    }

    /// Restores a consumer from one checkpoint.
    #[must_use]
    pub fn restore(config: ConsumerConfig, checkpoint: ConsumerCheckpoint) -> Self {
        let completions = checkpoint
            .completions
            .into_iter()
            .map(|record| ((record.key.clone(), record.event_id), record))
            .collect();
        Self {
            config,
            cursors: checkpoint.cursors,
            completions,
            pending: BTreeMap::new(),
            last_attempts: BTreeMap::new(),
            dead_letters: checkpoint.dead_letters,
            retention_floor: checkpoint.retention_floor,
        }
    }

    /// Reserves one key and prepares one handler attempt.
    ///
    /// The function returns [`Begin::Done`] when the event needs no effect.
    /// The function returns [`Begin::Ready`] when the caller must run the
    /// effect and then call [`Self::ack`] or [`Self::nack`].
    ///
    /// # Errors
    ///
    /// Returns [`ConsumerError::TooSlow`] when the backlog is over budget.
    /// Returns [`ConsumerError::CursorExpired`] when the cursor is past
    /// completion retention. Returns [`ConsumerError::SequenceGap`] when the
    /// per-key sequence has a gap.
    pub fn begin(&mut self, event: &EventEnvelope, now: u64) -> Result<Begin, ConsumerError> {
        self.begin_event(event, now, false)
    }

    /// Records one acknowledged effect.
    ///
    /// The caller MUST run the effect before this call. The delivery loop
    /// calls this method after a handler success. The completion record and
    /// the per-key cursor commit in the same step.
    ///
    /// # Errors
    ///
    /// Returns [`ConsumerError::NotPending`] when the event is not in flight.
    pub fn ack(&mut self, event: &EventEnvelope, now: u64) -> Result<Delivery, ConsumerError> {
        let key = event.key.clone();
        let pending = self
            .pending
            .get(&key)
            .ok_or_else(|| ConsumerError::NotPending { key: key.clone() })?;
        if pending.event_id != event.event_id {
            return Err(ConsumerError::NotPending { key });
        }
        self.pending.remove(&key);
        self.complete(event, now);
        Ok(Delivery::Acked {
            key,
            sequence: event.key_sequence,
        })
    }

    /// Records one failed attempt.
    ///
    /// A retryable failure schedules a retry. A permanent failure, a panic, an
    /// exhausted attempt budget, or an exhausted time budget writes one
    /// dead-letter record and advances the key cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ConsumerError::NotPending`] when the event is not in flight.
    /// Returns [`ConsumerError::InvalidTransition`] when the outcome is
    /// [`HandlerOutcome::Success`].
    pub fn nack(
        &mut self,
        event: &EventEnvelope,
        now: u64,
        outcome: HandlerOutcome,
    ) -> Result<Delivery, ConsumerError> {
        let key = event.key.clone();
        let pending = self
            .pending
            .get(&key)
            .ok_or_else(|| ConsumerError::NotPending { key: key.clone() })?;
        if pending.event_id != event.event_id {
            return Err(ConsumerError::NotPending { key });
        }
        let attempts = pending.attempts;
        let first_attempt_at = pending.first_attempt_at;
        let sequence = pending.sequence;

        match outcome {
            HandlerOutcome::Success => Err(ConsumerError::InvalidTransition { key }),
            HandlerOutcome::Retryable { retry_after_ms } => {
                let elapsed = now.saturating_sub(first_attempt_at);
                if attempts >= self.config.retry.max_attempts
                    || elapsed >= self.config.retry.deadline_ms
                {
                    let reason = if elapsed >= self.config.retry.deadline_ms {
                        DeadLetterReason::RetryTimeExhausted
                    } else {
                        DeadLetterReason::RetryBudgetExhausted
                    };
                    self.pending.remove(&key);
                    self.record_dead_letter(event, attempts, first_attempt_at, now, now, reason);
                    Ok(Delivery::DeadLettered {
                        key,
                        sequence,
                        reason,
                    })
                } else {
                    let delay =
                        self.config
                            .retry
                            .delay_ms(event.event_id, attempts, retry_after_ms);
                    let retry_at = now.saturating_add(delay);
                    if let Some(pending) = self.pending.get_mut(&key) {
                        pending.next_attempt_at = retry_at;
                    }
                    Ok(Delivery::Retried {
                        key,
                        sequence,
                        attempt: attempts,
                        retry_at,
                    })
                }
            }
            HandlerOutcome::Permanent | HandlerOutcome::Panic => {
                let reason = if matches!(outcome, HandlerOutcome::Panic) {
                    DeadLetterReason::Panic
                } else {
                    DeadLetterReason::PermanentFailure
                };
                self.pending.remove(&key);
                self.record_dead_letter(event, attempts, first_attempt_at, now, now, reason);
                Ok(Delivery::DeadLettered {
                    key,
                    sequence,
                    reason,
                })
            }
        }
    }

    /// Delivers one event to one in-process handler.
    ///
    /// The method reserves the key, builds the effect from the event payload,
    /// calls the handler, and then acknowledges or negatively acknowledges the
    /// event.
    ///
    /// # Errors
    ///
    /// Returns the errors from [`Self::begin`].
    pub fn deliver(
        &mut self,
        event: &EventEnvelope,
        now: u64,
        handler: &mut dyn EffectHandler,
    ) -> Result<Delivery, ConsumerError> {
        let attempt = match self.begin(event, now)? {
            Begin::Done(delivery) => return Ok(delivery),
            Begin::Ready(attempt) => attempt,
        };
        let request = EffectRequest {
            event,
            effect: attempt.effect,
            idempotency_key: attempt.idempotency_key,
            deadline: attempt.deadline,
        };
        let outcome = handler.handle(&request);
        if outcome == HandlerOutcome::Success {
            self.ack(event, now)
        } else {
            self.nack(event, now, outcome)
        }
    }

    /// Redrives one dead-letter record with the original key.
    ///
    /// The method runs one fresh attempt with a new attempt budget. It returns
    /// an audit record. A failed redrive writes a new dead-letter record. The
    /// caller can pass a record from [`Self::dead_letters`] or from
    /// [`Self::take_dead_letter`].
    ///
    /// # Errors
    ///
    /// Returns the errors from [`Self::begin`].
    pub fn redrive(
        &mut self,
        record: DeadLetterRecord,
        now: u64,
        operator: &str,
        handler: &mut dyn EffectHandler,
    ) -> Result<RedriveOutcome, ConsumerError> {
        let event_id = record.event.event_id;
        let index = self
            .dead_letters
            .iter()
            .position(|existing| existing.event.event_id == event_id);
        let event = record.event;
        let mut audit = RedriveAudit {
            event_id,
            key: event.key.clone(),
            operator: operator.to_owned(),
            redriven_at: now,
            attempt: 0,
        };

        let attempt = match self.begin_event(&event, now, true)? {
            Begin::Done(delivery) => {
                if matches!(delivery, Delivery::DeadLettered { .. })
                    && let Some(index) = index
                {
                    self.dead_letters.remove(index);
                }
                return Ok(RedriveOutcome { delivery, audit });
            }
            Begin::Ready(attempt) => attempt,
        };
        if let Some(index) = index {
            self.dead_letters.remove(index);
        }
        audit.attempt = attempt.number;
        let request = EffectRequest {
            event: &event,
            effect: attempt.effect,
            idempotency_key: attempt.idempotency_key,
            deadline: attempt.deadline,
        };
        let outcome = handler.handle(&request);
        let delivery = if outcome == HandlerOutcome::Success {
            self.ack(&event, now)?
        } else {
            self.nack(&event, now, outcome)?
        };
        Ok(RedriveOutcome { delivery, audit })
    }

    fn begin_event(
        &mut self,
        event: &EventEnvelope,
        now: u64,
        redrive: bool,
    ) -> Result<Begin, ConsumerError> {
        let key = event.key.clone();
        let sequence = event.key_sequence;
        let cursor = self.cursor(&key);

        if let Some(pending) = self.pending.get(&key) {
            if pending.event_id != event.event_id {
                return Ok(Begin::Done(Delivery::Blocked {
                    key,
                    sequence,
                    pending_sequence: pending.sequence,
                }));
            }
            if now < pending.next_attempt_at {
                return Ok(Begin::Done(Delivery::RetryNotDue {
                    key,
                    sequence,
                    retry_at: pending.next_attempt_at,
                }));
            }
            let attempt = pending.attempts.saturating_add(1);
            if let Some(pending) = self.pending.get_mut(&key) {
                pending.attempts = attempt;
                pending.last_attempt_at = now;
            }
            self.last_attempts.insert(key, attempt);
            return self.ready(event, attempt, now);
        }

        if redrive {
            if let Some(reason) = self.poison_reason(event) {
                self.last_attempts.insert(key.clone(), 1);
                self.record_dead_letter(event, 1, now, now, now, reason);
                return Ok(Begin::Done(Delivery::DeadLettered {
                    key,
                    sequence,
                    reason,
                }));
            }
        } else if let Some(delivery) = self.admit(event, cursor, now)? {
            return Ok(Begin::Done(delivery));
        }

        self.reserve(event, now)
    }

    fn admit(
        &mut self,
        event: &EventEnvelope,
        cursor: EventSequence,
        now: u64,
    ) -> Result<Option<Delivery>, ConsumerError> {
        let key = event.key.clone();
        let sequence = event.key_sequence;
        let completed = self
            .completion(&key, event.event_id)
            .map(|record| record.sequence);
        if let Some(completed) = completed {
            self.set_cursor_at_least(&key, completed);
            return Ok(Some(Delivery::Duplicate {
                key,
                sequence: completed,
                cause: DuplicateCause::CompletedEvent,
            }));
        }
        if self
            .dead_letters
            .iter()
            .any(|record| record.event.event_id == event.event_id)
        {
            return Ok(Some(Delivery::Duplicate {
                key,
                sequence,
                cause: DuplicateCause::DeadLetteredEvent,
            }));
        }
        if sequence <= cursor {
            return Ok(Some(Delivery::Duplicate {
                key,
                sequence,
                cause: DuplicateCause::CursorAtOrPast,
            }));
        }
        if cursor < self.retention_floor || sequence < self.retention_floor {
            return Err(ConsumerError::CursorExpired {
                key,
                cursor,
                oldest: self.retention_floor,
            });
        }
        let expected = EventSequence::new(cursor.get().saturating_add(1));
        if sequence > expected {
            return Err(ConsumerError::SequenceGap {
                key,
                expected,
                found: sequence,
            });
        }
        let backlog = u64::try_from(self.pending.len()).unwrap_or(u64::MAX);
        if backlog >= self.config.backlog_budget {
            return Err(ConsumerError::TooSlow {
                backlog,
                budget: self.config.backlog_budget,
            });
        }
        if let Some(reason) = self.poison_reason(event) {
            self.last_attempts.insert(key.clone(), 1);
            self.record_dead_letter(event, 1, now, now, now, reason);
            return Ok(Some(Delivery::DeadLettered {
                key,
                sequence,
                reason,
            }));
        }
        Ok(None)
    }

    fn reserve(&mut self, event: &EventEnvelope, now: u64) -> Result<Begin, ConsumerError> {
        let key = event.key.clone();
        let pending = Pending {
            event_id: event.event_id,
            sequence: event.key_sequence,
            attempts: 1,
            first_attempt_at: now,
            last_attempt_at: now,
            next_attempt_at: now,
        };
        self.pending.insert(key.clone(), pending);
        self.last_attempts.insert(key, 1);
        self.ready(event, 1, now)
    }

    fn ready(&self, event: &EventEnvelope, attempt: u32, now: u64) -> Result<Begin, ConsumerError> {
        let effect = Effect::from_payload(&event.payload).map_err(|error| {
            ConsumerError::MalformedPayload {
                field: error.field(),
            }
        })?;
        Ok(Begin::Ready(Attempt {
            number: attempt,
            effect,
            idempotency_key: ProviderIdempotencyKey::new(&event.rule_id, event.event_id),
            deadline: Deadline::new(now.saturating_add(self.config.handler_deadline_ms)),
        }))
    }

    fn poison_reason(&self, event: &EventEnvelope) -> Option<DeadLetterReason> {
        if event.envelope_version != EFFECT_ENVELOPE_VERSION {
            return Some(DeadLetterReason::UnknownEventVersion);
        }
        if event.payload_schema != self.config.effect_schema {
            return Some(DeadLetterReason::UnsupportedSchema);
        }
        event
            .payload
            .validate()
            .err()
            .map(|error| DeadLetterReason::MissingRequiredField {
                field: error.field(),
            })
    }

    fn complete(&mut self, event: &EventEnvelope, now: u64) {
        let key = event.key.clone();
        let record = CompletionRecord {
            key: key.clone(),
            event_id: event.event_id,
            sequence: event.key_sequence,
            completed_at: now,
        };
        self.completions
            .insert((key.clone(), event.event_id), record);
        self.set_cursor_at_least(&key, event.key_sequence);
    }

    fn record_dead_letter(
        &mut self,
        event: &EventEnvelope,
        attempts: u32,
        first_attempt_at: u64,
        last_attempt_at: u64,
        dead_lettered_at: u64,
        reason: DeadLetterReason,
    ) {
        let key = event.key.clone();
        self.set_cursor_at_least(&key, event.key_sequence);
        self.dead_letters.push(DeadLetterRecord {
            record_version: DEAD_LETTER_RECORD_VERSION,
            event: event.clone(),
            attempts,
            first_attempt_at,
            last_attempt_at,
            dead_lettered_at,
            reason,
        });
    }

    fn set_cursor_at_least(&mut self, key: &[u8], sequence: EventSequence) {
        let cursor = self
            .cursors
            .entry(key.to_vec())
            .or_insert(EventSequence::new(0));
        if *cursor < sequence {
            *cursor = sequence;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EffectPayload, ObjectReference};
    use oxkv_contracts::keyspace::TableName;
    use oxkv_contracts::version::{CommitVersion, ContentDigest, ShardId};

    struct RecordingHandler {
        outcome: HandlerOutcome,
        effects: Vec<Effect>,
        keys: Vec<ProviderIdempotencyKey>,
    }

    impl RecordingHandler {
        fn new(outcome: HandlerOutcome) -> Self {
            Self {
                outcome,
                effects: Vec::new(),
                keys: Vec::new(),
            }
        }
    }

    impl EffectHandler for RecordingHandler {
        fn handle(&mut self, request: &EffectRequest<'_>) -> HandlerOutcome {
            self.effects.push(request.effect.clone());
            self.keys.push(request.idempotency_key.clone());
            self.outcome
        }
    }

    fn payload() -> EffectPayload {
        EffectPayload::new(
            "ada@example.test".to_owned(),
            "Invoice 1042 approved".to_owned(),
            "The invoice is approved.".to_owned(),
        )
    }

    fn envelope(key: &[u8], sequence: u64) -> EventEnvelope {
        let shard = ShardId::new(b"invoice/".to_vec());
        let emission_index = u32::try_from(sequence).unwrap_or(u32::MAX);
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

    fn config(backlog_budget: u64) -> ConsumerConfig {
        ConsumerConfig::new(RetryPolicy::new(3, 100, 1_000, 10_000), 500, backlog_budget)
    }

    #[test]
    fn one_poison_key_does_not_block_another_key() {
        let mut consumer = EffectConsumer::new(config(8));
        let mut handler = RecordingHandler::new(HandlerOutcome::Success);
        let mut poison = envelope(b"a", 1);
        poison.payload_schema = "unknown/v9".to_owned();

        let first = consumer
            .deliver(&poison, 1_000, &mut handler)
            .expect("the poison event dead-letters");
        assert_eq!(first.name(), "dead_lettered");
        let second = consumer
            .deliver(&envelope(b"b", 1), 1_000, &mut handler)
            .expect("the other key is not blocked");
        assert_eq!(second.name(), "acked");
        assert_eq!(handler.effects.len(), 1);
        assert_eq!(consumer.cursor(b"a"), EventSequence::new(1));
        assert_eq!(consumer.cursor(b"b"), EventSequence::new(1));
    }

    #[test]
    fn a_duplicate_delivery_runs_one_effect() {
        let mut consumer = EffectConsumer::new(config(8));
        let mut handler = RecordingHandler::new(HandlerOutcome::Success);
        let event = envelope(b"a", 1);
        assert_eq!(
            consumer
                .deliver(&event, 1_000, &mut handler)
                .expect("the first delivery succeeds")
                .name(),
            "acked"
        );
        let duplicate = consumer
            .deliver(&event, 1_000, &mut handler)
            .expect("the duplicate is resolved");
        assert_eq!(duplicate.name(), "duplicate");
        assert_eq!(handler.effects.len(), 1);
        assert_eq!(consumer.completion_count(b"a"), 1);
    }

    #[test]
    fn a_retry_does_not_advance_the_cursor() {
        let mut consumer = EffectConsumer::new(config(8));
        let mut handler = RecordingHandler::new(HandlerOutcome::Retryable {
            retry_after_ms: None,
        });
        let event = envelope(b"a", 1);
        let outcome = consumer
            .deliver(&event, 1_000, &mut handler)
            .expect("the failure schedules a retry");
        assert_eq!(outcome.name(), "retried");
        assert_eq!(consumer.cursor(b"a"), EventSequence::new(0));
        assert_eq!(consumer.pending_depth(), 1);
        let early = consumer
            .deliver(&event, 1_050, &mut handler)
            .expect("the retry is not due");
        assert_eq!(early.name(), "retry_not_due");
        assert_eq!(handler.effects.len(), 1);
    }

    #[test]
    fn an_optional_reference_does_not_block_the_effect() {
        let mut consumer = EffectConsumer::new(config(8));
        let mut handler = RecordingHandler::new(HandlerOutcome::Success);
        let mut event = envelope(b"a", 1);
        event.payload = event
            .payload
            .with_enrichment(ObjectReference::new(ContentDigest::from_bytes([9; 32]), 4));
        assert_eq!(
            consumer
                .deliver(&event, 1_000, &mut handler)
                .expect("the effect runs")
                .name(),
            "acked"
        );
        assert_eq!(
            handler.effects[0].enrichment,
            Some(ObjectReference::new(ContentDigest::from_bytes([9; 32]), 4))
        );
    }

    #[test]
    fn a_checkpoint_keeps_the_per_key_cursor() {
        let mut consumer = EffectConsumer::new(config(8));
        let mut handler = RecordingHandler::new(HandlerOutcome::Success);
        consumer
            .deliver(&envelope(b"a", 1), 1_000, &mut handler)
            .expect("the first event succeeds");
        consumer
            .deliver(&envelope(b"a", 2), 1_100, &mut handler)
            .expect("the second event succeeds");
        let checkpoint = consumer.checkpoint();
        let mut restored = EffectConsumer::restore(config(8), checkpoint);
        assert_eq!(restored.cursor(b"a"), EventSequence::new(2));
        let duplicate = restored
            .deliver(&envelope(b"a", 2), 1_200, &mut handler)
            .expect("the duplicate is resolved");
        assert_eq!(duplicate.name(), "duplicate");
        let next = restored
            .deliver(&envelope(b"a", 3), 1_300, &mut handler)
            .expect("the next event succeeds");
        assert_eq!(next.name(), "acked");
        assert_eq!(restored.cursor(b"a"), EventSequence::new(3));
        assert_eq!(handler.effects.len(), 3);
    }

    #[test]
    fn a_cursor_past_retention_returns_cursor_expired() {
        let mut consumer = EffectConsumer::new(config(8));
        consumer.set_retention_floor(EventSequence::new(5));
        let mut handler = RecordingHandler::new(HandlerOutcome::Success);
        let error = consumer
            .deliver(&envelope(b"a", 1), 1_000, &mut handler)
            .expect_err("the cursor is past retention");
        assert_eq!(error.code(), "consumer.cursor_expired");
    }

    #[test]
    fn a_backlog_over_budget_returns_too_slow() {
        let mut consumer = EffectConsumer::new(config(1));
        let mut handler = RecordingHandler::new(HandlerOutcome::Retryable {
            retry_after_ms: None,
        });
        consumer
            .deliver(&envelope(b"a", 1), 1_000, &mut handler)
            .expect("the first event retries");
        let error = consumer
            .deliver(&envelope(b"b", 1), 1_000, &mut handler)
            .expect_err("the backlog is over budget");
        assert_eq!(error.code(), "consumer.too_slow");
    }

    #[test]
    fn a_sequence_gap_returns_retention_gap() {
        let mut consumer = EffectConsumer::new(config(8));
        let mut handler = RecordingHandler::new(HandlerOutcome::Success);
        let error = consumer
            .deliver(&envelope(b"a", 3), 1_000, &mut handler)
            .expect_err("the sequence has a gap");
        assert_eq!(error.code(), "event.retention_gap");
        assert_eq!(
            error.to_string(),
            "The event sequence has a gap. Expected 1, found 3."
        );
    }

    #[test]
    fn a_redrive_uses_the_original_key() {
        let mut consumer = EffectConsumer::new(config(8));
        let mut failing = RecordingHandler::new(HandlerOutcome::Permanent);
        let event = envelope(b"a", 1);
        consumer
            .deliver(&event, 1_000, &mut failing)
            .expect("the event dead-letters");
        assert_eq!(consumer.dead_letter_depth(), 1);
        let record = consumer
            .take_dead_letter(event.event_id)
            .expect("the record exists");

        let mut succeeding = RecordingHandler::new(HandlerOutcome::Success);
        let outcome = consumer
            .redrive(record, 2_000, "operator-7", &mut succeeding)
            .expect("the redrive succeeds");
        assert_eq!(outcome.delivery.name(), "acked");
        assert_eq!(outcome.audit.key, b"a");
        assert_eq!(outcome.audit.operator, "operator-7");
        assert_eq!(outcome.audit.attempt, 1);
        assert_eq!(consumer.dead_letter_depth(), 0);
        assert_eq!(succeeding.keys.len(), 1);
    }

    #[test]
    fn a_nack_of_a_success_is_invalid() {
        let mut consumer = EffectConsumer::new(config(8));
        let event = envelope(b"a", 1);
        assert!(matches!(consumer.begin(&event, 1_000), Ok(Begin::Ready(_))));
        let error = consumer
            .nack(&event, 1_000, HandlerOutcome::Success)
            .expect_err("a success is not a nack");
        assert_eq!(error.code(), "internal.invariant_violation");
    }
}
