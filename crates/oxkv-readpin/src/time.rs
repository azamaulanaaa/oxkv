//! Injected time values.
//!
//! The crate does not read a clock. The caller passes a [`Timestamp`] on every
//! admission decision and on every retention check. A test adapter can supply
//! a scripted time sequence.

use core::fmt;

/// The number of milliseconds in one second.
pub const MILLIS_PER_SECOND: u64 = 1_000;

/// The number of milliseconds in one hour.
pub const MILLIS_PER_HOUR: u64 = 3_600_000;

/// The number of milliseconds in one day.
pub const MILLIS_PER_DAY: u64 = 86_400_000;

/// A duration in whole milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DurationMillis(u64);

impl DurationMillis {
    /// A zero duration.
    pub const ZERO: Self = Self(0);

    /// Creates a duration from milliseconds.
    #[must_use]
    pub const fn from_millis(value: u64) -> Self {
        Self(value)
    }

    /// Creates a duration from seconds.
    #[must_use]
    pub const fn from_seconds(value: u64) -> Self {
        Self(value.saturating_mul(MILLIS_PER_SECOND))
    }

    /// Creates a duration from hours.
    #[must_use]
    pub const fn from_hours(value: u64) -> Self {
        Self(value.saturating_mul(MILLIS_PER_HOUR))
    }

    /// Creates a duration from days.
    #[must_use]
    pub const fn from_days(value: u64) -> Self {
        Self(value.saturating_mul(MILLIS_PER_DAY))
    }

    /// Returns the duration in milliseconds.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Adds two durations. The result saturates at [`u64::MAX`].
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Subtracts one duration from another. The result saturates at zero.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl fmt::Display for DurationMillis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// A monotonic time value in milliseconds.
///
/// The value has no epoch. The caller injects a consistent sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(u64);

impl Timestamp {
    /// The zero time.
    pub const ZERO: Self = Self(0);

    /// Creates a time value from milliseconds.
    #[must_use]
    pub const fn from_millis(value: u64) -> Self {
        Self(value)
    }

    /// Returns the time value in milliseconds.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Adds a duration. The result saturates at [`u64::MAX`].
    #[must_use]
    pub const fn saturating_add(self, duration: DurationMillis) -> Self {
        Self(self.0.saturating_add(duration.0))
    }

    /// Returns the duration since an earlier time. The result saturates at
    /// zero when the earlier time is later.
    #[must_use]
    pub const fn duration_since(self, earlier: Self) -> DurationMillis {
        DurationMillis(self.0.saturating_sub(earlier.0))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}
