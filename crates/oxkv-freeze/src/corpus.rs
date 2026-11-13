//! Vector corpus coverage for the register.
//!
//! `30-qualification-suite.md` must carry a vector for every core contract and
//! every annex. [`VectorCorpus::check`] fails when one register row has no
//! vector.

use core::fmt;

use crate::register::{ContractId, RegisterRow};

/// One vector in a qualification corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusEntry {
    /// The contract or annex that the vector covers.
    pub contract: ContractId,
    /// The vector name.
    pub vector: String,
}

/// A supplied qualification corpus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VectorCorpus {
    entries: Vec<CorpusEntry>,
}

impl VectorCorpus {
    /// Creates a corpus from entries.
    #[must_use]
    pub fn new(entries: Vec<CorpusEntry>) -> Self {
        Self { entries }
    }

    /// Creates a corpus from register identifiers.
    ///
    /// The function sets each vector name to `inline`.
    #[must_use]
    pub fn from_ids(ids: impl IntoIterator<Item = ContractId>) -> Self {
        let entries = ids
            .into_iter()
            .map(|contract| CorpusEntry {
                contract,
                vector: "inline".to_owned(),
            })
            .collect();
        Self { entries }
    }

    /// Returns every corpus entry.
    #[must_use]
    pub fn entries(&self) -> &[CorpusEntry] {
        &self.entries
    }

    /// Returns the number of corpus entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the corpus has no entry.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns true when the corpus has a vector for the identifier.
    #[must_use]
    pub fn covers(&self, id: ContractId) -> bool {
        self.entries.iter().any(|entry| entry.contract == id)
    }

    /// Returns every register row that the corpus does not cover.
    #[must_use]
    pub fn missing(&self, register: &[RegisterRow]) -> Vec<ContractId> {
        register
            .iter()
            .map(|row| row.id)
            .filter(|id| !self.covers(*id))
            .collect()
    }

    /// Checks that the corpus covers every register row.
    ///
    /// # Errors
    ///
    /// Returns [`CoverageError::MissingRow`] for the first row without a
    /// vector.
    pub fn check(&self, register: &[RegisterRow]) -> Result<(), CoverageError> {
        if let Some(id) = self.missing(register).first() {
            return Err(CoverageError::MissingRow { id: *id });
        }
        Ok(())
    }
}

/// An error from corpus coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageError {
    /// A register row has no vector.
    MissingRow {
        /// The uncovered identifier.
        id: ContractId,
    },
}

impl fmt::Display for CoverageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRow { id } => {
                write!(formatter, "The corpus has no vector for contract {id}.")
            }
        }
    }
}

impl std::error::Error for CoverageError {}

/// Checks one supplied corpus against a register.
///
/// # Errors
///
/// Returns [`CoverageError::MissingRow`] when a register row has no vector.
pub fn check_corpus(corpus: &VectorCorpus, register: &[RegisterRow]) -> Result<(), CoverageError> {
    corpus.check(register)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::register::REGISTER;

    #[test]
    fn a_full_corpus_passes() {
        let corpus = VectorCorpus::from_ids(ContractId::ALL.iter().copied());
        assert_eq!(corpus.check(REGISTER), Ok(()));
        let missing: Vec<ContractId> = Vec::new();
        assert_eq!(corpus.missing(REGISTER), missing);
    }

    #[test]
    fn a_corpus_without_one_row_fails() {
        let ids = ContractId::ALL
            .iter()
            .copied()
            .filter(|id| *id != ContractId::A12);
        let corpus = VectorCorpus::from_ids(ids);
        assert_eq!(
            corpus.check(REGISTER),
            Err(CoverageError::MissingRow {
                id: ContractId::A12
            })
        );
    }

    #[test]
    fn an_empty_corpus_reports_the_first_row() {
        let corpus = VectorCorpus::default();
        assert!(corpus.is_empty());
        assert_eq!(
            corpus.check(REGISTER),
            Err(CoverageError::MissingRow { id: ContractId::C1 })
        );
    }
}
