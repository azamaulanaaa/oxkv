//! The freeze statement gate.
//!
//! A freeze statement must name a register row. [`check_claim`] fails when a
//! statement says "freeze" without a row, names an unknown row, or names a row
//! in its row list but not in its text.
//!
//! [`check_claim_coverage`] fails when no claim names a register row.

use core::fmt;

use crate::register::{ContractId, RegisterRow, require_row};

/// One freeze claim in an essay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EssayFreezeClaim {
    /// The essay file name.
    pub essay: String,
    /// The statement that uses the word "freeze" or "frozen".
    pub statement: String,
    /// The register rows that the statement names.
    pub rows: Vec<ContractId>,
}

impl EssayFreezeClaim {
    /// Creates one freeze claim.
    #[must_use]
    pub fn new(
        essay: impl Into<String>,
        statement: impl Into<String>,
        rows: Vec<ContractId>,
    ) -> Self {
        Self {
            essay: essay.into(),
            statement: statement.into(),
            rows,
        }
    }
}

/// An error from the freeze statement gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// A freeze statement names no register row.
    MissingRowReference {
        /// The essay file name.
        essay: String,
    },
    /// A freeze statement names a row that the register does not have.
    UnknownRow {
        /// The essay file name.
        essay: String,
        /// The unknown row.
        id: ContractId,
    },
    /// A claim lists a row that its statement does not name.
    RowNotNamed {
        /// The essay file name.
        essay: String,
        /// The row that the statement omits.
        id: ContractId,
    },
    /// A claim has no freeze word.
    NotAFreezeStatement {
        /// The essay file name.
        essay: String,
    },
    /// No claim names one register row.
    UncoveredRow {
        /// The row without a claim.
        id: ContractId,
    },
}

impl fmt::Display for GateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRowReference { essay } => write!(
                formatter,
                "The freeze statement in {essay} names no register row."
            ),
            Self::UnknownRow { essay, id } => write!(
                formatter,
                "The freeze statement in {essay} names {id}. The register has no such row."
            ),
            Self::RowNotNamed { essay, id } => write!(
                formatter,
                "The claim in {essay} lists {id}. The statement does not name it."
            ),
            Self::NotAFreezeStatement { essay } => {
                write!(formatter, "The claim in {essay} has no freeze word.")
            }
            Self::UncoveredRow { id } => {
                write!(
                    formatter,
                    "No essay freeze claim names the register row {id}."
                )
            }
        }
    }
}

impl std::error::Error for GateError {}

/// Returns true when the text uses "freeze" or "frozen".
#[must_use]
pub fn mentions_freeze(statement: &str) -> bool {
    let lower = statement.to_ascii_lowercase();
    lower.contains("freeze") || lower.contains("frozen")
}

/// Checks one freeze claim against the register.
///
/// # Errors
///
/// Returns [`GateError::NotAFreezeStatement`] when the statement has no freeze
/// word. Returns [`GateError::MissingRowReference`] when the claim lists no
/// row. Returns [`GateError::UnknownRow`] when a listed row is absent from the
/// register. Returns [`GateError::RowNotNamed`] when the statement text does
/// not name a listed row.
pub fn check_claim(claim: &EssayFreezeClaim, register: &[RegisterRow]) -> Result<(), GateError> {
    if !mentions_freeze(&claim.statement) {
        return Err(GateError::NotAFreezeStatement {
            essay: claim.essay.clone(),
        });
    }
    if claim.rows.is_empty() {
        return Err(GateError::MissingRowReference {
            essay: claim.essay.clone(),
        });
    }
    for id in &claim.rows {
        require_row(*id, register).map_err(|_| GateError::UnknownRow {
            essay: claim.essay.clone(),
            id: *id,
        })?;
        if !claim.statement.contains(id.as_str()) {
            return Err(GateError::RowNotNamed {
                essay: claim.essay.clone(),
                id: *id,
            });
        }
    }
    Ok(())
}

