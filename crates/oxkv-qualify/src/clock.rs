//! A logical clock.
//!
//! The suite never reads the wall clock. A scenario advances the clock by a
//! fixed step. The clock value enters a trace as a canonical integer.

/// A logical clock with a fixed step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalClock {
    now: u64,
    step: u64,
}

impl LogicalClock {
    /// Creates a clock at a start value with a fixed step.
    #[must_use]
    pub const fn new(start: u64, step: u64) -> Self {
        Self { now: start, step }
    }

    /// Returns the current logical time.
    #[must_use]
    pub const fn now(self) -> u64 {
        self.now
    }

    /// Returns the fixed step.
    #[must_use]
    pub const fn step(self) -> u64 {
        self.step
    }

    /// Advances the clock by one step and returns the new time.
    pub fn tick(&mut self) -> u64 {
        self.now = self.now.saturating_add(self.step);
        self.now
    }
}

impl Default for LogicalClock {
    fn default() -> Self {
        Self::new(0, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::LogicalClock;

    #[test]
    fn the_clock_advances_by_a_fixed_step() {
        let mut clock = LogicalClock::new(10, 5);
        assert_eq!(clock.now(), 10);
        assert_eq!(clock.tick(), 15);
        assert_eq!(clock.tick(), 20);
        assert_eq!(clock.step(), 5);
    }

    #[test]
    fn the_clock_saturates_at_the_maximum() {
        let mut clock = LogicalClock::new(u64::MAX - 1, 4);
        assert_eq!(clock.tick(), u64::MAX);
        assert_eq!(clock.tick(), u64::MAX);
    }
}
