//! Named errors for the event stream and the effect consumer.

use oxkv_contracts::cursor::EventPosition;

use crate::id::EventId;

/// An error from the business event stream or the effect consumer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    /// The envelope version is not supported.
    #[error("The event envelope version is not supported: {found}.")]
    UnknownVersion {
        /// The version text that the reader found.
        found: String,
    },
    /// The consumer cursor is behind event retention.
    #[error("The event cursor is behind retention. The oldest position is {oldest:?}.")]
    RetentionGap {
        /// The oldest event position that retention still holds.
        oldest: EventPosition,
    },
    /// The event cannot reach the handler.
    #[error("The event {event_id} is poison: {reason}.")]
    Poison {
        /// The poison event identity.
        event_id: EventId,
        /// The short cause text. It contains no payload data.
        reason: &'static str,
    },
    /// The event payload is above its declared bound.
    #[error(
        "The event payload is too large. The limit is {limit} bytes and the payload has {actual} bytes."
    )]
    PayloadTooLarge {
        /// The declared byte limit.
        limit: usize,
        /// The actual payload byte length.
        actual: usize,
    },
    /// The consumer is behind the backlog budget.
    #[error("The consumer is too slow. The lag is {lag} events.")]
    TooSlow {
        /// The total unacknowledged event count.
        lag: u64,
    },
    /// The effect handler failed.
    #[error("The effect handler failed for event {event_id} on attempt {attempt}.")]
    HandlerFailed {
        /// The event identity.
        event_id: EventId,
        /// The failed attempt number.
        attempt: u32,
    },
    /// The event moved to the dead-letter queue.
    #[error("The event {event_id} is dead-lettered.")]
    DeadLettered {
        /// The dead-lettered event identity.
        event_id: EventId,
    },
    /// The consumer completion cursor expired.
    #[error("The consumer cursor expired. The oldest position is {oldest:?}.")]
    CursorExpired {
        /// The oldest completion position that retention still holds.
        oldest: EventPosition,
    },
}

impl EventError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownVersion { .. } => "event.unknown_version",
            Self::RetentionGap { .. } => "event.retention_gap",
            Self::Poison { .. } => "event.poison",
            Self::PayloadTooLarge { .. } => "event.payload_too_large",
            Self::TooSlow { .. } => "consumer.too_slow",
            Self::HandlerFailed { .. } => "consumer.handler_failed",
            Self::DeadLettered { .. } => "consumer.dead_lettered",
            Self::CursorExpired { .. } => "consumer.cursor_expired",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_one_canonical_code() {
        let position = EventPosition::Compact(oxkv_contracts::cursor::EventSequence::new(1));
        let id = EventId::from_bytes([1; 32]);
        let cases = [
            (
                EventError::UnknownVersion {
                    found: "oxkv/event/v2".to_owned(),
                },
                "event.unknown_version",
            ),
            (
                EventError::RetentionGap {
                    oldest: position.clone(),
                },
                "event.retention_gap",
            ),
            (
                EventError::Poison {
                    event_id: id,
                    reason: "the payload is malformed",
                },
                "event.poison",
            ),
            (
                EventError::PayloadTooLarge {
                    limit: 10,
                    actual: 11,
                },
                "event.payload_too_large",
            ),
            (EventError::TooSlow { lag: 4 }, "consumer.too_slow"),
            (
                EventError::HandlerFailed {
                    event_id: id,
                    attempt: 2,
                },
                "consumer.handler_failed",
            ),
            (
                EventError::DeadLettered { event_id: id },
                "consumer.dead_lettered",
            ),
            (
                EventError::CursorExpired { oldest: position },
                "consumer.cursor_expired",
            ),
        ];
        for (error, code) in cases {
            assert_eq!(error.code(), code);
        }
    }
}
