//! The deterministic retry policy.
//!
//! The policy bounds every retry by an attempt cap and a time cap. The delay
//! grows with the attempt number. A deterministic jitter keeps a replay equal
//! to the first run. The core draws no randomness.
//!
//! A provider delay hint can raise the delay. The cap still bounds it.

use crate::event::EventId;

/// One attempt cap, time cap, and delay schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RetryPolicy {
    /// The maximum number of attempts for one event.
    pub max_attempts: u32,
    /// The delay of the first retry in milliseconds.
    pub base_delay_ms: u64,
    /// The maximum delay in milliseconds.
    pub cap_delay_ms: u64,
    /// The maximum elapsed time in milliseconds.
    pub deadline_ms: u64,
}

impl RetryPolicy {
    /// The default reference policy.
    pub const DEFAULT: Self = Self {
        max_attempts: 8,
        base_delay_ms: 250,
        cap_delay_ms: 60_000,
        deadline_ms: 3_600_000,
    };

    /// Creates a retry policy.
    #[must_use]
    pub const fn new(
        max_attempts: u32,
        base_delay_ms: u64,
        cap_delay_ms: u64,
        deadline_ms: u64,
    ) -> Self {
        Self {
            max_attempts,
            base_delay_ms,
            cap_delay_ms,
            deadline_ms,
        }
    }

    /// Returns the delay before the next attempt.
    ///
    /// The function doubles the base delay for each attempt. It caps the
    /// result. It adds a jitter in the range `[0, delay / 4]`. The jitter byte
    /// comes from the event identity, so a replay produces the same delay. A
    /// provider hint raises the delay to at least the hint. The cap still
    /// bounds the result.
    #[must_use]
    pub fn delay_ms(&self, event_id: EventId, attempt: u32, hint_ms: Option<u64>) -> u64 {
        let exponent = attempt.saturating_sub(1).min(63);
        let factor = 1_u64 << exponent;
        let base = self
            .base_delay_ms
            .saturating_mul(factor)
            .min(self.cap_delay_ms);
        let span = base / 4 + 1;
        let index = usize::try_from(exponent % 32).unwrap_or(0);
        let byte = u64::from(event_id.as_bytes().get(index).copied().unwrap_or(0));
        let jitter = byte % span;
        let computed = base.saturating_add(jitter).min(self.cap_delay_ms);
        hint_ms.map_or(computed, |hint| computed.max(hint).min(self.cap_delay_ms))
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_id(byte: u8) -> EventId {
        EventId::from_bytes([byte; 32])
    }

    #[test]
    fn the_delay_grows_with_the_attempt() {
        let policy = RetryPolicy::new(8, 100, 1_000, 10_000);
        assert_eq!(policy.delay_ms(event_id(0), 1, None), 100);
        assert_eq!(policy.delay_ms(event_id(0), 2, None), 200);
        assert_eq!(policy.delay_ms(event_id(0), 3, None), 400);
        assert_eq!(policy.delay_ms(event_id(0), 4, None), 800);
        assert_eq!(policy.delay_ms(event_id(0), 5, None), 1_000);
        assert_eq!(policy.delay_ms(event_id(0), 8, None), 1_000);
    }

    #[test]
    fn the_jitter_is_deterministic() {
        let policy = RetryPolicy::new(8, 100, 1_000, 10_000);
        assert_eq!(policy.delay_ms(event_id(0x11), 1, None), 117);
        assert_eq!(policy.delay_ms(event_id(0x11), 2, None), 217);
        assert_eq!(policy.delay_ms(event_id(0x11), 3, None), 417);
        assert_eq!(policy.delay_ms(event_id(0x11), 4, None), 817);
        assert_eq!(policy.delay_ms(event_id(0x11), 5, None), 1_000);
        assert_eq!(
            policy.delay_ms(event_id(0x11), 1, None),
            policy.delay_ms(event_id(0x11), 1, None)
        );
    }

    #[test]
    fn a_provider_hint_raises_the_delay_up_to_the_cap() {
        let policy = RetryPolicy::new(8, 100, 1_000, 10_000);
        assert_eq!(policy.delay_ms(event_id(0x11), 1, Some(500)), 500);
        assert_eq!(policy.delay_ms(event_id(0x11), 1, Some(5_000)), 1_000);
        assert_eq!(policy.delay_ms(event_id(0x11), 1, Some(0)), 117);
    }

    #[test]
    fn a_large_attempt_does_not_overflow() {
        let policy = RetryPolicy::new(8, 100, 1_000, 10_000);
        assert_eq!(policy.delay_ms(event_id(1), u32::MAX, None), 1_000);
    }

    #[test]
    fn the_default_policy_has_eight_attempts() {
        assert_eq!(RetryPolicy::default(), RetryPolicy::DEFAULT);
        assert_eq!(RetryPolicy::default().max_attempts, 8);
    }
}
