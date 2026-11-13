//! Measurement records and their verdict.
//!
//! One record holds the measured slope, the confidence interval, the verdict,
//! the hardware class, the toolchain version, the commit, the sample count,
//! and the threshold.
//!
//! A missing record is not a pass. A skipped record is not a pass.

use core::fmt;

use thiserror::Error;

use crate::falsifier::{Falsifier, Threshold};
use crate::fixed::Fixed;

/// The hardware class of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HardwareClass {
    /// The committed reference machine.
    Reference,
    /// A shared continuous-integration machine.
    Ci,
    /// A developer machine.
    Developer,
    /// A browser target.
    Browser,
}

impl HardwareClass {
    /// Every hardware class.
    pub const ALL: [Self; 4] = [Self::Reference, Self::Ci, Self::Developer, Self::Browser];

    /// Returns the stable class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Ci => "ci",
            Self::Developer => "developer",
            Self::Browser => "browser",
        }
    }

    /// Reads a hardware class from its stable name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|class| class.name() == name)
    }
}

impl fmt::Display for HardwareClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The verdict of one measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Verdict {
    /// The measured value did not cross the threshold.
    Pass,
    /// The measured value crossed the threshold.
    Breach,
}

impl Verdict {
    /// Returns the stable verdict name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Breach => "breach",
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A closed confidence interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Interval {
    low: Fixed,
    high: Fixed,
}

impl Interval {
    /// Creates a closed interval.
    ///
    /// # Errors
    ///
    /// Returns [`MeasurementError::InvertedInterval`] when the low bound is
    /// above the high bound.
    pub fn new(low: Fixed, high: Fixed) -> Result<Self, MeasurementError> {
        if low > high {
            return Err(MeasurementError::InvertedInterval);
        }
        Ok(Self { low, high })
    }

    /// Returns the low bound.
    #[must_use]
    pub const fn low(self) -> Fixed {
        self.low
    }

    /// Returns the high bound.
    #[must_use]
    pub const fn high(self) -> Fixed {
        self.high
    }

    /// Returns true when the interval contains the value.
    #[must_use]
    pub const fn contains(self, value: Fixed) -> bool {
        self.low.micros() <= value.micros() && value.micros() <= self.high.micros()
    }
}

/// The fields of one measurement record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeasurementParts<'a> {
    /// The falsifier.
    pub falsifier: Falsifier,
    /// The measured slope or primary value.
    pub slope: Fixed,
    /// The confidence interval.
    pub interval: Interval,
    /// The hardware class.
    pub hardware_class: HardwareClass,
    /// The toolchain version.
    pub toolchain_version: &'a str,
    /// The source commit.
    pub commit: &'a str,
    /// The sample count.
    pub sample_count: u32,
}

/// One measurement record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasurementRecord {
    falsifier: Falsifier,
    slope: Fixed,
    interval: Interval,
    verdict: Verdict,
    hardware_class: HardwareClass,
    toolchain_version: String,
    commit: String,
    sample_count: u32,
    threshold: Threshold,
}

impl MeasurementRecord {
    /// Creates a measurement record and derives its verdict.
    ///
    /// # Errors
    ///
    /// Returns [`MeasurementError`] when the interval does not contain the
    /// slope, the sample count is zero, or a required text field is empty.
    pub fn new(parts: MeasurementParts<'_>) -> Result<Self, MeasurementError> {
        if !parts.interval.contains(parts.slope) {
            return Err(MeasurementError::IntervalMissesValue);
        }
        if parts.sample_count == 0 {
            return Err(MeasurementError::NoSamples);
        }
        if parts.toolchain_version.trim().is_empty() {
            return Err(MeasurementError::MissingToolchain);
        }
        if parts.commit.trim().is_empty() {
            return Err(MeasurementError::MissingCommit);
        }
        let verdict = if parts.falsifier.breaches(parts.slope) {
            Verdict::Breach
        } else {
            Verdict::Pass
        };
        Ok(Self {
            falsifier: parts.falsifier,
            slope: parts.slope,
            interval: parts.interval,
            verdict,
            hardware_class: parts.hardware_class,
            toolchain_version: parts.toolchain_version.to_owned(),
            commit: parts.commit.to_owned(),
            sample_count: parts.sample_count,
            threshold: parts.falsifier.threshold(),
        })
    }

    /// Returns the falsifier.
    #[must_use]
    pub const fn falsifier(&self) -> Falsifier {
        self.falsifier
    }

    /// Returns the measured slope.
    #[must_use]
    pub const fn slope(&self) -> Fixed {
        self.slope
    }

    /// Returns the confidence interval.
    #[must_use]
    pub const fn interval(&self) -> Interval {
        self.interval
    }

    /// Returns the verdict.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        self.verdict
    }

    /// Returns true when the verdict is a breach.
    #[must_use]
    pub const fn is_breach(&self) -> bool {
        matches!(self.verdict, Verdict::Breach)
    }

    /// Returns the hardware class.
    #[must_use]
    pub const fn hardware_class(&self) -> HardwareClass {
        self.hardware_class
    }

    /// Returns the toolchain version.
    #[must_use]
    pub fn toolchain_version(&self) -> &str {
        &self.toolchain_version
    }

    /// Returns the source commit.
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }

    /// Returns the sample count.
    #[must_use]
    pub const fn sample_count(&self) -> u32 {
        self.sample_count
    }

    /// Returns the threshold.
    #[must_use]
    pub const fn threshold(&self) -> Threshold {
        self.threshold
    }
}

