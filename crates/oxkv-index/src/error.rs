//! Named errors for the inverted index and its tokenizer.
//!
//! Each error maps to one code in the canonical registry. A caller can match
//! on the error or on [`IndexError::code`]. A search never returns a shorter
//! result in place of an error.

use oxkv_contracts::version::CommitVersion;

use crate::tokenizer::{TokenizeError, TokenizerIdentity};

/// An error from an index operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IndexError {
    /// The index bytes do not match the index digest or a segment checksum.
    #[error("The index bytes do not match the index digest.")]
    IndexMismatch,
    /// The index uses one tokenizer and the request uses another.
    #[error("The index uses the tokenizer {found} but the request uses {expected}.")]
    TokenizerMismatch {
        /// The tokenizer that the request declares.
        expected: TokenizerIdentity,
        /// The tokenizer that the index records.
        found: TokenizerIdentity,
    },
    /// The tokenizer identity is not registered.
    #[error("The tokenizer is not registered: {id}.")]
    TokenizerUnknown {
        /// The tokenizer identity text.
        id: String,
    },
    /// The index would exceed its declared overhead budget.
    #[error("The index needs {used} bytes. The manifest allows {budget} bytes.")]
    OverheadExceeded {
        /// The prospective index size in bytes.
        used: u64,
        /// The declared overhead budget in bytes.
        budget: u64,
    },
    /// A field has more tokens than the budget allows.
    #[error("The field {field} has {count} tokens. The budget allows {limit} tokens.")]
    TokenCountExceeded {
        /// The field name.
        field: String,
        /// The token count.
        count: u64,
        /// The declared token cap.
        limit: u64,
    },
    /// A token is longer than the budget allows.
    #[error("A token in the field {field} has {length} bytes. The budget allows {limit} bytes.")]
    TokenLengthExceeded {
        /// The field name.
        field: String,
        /// The token length in bytes.
        length: u64,
        /// The declared token length cap.
        limit: u64,
    },
    /// A field is not declared as indexed.
    #[error("The field {field} is not declared as indexed.")]
    UnknownField {
        /// The undeclared field name.
        field: String,
    },
    /// A field is not searchable.
    #[error("The field {field} is not searchable.")]
    FieldNotSearchable {
        /// The field name.
        field: String,
    },
    /// A flag bit is not declared for the field.
    #[error("The flag bit {bit} is not declared for the field {field}.")]
    UnknownFlag {
        /// The field name.
        field: String,
        /// The undeclared bit.
        bit: u8,
    },
    /// A document key is outside the shard prefix range.
    #[error("The document key is outside the shard prefix range.")]
    OutOfScope {
        /// The rejected key.
        key: Vec<u8>,
    },
    /// A document is malformed.
    #[error("The document is malformed: {reason}.")]
    MalformedDocument {
        /// The reason text.
        reason: String,
    },
    /// A manifest name is declared more than one time.
    #[error("The name {name} is declared more than one time.")]
    DuplicateName {
        /// The duplicate name.
        name: String,
    },
    /// The requested pin is not retained.
    #[error(
        "The pin {requested} is not readable. The current commit version is {current} and the oldest retained commit version is {oldest}."
    )]
    PinUnavailable {
        /// The requested commit version.
        requested: CommitVersion,
        /// The current commit version.
        current: CommitVersion,
        /// The oldest retained commit version.
        oldest: CommitVersion,
    },
    /// A write commit version does not advance the index.
    #[error(
        "The commit version {requested} is not greater than the current commit version {current}."
    )]
    VersionNotMonotonic {
        /// The current commit version.
        current: CommitVersion,
        /// The requested commit version.
        requested: CommitVersion,
    },
    /// The segment schema does not match the current manifest.
    #[error("The segment schema does not match the current manifest.")]
    ManifestDrift,
    /// The artifact version is not supported.
    #[error("The index format version {found} is not supported. Expected {expected}.")]
    UnsupportedVersion {
        /// The supported format version.
        expected: u16,
        /// The found format version.
        found: u16,
    },
    /// The manifest declaration is invalid.
    #[error("The index manifest is invalid: {reason}.")]
    InvalidManifest {
        /// The reason text.
        reason: String,
    },
    /// The scorer declaration is invalid.
    #[error("The scorer declaration is invalid: {reason}.")]
    InvalidScorer {
        /// The reason text.
        reason: String,
    },
    /// A write transaction has no operation.
    #[error("The write transaction has no operation.")]
    EmptyWrite,
}

