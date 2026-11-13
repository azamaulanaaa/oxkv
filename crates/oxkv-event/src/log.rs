//! An in-memory durable event log for the authority and the relay.
//!
//! The log stores canonical envelopes in append order. It serves the consumer
//! protocol through [`EventLog::read_from`]. It is a test and reference
//! implementation of the outbox record. It does not perform I/O.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::cursor::{EventPosition, EventSequence};

use crate::codec::{Decoder, Encoder};
use crate::cursor::ConsumerCursor;
use crate::envelope::EventEnvelope;
use crate::error::EventError;
use crate::retention::RetentionFloor;

/// An in-memory event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLog {
    events: Vec<EventEnvelope>,
    floor: RetentionFloor,
}

impl EventLog {
    /// Creates an empty log with a zero retention floor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            floor: RetentionFloor::new(EventPosition::Compact(EventSequence::new(0))),
        }
    }

    /// Appends one event. The method returns false for a duplicate event id.
    ///
    /// A duplicate append is a replay of one commit. The log keeps one record.
    pub fn append(&mut self, event: EventEnvelope) -> bool {
        if self
            .events
            .iter()
            .any(|existing| existing.event_id() == event.event_id())
        {
            return false;
        }
        self.events.push(event);
        true
    }

    /// Appends every event in order.
    pub fn append_all(&mut self, events: impl IntoIterator<Item = EventEnvelope>) {
        for event in events {
            self.append(event);
        }
    }

    /// Returns the events in append order.
    #[must_use]
    pub fn events(&self) -> &[EventEnvelope] {
        &self.events
    }

    /// Returns the event count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns true when the log has no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Returns the retention floor.
    #[must_use]
    pub const fn retention_floor(&self) -> &RetentionFloor {
        &self.floor
    }

    /// Sets the retention floor.
    ///
    /// The floor is the signal that older positions are no longer readable.
    pub fn set_retention_floor(&mut self, floor: RetentionFloor) {
        self.floor = floor;
    }

    /// Reads retained events from one cursor.
    ///
    /// The method returns per-key events in order. It does not skip a key that
    /// has an earlier unacknowledged event.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::RetentionGap`] when the cursor is behind the
    /// retention floor. The error carries the oldest retained position.
    pub fn read_from(
        &self,
        cursor: &ConsumerCursor,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, EventError> {
        cursor.ensure_readable(&self.floor)?;
        let mut result = Vec::new();
        let mut seen = BTreeSet::new();
        for event in &self.events {
            if result.len() >= limit {
                break;
            }
            if !seen.insert(event.event_id()) {
                continue;
            }
            let key = event.key();
            let next = cursor.next_sequence(key);
            let readable = if cursor.is_compact() {
                event.key_sequence() >= next
            } else {
                event.key_sequence() == next
            };
            if readable {
                result.push(event.clone());
            }
        }
        Ok(result)
    }

    /// Returns the lag per key.
    ///
    /// The lag is the highest retained sequence for a key minus the
    /// acknowledged sequence.
    #[must_use]
    pub fn lag(&self, cursor: &ConsumerCursor) -> BTreeMap<Vec<u8>, u64> {
        let mut high: BTreeMap<Vec<u8>, EventSequence> = BTreeMap::new();
        for event in &self.events {
            high.entry(event.key().to_vec())
                .and_modify(|sequence| {
                    if event.key_sequence() > *sequence {
                        *sequence = event.key_sequence();
                    }
                })
                .or_insert_with(|| event.key_sequence());
        }
        high.into_iter()
            .map(|(key, sequence)| {
                let lag = cursor.lag(&key, sequence);
                (key, lag)
            })
            .collect()
    }

    /// Returns the total lag.
    #[must_use]
    pub fn total_lag(&self, cursor: &ConsumerCursor) -> u64 {
        self.lag(cursor)
            .values()
            .fold(0_u64, |total, lag| total.saturating_add(*lag))
    }

    /// Encodes the log in a canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u64(u64::try_from(self.events.len()).unwrap_or(u64::MAX));
        for event in &self.events {
            encoder.bytes(&event.encode());
        }
        encoder.finish()
    }

    /// Decodes one canonical log.
    ///
    /// # Errors
    ///
    /// Returns [`EventError`] when the bytes are malformed or one envelope is
    /// invalid.
    pub fn decode(bytes: &[u8]) -> Result<Self, EventError> {
        let mut decoder = Decoder::new(bytes);
        let count = decoder.count().map_err(|_| EventError::UnknownVersion {
            found: "<malformed>".to_owned(),
        })?;
        let mut log = Self::new();
        for _ in 0..count {
            let envelope_bytes = decoder.bytes().map_err(|_| EventError::UnknownVersion {
                found: "<malformed>".to_owned(),
            })?;
            log.append(EventEnvelope::decode(envelope_bytes)?);
        }
        Ok(log)
    }
}

impl Default for EventLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{DEFAULT_PAYLOAD_BOUND, EventEnvelopeParts, EventPayload, PayloadValue};
    use crate::id::{
        CommittedAt, EffectField, EmissionIndex, OriginRequestId, PayloadSchema, RuleId,
    };
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
            required_effects: vec![EffectField::new("recipient")],
            retention: RetentionClass::time(3600),
            payload_bound: DEFAULT_PAYLOAD_BOUND,
        })
        .expect("the event is valid")
    }

    #[test]
    fn a_replayed_commit_keeps_one_record() {
        let mut log = EventLog::new();
        assert!(log.append(event(1, b"a")));
        assert!(!log.append(event(1, b"a")));
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn a_cursor_behind_retention_returns_a_gap() {
        let mut log = EventLog::new();
        log.append(event(1, b"a"));
        let floor = RetentionFloor::new(EventPosition::Compact(EventSequence::new(1)));
        log.set_retention_floor(floor.clone());
        let error = log
            .read_from(&ConsumerCursor::compact(EventSequence::new(0)), 10)
            .expect_err("the cursor is behind");
        assert_eq!(error.code(), "event.retention_gap");
        assert_eq!(
            error,
            EventError::RetentionGap {
                oldest: floor.position().clone()
            }
        );
    }

    #[test]
    fn the_log_round_trips() {
        let mut log = EventLog::new();
        log.append(event(1, b"a"));
        log.append(event(1, b"b"));
        assert_eq!(EventLog::decode(&log.encode()), Ok(log));
    }
}