/// One result for one falsifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FalsifierResult {
    /// A measured record.
    Measured(MeasurementRecord),
    /// A skipped falsifier with a named reason.
    Skipped {
        /// The skipped falsifier.
        falsifier: Falsifier,
        /// The skip reason.
        reason: String,
    },
}

impl FalsifierResult {
    /// Creates a skipped result.
    #[must_use]
    pub fn skipped(falsifier: Falsifier, reason: impl Into<String>) -> Self {
        Self::Skipped {
            falsifier,
            reason: reason.into(),
        }
    }

    /// Returns the falsifier.
    #[must_use]
    pub const fn falsifier(&self) -> Falsifier {
        match self {
            Self::Measured(record) => record.falsifier(),
            Self::Skipped { falsifier, .. } => *falsifier,
        }
    }

    /// Returns true when the result is a skip.
    #[must_use]
    pub const fn is_skipped(&self) -> bool {
        matches!(self, Self::Skipped { .. })
    }

    /// Returns the measured record.
    #[must_use]
    pub const fn measured(&self) -> Option<&MeasurementRecord> {
        match self {
            Self::Measured(record) => Some(record),
            Self::Skipped { .. } => None,
        }
    }
}

/// An error from a measurement record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MeasurementError {
    /// The interval is inverted.
    #[error("The confidence interval is inverted.")]
    InvertedInterval,
    /// The interval does not contain the measured value.
    #[error("The confidence interval does not contain the measured value.")]
    IntervalMissesValue,
    /// The sample count is zero.
    #[error("The sample count is zero.")]
    NoSamples,
    /// The toolchain version is empty.
    #[error("The toolchain version is empty.")]
    MissingToolchain,
    /// The commit is empty.
    #[error("The commit is empty.")]
    MissingCommit,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts<'a>(falsifier: Falsifier, micros: i64) -> MeasurementParts<'a> {
        MeasurementParts {
            falsifier,
            slope: Fixed::from_micros(micros),
            interval: Interval::new(
                Fixed::from_micros(micros - 1_000),
                Fixed::from_micros(micros + 1_000),
            )
            .expect("the interval is valid"),
            hardware_class: HardwareClass::Ci,
            toolchain_version: "rustc 1.99.1",
            commit: "0123456",
            sample_count: 30,
        }
    }

    #[test]
    fn one_record_names_every_required_field() {
        let record = MeasurementRecord::new(parts(Falsifier::SyncAmplification, 20_000))
            .expect("the record is valid");
        assert_eq!(record.falsifier(), Falsifier::SyncAmplification);
        assert_eq!(record.slope(), Fixed::from_micros(20_000));
        assert_eq!(record.interval().low(), Fixed::from_micros(19_000));
        assert_eq!(record.interval().high(), Fixed::from_micros(21_000));
        assert_eq!(record.verdict(), Verdict::Pass);
        assert_eq!(record.hardware_class(), HardwareClass::Ci);
        assert_eq!(record.toolchain_version(), "rustc 1.99.1");
        assert_eq!(record.commit(), "0123456");
        assert_eq!(record.sample_count(), 30);
        assert!(record.threshold().is_candidate());
    }

    #[test]
    fn a_breach_is_derived_from_the_threshold() {
        let record = MeasurementRecord::new(parts(Falsifier::SyncAmplification, 60_000))
            .expect("the record is valid");
        assert_eq!(record.verdict(), Verdict::Breach);
        assert!(record.is_breach());
    }

    #[test]
    fn a_bad_record_is_rejected() {
        let mut bad = parts(Falsifier::SyncAmplification, 20_000);
        bad.interval = Interval::new(Fixed::from_micros(30_000), Fixed::from_micros(40_000))
            .expect("the interval is valid");
        assert_eq!(
            MeasurementRecord::new(bad),
            Err(MeasurementError::IntervalMissesValue)
        );

        let mut no_samples = parts(Falsifier::SyncAmplification, 20_000);
        no_samples.sample_count = 0;
        assert_eq!(
            MeasurementRecord::new(no_samples),
            Err(MeasurementError::NoSamples)
        );

        let mut no_commit = parts(Falsifier::SyncAmplification, 20_000);
        no_commit.commit = "";
        assert_eq!(
            MeasurementRecord::new(no_commit),
            Err(MeasurementError::MissingCommit)
        );

        assert_eq!(
            Interval::new(Fixed::from_micros(2), Fixed::from_micros(1)),
            Err(MeasurementError::InvertedInterval)
        );
    }

    #[test]
    fn a_skip_names_its_falsifier_and_reason() {
        let skip = FalsifierResult::skipped(Falsifier::ColdStart, "The reference machine is down.");
        assert!(skip.is_skipped());
        assert!(skip.measured().is_none());
        assert_eq!(skip.falsifier(), Falsifier::ColdStart);
        assert_eq!(
            skip,
            FalsifierResult::Skipped {
                falsifier: Falsifier::ColdStart,
                reason: "The reference machine is down.".to_owned()
            }
        );
    }
}