impl IndexError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::IndexMismatch => "search.index_mismatch",
            Self::TokenizerMismatch { .. } => "search.tokenizer_mismatch",
            Self::TokenizerUnknown { .. } => "search.tokenizer_unknown",
            Self::OverheadExceeded { .. } => "index.overhead_exceeded",
            Self::TokenCountExceeded { .. } | Self::TokenLengthExceeded { .. } => {
                "search.token_budget_exceeded"
            }
            Self::UnknownField { .. }
            | Self::FieldNotSearchable { .. }
            | Self::UnknownFlag { .. } => "manifest.index_unknown_field",
            Self::OutOfScope { .. } => "key.outside_table",
            Self::MalformedDocument { .. } => "key.malformed",
            Self::DuplicateName { .. } => "manifest.duplicate_name",
            Self::PinUnavailable { .. } => "search.pin_expired",
            Self::VersionNotMonotonic { .. } | Self::EmptyWrite => "internal.invariant_violation",
            Self::ManifestDrift => "manifest.artifact_drift",
            Self::UnsupportedVersion { .. } => "format.unsupported_version",
            Self::InvalidManifest { .. } => "manifest.unsupported_schema",
            Self::InvalidScorer { .. } => "manifest.float_not_allowed",
        }
    }
}

impl From<(String, TokenizeError)> for IndexError {
    fn from((field, error): (String, TokenizeError)) -> Self {
        match error {
            TokenizeError::TokenCountExceeded { count, limit } => Self::TokenCountExceeded {
                field,
                count,
                limit,
            },
            TokenizeError::TokenLengthExceeded { length, limit } => Self::TokenLengthExceeded {
                field,
                length,
                limit,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::error::lookup;

    use crate::tokenizer::TokenizerId;

    fn sample_errors() -> Vec<IndexError> {
        let identity = TokenizerIdentity::v1(TokenizerId::UnicodeWord);
        vec![
            IndexError::IndexMismatch,
            IndexError::TokenizerMismatch {
                expected: identity,
                found: identity,
            },
            IndexError::TokenizerUnknown {
                id: "unknown/v1".to_owned(),
            },
            IndexError::OverheadExceeded { used: 2, budget: 1 },
            IndexError::TokenCountExceeded {
                field: "title".to_owned(),
                count: 2,
                limit: 1,
            },
            IndexError::TokenLengthExceeded {
                field: "title".to_owned(),
                length: 2,
                limit: 1,
            },
            IndexError::UnknownField {
                field: "title".to_owned(),
            },
            IndexError::FieldNotSearchable {
                field: "title".to_owned(),
            },
            IndexError::UnknownFlag {
                field: "title".to_owned(),
                bit: 1,
            },
            IndexError::OutOfScope { key: vec![1] },
            IndexError::MalformedDocument {
                reason: "empty".to_owned(),
            },
            IndexError::DuplicateName {
                name: "title".to_owned(),
            },
            IndexError::PinUnavailable {
                requested: CommitVersion::new(2),
                current: CommitVersion::new(1),
                oldest: CommitVersion::new(1),
            },
            IndexError::VersionNotMonotonic {
                current: CommitVersion::new(1),
                requested: CommitVersion::new(1),
            },
            IndexError::ManifestDrift,
            IndexError::UnsupportedVersion {
                expected: 1,
                found: 2,
            },
            IndexError::InvalidManifest {
                reason: "empty".to_owned(),
            },
            IndexError::InvalidScorer {
                reason: "empty".to_owned(),
            },
            IndexError::EmptyWrite,
        ]
    }

    #[test]
    fn every_error_code_is_registered() {
        for error in sample_errors() {
            assert!(
                lookup(error.code()).is_some(),
                "the code {} is not registered",
                error.code()
            );
        }
    }
}
