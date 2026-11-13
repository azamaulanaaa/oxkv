//! Deterministic document-local scoring.
//!
//! The scorer uses integer arithmetic only. It reads no corpus statistic. The
//! manifest declares the scorer kind and its integer parameters. The score of
//! one term is a BM25-like saturation over the term frequency and the field
//! length. A document-local reference length replaces a global average.

use crate::canonical;
use crate::error::IndexError;

/// The scorer kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScorerKind {
    /// A document-local term-frequency scorer.
    DocumentLocalTf,
}

impl ScorerKind {
    /// Every scorer kind.
    pub const ALL: &'static [Self] = &[Self::DocumentLocalTf];

    /// Returns the stable wire tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::DocumentLocalTf => 1,
        }
    }

    /// Returns the canonical name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DocumentLocalTf => "document-local-tf",
        }
    }

    /// Reads a scorer kind from its wire tag.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::DocumentLocalTf),
            _ => None,
        }
    }
}

/// The manifest scorer declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScorerSpec {
    kind: ScorerKind,
    k1_milli: u32,
    b_milli: u32,
    reference_length: u32,
}

impl ScorerSpec {
    /// Creates the document-local term-frequency scorer.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidScorer`] when `b_milli` is above 1000 or
    /// `reference_length` is zero.
    pub fn new(k1_milli: u32, b_milli: u32, reference_length: u32) -> Result<Self, IndexError> {
        let spec = Self {
            kind: ScorerKind::DocumentLocalTf,
            k1_milli,
            b_milli,
            reference_length,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Returns the scorer kind.
    #[must_use]
    pub const fn kind(self) -> ScorerKind {
        self.kind
    }

    /// Returns the saturation parameter in thousandths.
    #[must_use]
    pub const fn k1_milli(self) -> u32 {
        self.k1_milli
    }

    /// Returns the length normalization parameter in thousandths.
    #[must_use]
    pub const fn b_milli(self) -> u32 {
        self.b_milli
    }

    /// Returns the document-local reference length.
    #[must_use]
    pub const fn reference_length(self) -> u32 {
        self.reference_length
    }

    /// Checks the declared parameters.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidScorer`] when `b_milli` is above 1000 or
    /// `reference_length` is zero.
    pub fn validate(self) -> Result<(), IndexError> {
        if self.b_milli > 1000 {
            return Err(IndexError::InvalidScorer {
                reason: "b_milli must be at most 1000".to_owned(),
            });
        }
        if self.reference_length == 0 {
            return Err(IndexError::InvalidScorer {
                reason: "reference_length must be at least 1".to_owned(),
            });
        }
        Ok(())
    }

    /// Scores one term in one field.
    ///
    /// The result is in thousandths. A zero term frequency scores zero.
    #[must_use]
    pub fn score(self, term_frequency: u32, field_length: u32) -> u64 {
        if term_frequency == 0 {
            return 0;
        }
        let tf = u128::from(term_frequency);
        let k1 = u128::from(self.k1_milli);
        let b = u128::from(self.b_milli);
        let length = u128::from(field_length);
        let reference = u128::from(self.reference_length);
        let normalization = (1000 - b) + b * length / reference;
        let denominator = tf * 1_000_000 + k1 * normalization;
        let numerator = tf * (k1 + 1000) * 1_000_000;
        let score = numerator / denominator;
        u64::try_from(score).unwrap_or(u64::MAX)
    }

    pub(crate) fn encode(self, output: &mut Vec<u8>) {
        canonical::u8(output, self.kind.tag());
        canonical::u32(output, self.k1_milli);
        canonical::u32(output, self.b_milli);
        canonical::u32(output, self.reference_length);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_term_frequency_scores_zero() {
        let scorer = ScorerSpec::new(1200, 750, 8).expect("the scorer is valid");
        assert_eq!(scorer.score(0, 8), 0);
    }

    #[test]
    fn the_score_saturates_with_term_frequency() {
        let scorer = ScorerSpec::new(1200, 0, 8).expect("the scorer is valid");
        assert_eq!(scorer.score(1, 8), 1000);
        assert_eq!(scorer.score(2, 8), 1375);
        assert!(scorer.score(2, 8) > scorer.score(1, 8));
        assert!(scorer.score(100, 8) < 2200);
    }

    #[test]
    fn a_long_field_scores_lower() {
        let scorer = ScorerSpec::new(1200, 750, 8).expect("the scorer is valid");
        assert!(scorer.score(2, 4) > scorer.score(2, 32));
    }

    #[test]
    fn the_parameters_are_checked() {
        assert!(ScorerSpec::new(1200, 1001, 8).is_err());
        assert!(ScorerSpec::new(1200, 750, 0).is_err());
        assert!(ScorerSpec::new(0, 0, 1).is_ok());
    }
}
