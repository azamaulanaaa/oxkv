//! The handler boundary for one effect type.
//!
//! The handler receives the event, an idempotency key, and a deadline. It
//! receives no authority write handle. A data change that the effect needs
//! becomes a new request with its own key and base version.
//!
//! The handler is synchronous and pure from the core point of view. The
//! deployment wraps the network call. A panic must cross the boundary as
//! [`HandlerOutcome::Panic`], so the core can dead-letter the event.

use core::fmt;

use crate::event::{Effect, EventEnvelope, EventId};

/// One absolute deadline in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Deadline(u64);

impl Deadline {
    /// Creates a deadline at one injected time.
    #[must_use]
    pub const fn new(at_ms: u64) -> Self {
        Self(at_ms)
    }

    /// Returns the deadline time in milliseconds.
    #[must_use]
    pub const fn at_ms(self) -> u64 {
        self.0
    }

    /// Returns true when the injected time is at or after the deadline.
    #[must_use]
    pub const fn is_met(self, now_ms: u64) -> bool {
        now_ms >= self.0
    }
}

impl fmt::Display for Deadline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// One provider idempotency key.
///
/// The key derives from the rule identity and the event identity. The same
/// event produces the same key on every delivery, so the provider can absorb a
/// duplicate effect across a crash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderIdempotencyKey(String);

impl ProviderIdempotencyKey {
    /// Builds the provider key for one event.
    #[must_use]
    pub fn new(rule_id: &str, event_id: EventId) -> Self {
        Self(format!("oxkv/{rule_id}/{event_id}"))
    }

    /// Returns the key text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderIdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One request to the effect handler.
///
/// The request carries the event, the effect that the consumer built from the
/// event payload, the provider idempotency key, and the deadline. It carries no
/// authority handle.
#[derive(Debug)]
pub struct EffectRequest<'a> {
    /// The source business event.
    pub event: &'a EventEnvelope,
    /// The required effect fields, copied from the event payload.
    pub effect: Effect,
    /// The provider idempotency key for this event.
    pub idempotency_key: ProviderIdempotencyKey,
    /// The absolute handler deadline.
    pub deadline: Deadline,
}

/// The result of one handler attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandlerOutcome {
    /// The provider accepted the effect.
    Success,
    /// The provider rejected the effect with a retryable error.
    Retryable {
        /// An optional provider delay hint in milliseconds.
        retry_after_ms: Option<u64>,
    },
    /// The provider rejected the effect with a permanent error.
    Permanent,
    /// The handler panicked. The boundary converts the panic to this value.
    Panic,
}

/// One effect handler.
///
/// A deployment supplies one handler per event type. The handler must use the
/// provider idempotency key on every call. The handler must not write the
/// authority.
pub trait EffectHandler {
    /// Runs one effect attempt and reports the outcome.
    fn handle(&mut self, request: &EffectRequest<'_>) -> HandlerOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_key_is_stable_for_one_event() {
        let event_id = EventId::from_bytes([9; 32]);
        let first = ProviderIdempotencyKey::new("invoice.approved", event_id);
        let second = ProviderIdempotencyKey::new("invoice.approved", event_id);
        assert_eq!(first, second);
        assert!(first.as_str().starts_with("oxkv/invoice.approved/"));
        assert_eq!(first.to_string(), first.as_str());
    }

    #[test]
    fn a_deadline_compares_injected_time() {
        let deadline = Deadline::new(1_000);
        assert_eq!(deadline.at_ms(), 1_000);
        assert!(!deadline.is_met(999));
        assert!(deadline.is_met(1_000));
        assert!(deadline.is_met(1_001));
        assert_eq!(deadline.to_string(), "1000");
    }
}
