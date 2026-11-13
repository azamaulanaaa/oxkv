//! Receipt retention.
//!
//! Receipt retention is separate from every read pin. An authority read pin,
//! a replica local pin, and a resume cursor have their own lifetimes.

use crate::time::{DurationMillis, Timestamp};

/// An expired receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("The receipt expired.")]
pub struct ReceiptExpired {
    /// The issue time of the receipt.
    pub issued_at: Timestamp,
    /// The time of the read attempt.
    pub now: Timestamp,
    /// The receipt retention horizon.
    pub horizon: DurationMillis,
}

impl ReceiptExpired {
    /// Returns the canonical error code.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn code(self) -> &'static str {
        "receipt.expired"
    }
}

/// A receipt retention rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptRetention {
    horizon: DurationMillis,
}

impl ReceiptRetention {
    /// The canonical error code for an expired receipt.
    pub const ERROR_CODE: &'static str = "receipt.expired";

    /// Creates a receipt retention rule.
    #[must_use]
    pub const fn new(horizon: DurationMillis) -> Self {
        Self { horizon }
    }

    /// Returns the retention horizon.
    #[must_use]
    pub const fn horizon(self) -> DurationMillis {
        self.horizon
    }

    /// Returns true when the receipt is still retained.
    #[must_use]
    pub fn retains(self, issued_at: Timestamp, now: Timestamp) -> bool {
        now.duration_since(issued_at) <= self.horizon
    }

    /// Checks the receipt against the current time.
    ///
    /// # Errors
    ///
    /// Returns [`ReceiptExpired`] when the receipt is past its horizon.
    pub fn check(self, issued_at: Timestamp, now: Timestamp) -> Result<(), ReceiptExpired> {
        if self.retains(issued_at, now) {
            return Ok(());
        }
        Err(ReceiptExpired {
            issued_at,
            now,
            horizon: self.horizon,
        })
    }
}
