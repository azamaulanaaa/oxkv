//! Dead-letter records and the redrive audit record.
//!
//! A poison event and an exhausted retry budget both end in one
//! [`DeadLetterRecord`]. The record holds the event, the attempt count, the
//! timestamps, the error class, and the record version. It holds no secret and
//! no provider key. The payload text is redacted in the `Debug` form.

use core::fmt;

use crate::event::{EventEnvelope, EventId};

/// The supported dead-letter record version.
pub const DEAD_LETTER_RECORD_VERSION: u16 = 1;

/// The terminal cause of one dead-letter record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeadLetterReason {
    /// The envelope version is unknown.
    UnknownEventVersion,
    /// The payload schema is unsupported.
    UnsupportedSchema,
    /// The payload did not decode.
    MalformedPayload,
    /// One required payload field is empty.
    MissingRequiredField {
        /// The missing field name.
        field: &'static str,
    },
    /// The attempt budget is exhausted.
    RetryBudgetExhausted,
    /// The time budget is exhausted.
    RetryTimeExhausted,
    /// The handler reported a permanent failure.
    PermanentFailure,
    /// The handler panicked.
    Panic,
}

impl DeadLetterReason {
    /// Returns the canonical error code for the cause.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownEventVersion => "event.unknown_version",
            Self::UnsupportedSchema
            | Self::MalformedPayload
            | Self::MissingRequiredField { .. } => "event.poison",
            Self::RetryBudgetExhausted
            | Self::RetryTimeExhausted
            | Self::PermanentFailure
            | Self::Panic => "consumer.dead_lettered",
        }
    }

    /// Returns true when the cause is deterministic.
    ///
    /// A deterministic cause is terminal on the first attempt.
    #[must_use]
    pub const fn is_poison(self) -> bool {
        matches!(
            self,
            Self::UnknownEventVersion
                | Self::UnsupportedSchema
                | Self::MalformedPayload
                | Self::MissingRequiredField { .. }
        )
    }
}

impl fmt::Display for DeadLetterReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownEventVersion => formatter.write_str("The event version is unknown."),
            Self::UnsupportedSchema => formatter.write_str("The payload schema is unsupported."),
            Self::MalformedPayload => formatter.write_str("The payload is malformed."),
            Self::MissingRequiredField { field } => {
                write!(
                    formatter,
                    "The payload is missing the required field {field}."
                )
            }
            Self::RetryBudgetExhausted => {
                formatter.write_str("The retry attempt budget is exhausted.")
            }
            Self::RetryTimeExhausted => formatter.write_str("The retry time limit is exhausted."),
            Self::PermanentFailure => {
                formatter.write_str("The handler reported a permanent failure.")
            }
            Self::Panic => formatter.write_str("The handler panicked."),
        }
    }
}

/// One dead-letter record.
///
/// The record holds the original event, the attempt count, the first and last
/// attempt times, the dead-letter time, the terminal cause, and the record
/// version. The record holds no idempotency key and no provider secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterRecord {
    /// The record schema version.
    pub record_version: u16,
    /// The original business event.
    pub event: EventEnvelope,
    /// The number of attempts.
    pub attempts: u32,
    /// The time of the first attempt in milliseconds.
    pub first_attempt_at: u64,
    /// The time of the last attempt in milliseconds.
    pub last_attempt_at: u64,
    /// The time of the dead-letter write in milliseconds.
    pub dead_lettered_at: u64,
    /// The terminal cause.
    pub reason: DeadLetterReason,
}

impl DeadLetterRecord {
    /// Returns the event identity.
    #[must_use]
    pub const fn event_id(&self) -> EventId {
        self.event.event_id
    }

    /// Returns the key of the original event.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.event.key
    }
}

/// The audit record for one operator redrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedriveAudit {
    /// The redriven event identity.
    pub event_id: EventId,
    /// The original event key.
    pub key: Vec<u8>,
    /// The operator identity.
    pub operator: String,
    /// The redrive time in milliseconds.
    pub redriven_at: u64,
    /// The attempt number that the redrive started.
    pub attempt: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EffectPayload, EventEnvelope};
    use oxkv_contracts::cursor::EventSequence;
    use oxkv_contracts::keyspace::TableName;
    use oxkv_contracts::version::ShardId;

    fn record() -> DeadLetterRecord {
        let payload = EffectPayload::new(
            "ada@example.test".to_owned(),
            "Invoice 1042 approved".to_owned(),
            "The secret body.".to_owned(),
        );
        let event = EventEnvelope::new(
            EventId::from_bytes([1; 32]),
            ShardId::new(b"invoice/".to_vec()),
            TableName::parse_user("invoice").expect("the table name is valid"),
            b"invoice/1042".to_vec(),
            EventSequence::new(1),
            payload,
        );
        DeadLetterRecord {
            record_version: DEAD_LETTER_RECORD_VERSION,
            event,
            attempts: 8,
            first_attempt_at: 100,
            last_attempt_at: 900,
            dead_lettered_at: 900,
            reason: DeadLetterReason::RetryBudgetExhausted,
        }
    }

    #[test]
    fn a_dead_letter_record_names_its_cause() {
        assert_eq!(
            DeadLetterReason::UnknownEventVersion.code(),
            "event.unknown_version"
        );
        assert_eq!(DeadLetterReason::UnsupportedSchema.code(), "event.poison");
        assert_eq!(
            DeadLetterReason::MissingRequiredField { field: "body" }.code(),
            "event.poison"
        );
        assert_eq!(
            DeadLetterReason::RetryBudgetExhausted.code(),
            "consumer.dead_lettered"
        );
        assert_eq!(DeadLetterReason::Panic.code(), "consumer.dead_lettered");
        assert!(DeadLetterReason::MalformedPayload.is_poison());
        assert!(!DeadLetterReason::RetryTimeExhausted.is_poison());
        assert_eq!(
            DeadLetterReason::MissingRequiredField { field: "body" }.to_string(),
            "The payload is missing the required field body."
        );
    }

    #[test]
    fn a_dead_letter_record_redacts_the_payload() {
        let record = record();
        assert_eq!(record.event_id(), EventId::from_bytes([1; 32]));
        assert_eq!(record.key(), b"invoice/1042");
        let text = format!("{record:?}");
        assert!(!text.contains("The secret body."));
        assert!(text.contains("dead_lettered_at"));
    }

    #[test]
    fn the_payload_does_not_enter_the_record_as_a_secret() {
        let text = format!("{:?}", record().event.payload);
        assert!(!text.contains("ada@example.test"));
    }

    #[test]
    fn every_reason_has_a_message() {
        for reason in [
            DeadLetterReason::UnknownEventVersion,
            DeadLetterReason::UnsupportedSchema,
            DeadLetterReason::MalformedPayload,
            DeadLetterReason::MissingRequiredField { field: "body" },
            DeadLetterReason::RetryBudgetExhausted,
            DeadLetterReason::RetryTimeExhausted,
            DeadLetterReason::PermanentFailure,
            DeadLetterReason::Panic,
        ] {
            assert_ne!(reason.to_string(), "");
        }
    }
}