/// Checks every freeze claim against the register.
///
/// # Errors
///
/// Returns the first [`GateError`] from [`check_claim`].
pub fn check_claims(
    claims: &[EssayFreezeClaim],
    register: &[RegisterRow],
) -> Result<(), GateError> {
    for claim in claims {
        check_claim(claim, register)?;
    }
    Ok(())
}

/// Checks that at least one claim names every register row.
///
/// # Errors
///
/// Returns [`GateError::UncoveredRow`] for the first row without a claim.
pub fn check_claim_coverage(
    claims: &[EssayFreezeClaim],
    register: &[RegisterRow],
) -> Result<(), GateError> {
    for row in register {
        if !claims.iter().any(|claim| claim.rows.contains(&row.id)) {
            return Err(GateError::UncoveredRow { id: row.id });
        }
    }
    Ok(())
}

/// Checks one statement against the register.
///
/// The function collects the register rows that the statement names. It then
/// applies [`check_claim`]. A statement that says "freeze" without a row fails
/// with [`GateError::MissingRowReference`].
///
/// # Errors
///
/// Returns a [`GateError`] when the statement has no freeze word or names no
/// register row.
pub fn check_freeze_statement(statement: &str, register: &[RegisterRow]) -> Result<(), GateError> {
    let rows = ContractId::ALL
        .iter()
        .copied()
        .filter(|id| statement.contains(id.as_str()))
        .collect();
    check_claim(&EssayFreezeClaim::new("inline", statement, rows), register)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::register::REGISTER;

    #[test]
    fn a_statement_with_a_row_passes() {
        assert_eq!(
            check_freeze_statement("Contract C2 freezes the snapshot bytes.", REGISTER),
            Ok(())
        );
    }

    #[test]
    fn a_freeze_without_a_row_fails() {
        assert_eq!(
            check_freeze_statement("This surface freezes the bytes.", REGISTER),
            Err(GateError::MissingRowReference {
                essay: "inline".to_owned()
            })
        );
    }

    #[test]
    fn a_frozen_statement_without_a_row_fails() {
        assert_eq!(
            check_freeze_statement("The bytes are frozen.", REGISTER),
            Err(GateError::MissingRowReference {
                essay: "inline".to_owned()
            })
        );
    }

    #[test]
    fn a_claim_that_lists_an_unknown_row_fails() {
        let claims = [EssayFreezeClaim::new(
            "99.md",
            "Contract C1 freezes the manifest.",
            vec![ContractId::C1, ContractId::A12],
        )];
        assert_eq!(
            check_claim(&claims[0], &REGISTER[1..]),
            Err(GateError::UnknownRow {
                essay: "99.md".to_owned(),
                id: ContractId::C1
            })
        );
    }

    #[test]
    fn a_claim_that_omits_the_row_text_fails() {
        let claim = EssayFreezeClaim::new(
            "99.md",
            "The manifest freezes the bytes.",
            vec![ContractId::C1],
        );
        assert_eq!(
            check_claim(&claim, REGISTER),
            Err(GateError::RowNotNamed {
                essay: "99.md".to_owned(),
                id: ContractId::C1
            })
        );
    }

    #[test]
    fn a_claim_without_a_freeze_word_fails() {
        let claim = EssayFreezeClaim::new("99.md", "Contract C1 is stable.", vec![ContractId::C1]);
        assert_eq!(
            check_claim(&claim, REGISTER),
            Err(GateError::NotAFreezeStatement {
                essay: "99.md".to_owned()
            })
        );
    }

    #[test]
    fn coverage_finds_an_uncovered_row() {
        let claims = [EssayFreezeClaim::new(
            "99.md",
            "Contract C1 freezes the manifest.",
            vec![ContractId::C1],
        )];
        assert_eq!(
            check_claim_coverage(&claims, REGISTER),
            Err(GateError::UncoveredRow { id: ContractId::C2 })
        );
    }
}
