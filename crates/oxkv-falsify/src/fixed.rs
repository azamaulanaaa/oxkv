//! Signed fixed-point numbers for stored measurements.
//!
//! [`Fixed`] stores one value as a signed integer count of millionths. The
//! type never stores a floating-point value. A report can therefore freeze the
//! exact bytes of a measurement.
//!
//! The display form uses six decimal places. For example, the stored value
//! `1_500_000` displays as `1.500000`.

use core::fmt;

/// A signed fixed-point value with six decimal places.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fixed(i64);

impl Fixed {
    /// The number of stored units in one whole value.
    pub const SCALE: i64 = 1_000_000;

    const SCALE_I128: i128 = 1_000_000;

    /// Creates a value from a count of millionths.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Creates a value from a whole number.
    ///
    /// Returns `None` when the value does not fit.
    #[must_use]
    pub const fn from_whole(whole: i64) -> Option<Self> {
        match whole.checked_mul(Self::SCALE) {
            Some(micros) => Some(Self(micros)),
            None => None,
        }
    }

    /// Creates a value from a whole percentage.
    ///
    /// Returns `None` when the value does not fit.
    #[must_use]
    pub const fn from_percent(percent: i64) -> Option<Self> {
        whole_parts(percent, 10_000)
    }

    /// Returns the stored count of millionths.
    #[must_use]
    pub const fn micros(self) -> i64 {
        self.0
    }

    /// Returns the zero value.
    #[must_use]
    pub const fn zero() -> Self {
        Self(0)
    }

    /// Returns true when the value is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Computes a ratio in fixed-point form.
    ///
    /// Returns `None` when the denominator is zero or the result does not fit.
    #[must_use]
    pub fn checked_ratio(numerator: u64, denominator: u64) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        let value = i128::from(numerator) * Self::SCALE_I128 / i128::from(denominator);
        i64::try_from(value).ok().map(Self)
    }

    /// Computes a linear slope per decade in fixed-point form.
    ///
    /// The function measures the relative change from `start` to `end` and
    /// divides it by the number of decades. It returns `None` when `start` or
    /// `decades` is zero, or when the result does not fit.
    #[must_use]
    pub fn slope_per_decade(start: u64, end: u64, decades: u32) -> Option<Self> {
        if start == 0 || decades == 0 {
            return None;
        }
        let numerator = (i128::from(end) - i128::from(start)) * Self::SCALE_I128;
        let denominator = i128::from(start) * i128::from(decades);
        let value = numerator / denominator;
        i64::try_from(value).ok().map(Self)
    }
}

const fn whole_parts(value: i64, parts: i64) -> Option<Fixed> {
    match value.checked_mul(parts) {
        Some(micros) => Some(Fixed(micros)),
        None => None,
    }
}

impl fmt::Display for Fixed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let negative = self.0 < 0;
        let magnitude = self.0.unsigned_abs();
        let whole = magnitude / Self::SCALE.unsigned_abs();
        let fraction = magnitude % Self::SCALE.unsigned_abs();
        if negative {
            formatter.write_str("-")?;
        }
        write!(formatter, "{whole}.{fraction:06}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_value_displays_with_six_places() {
        assert_eq!(
            Fixed::from_whole(2).map(|value| value.to_string()),
            Some("2.000000".to_owned())
        );
        assert_eq!(
            Fixed::from_percent(5).map(|value| value.to_string()),
            Some("0.050000".to_owned())
        );
        assert_eq!(Fixed::from_micros(1_500_000).to_string(), "1.500000");
        assert_eq!(Fixed::from_micros(-250_000).to_string(), "-0.250000");
        assert_eq!(Fixed::from_micros(1).to_string(), "0.000001");
        assert_eq!(Fixed::from_whole(i64::MAX), None);
    }

    #[test]
    fn a_ratio_uses_integer_units() {
        assert_eq!(
            Fixed::checked_ratio(5, 1),
            Some(Fixed::from_micros(5_000_000))
        );
        assert_eq!(
            Fixed::checked_ratio(1, 4),
            Some(Fixed::from_micros(250_000))
        );
        assert_eq!(Fixed::checked_ratio(1, 0), None);
    }

    #[test]
    fn a_slope_counts_the_relative_change_per_decade() {
        let slope = Fixed::slope_per_decade(100, 110, 1);
        assert_eq!(slope, Some(Fixed::from_micros(100_000)));
        let flat = Fixed::slope_per_decade(100, 100, 2);
        assert_eq!(flat, Some(Fixed::zero()));
        assert_eq!(Fixed::slope_per_decade(0, 10, 1), None);
        assert_eq!(Fixed::slope_per_decade(10, 20, 0), None);
    }
}
