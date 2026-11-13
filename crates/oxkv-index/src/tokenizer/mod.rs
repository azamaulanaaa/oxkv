//! The frozen tokenizer contract.
//!
//! [`TokenizerIdentity`] names one tokenizer by identifier, contract version,
//! and Unicode data version. [`Tokenizer`] turns text into tokens with a
//! position per token. [`tokenize_text`] is the dispatch entry point.
//!
//! The contract has two in-tree implementations. [`UnicodeWordTokenizer`] is
//! the frozen version 1 tokenizer for search. [`AsciiWordTokenizer`] is the
//! second implementation and the oracle for ASCII corpora.
//!
//! The tokenizer performs no I/O. It reads no clock, draws no randomness, and
//! uses no floating point. The same bytes produce the same tokens on native
//! and `wasm32-unknown-unknown`.

use crate::error::IndexError;

pub mod ascii;
pub mod identity;
pub mod normalize;
pub(crate) mod tables;
pub mod unicode;

pub use ascii::AsciiWordTokenizer;
pub use identity::{
    ASCII_CASE_FOLDING, ASCII_NORMALIZATION, ASCII_SEGMENTATION, NO_UNICODE_DATA_VERSION,
    STOP_WORDS, TOKENIZER_CONTRACT_VERSION, TokenizerId, TokenizerIdentity, UNICODE_CASE_FOLDING,
    UNICODE_DATA_VERSION, UNICODE_NORMALIZATION, UNICODE_SEGMENTATION,
};
pub use unicode::UnicodeWordTokenizer;

/// An error from a token budget check.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenizeError {
    /// The text has more tokens than the budget allows.
    #[error("The text has {count} tokens. The budget allows {limit} tokens.")]
    TokenCountExceeded {
        /// The token count.
        count: u64,
        /// The declared cap.
        limit: u64,
    },
    /// A token is longer than the budget allows.
    #[error("A token has {length} bytes. The budget allows {limit} bytes.")]
    TokenLengthExceeded {
        /// The token length in bytes.
        length: u64,
        /// The declared cap.
        limit: u64,
    },
}

impl TokenizeError {
    /// Returns the canonical error code.
    #[must_use]
    #[allow(clippy::unused_self)] // The method mirrors `IndexError::code`.
    pub const fn code(&self) -> &'static str {
        "search.token_budget_exceeded"
    }

    /// Converts the error to a field-specific index error.
    #[must_use]
    pub fn into_index_error(self, field: &str) -> IndexError {
        match self {
            Self::TokenCountExceeded { count, limit } => IndexError::TokenCountExceeded {
                field: field.to_owned(),
                count,
                limit,
            },
            Self::TokenLengthExceeded { length, limit } => IndexError::TokenLengthExceeded {
                field: field.to_owned(),
                length,
                limit,
            },
        }
    }
}

/// The declared token budgets for one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBudget {
    /// The maximum number of tokens in one field.
    pub max_tokens_per_field: u32,
    /// The maximum byte length of one token.
    pub max_token_bytes: u32,
}

impl TokenBudget {
    /// The default budget for a small search field.
    pub const DEFAULT: Self = Self {
        max_tokens_per_field: 256,
        max_token_bytes: 64,
    };

    /// Creates a token budget.
    #[must_use]
    pub const fn new(max_tokens_per_field: u32, max_token_bytes: u32) -> Self {
        Self {
            max_tokens_per_field,
            max_token_bytes,
        }
    }
}

/// One token from the tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    text: String,
    position: u32,
}

impl Token {
    /// Creates a token.
    #[must_use]
    pub fn new(text: impl Into<String>, position: u32) -> Self {
        Self {
            text: text.into(),
            position,
        }
    }

    /// Returns the token text after normalization and case folding.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the zero-based token position in the field.
    #[must_use]
    pub const fn position(&self) -> u32 {
        self.position
    }
}

/// The tokenizer seam.
pub trait Tokenizer {
    /// Returns the frozen identity of this tokenizer.
    fn identity(&self) -> TokenizerIdentity;

    /// Tokenizes one field.
    ///
    /// # Errors
    ///
    /// Returns [`TokenizeError`] when the token count or a token length
    /// exceeds the declared budget.
    fn tokenize(&self, text: &str, budget: TokenBudget) -> Result<Vec<Token>, TokenizeError>;
}

/// Tokenizes text with the tokenizer named by the identity.
///
/// # Errors
///
/// Returns [`IndexError::TokenizerUnknown`] when the identity is not
/// registered. Returns [`IndexError::TokenCountExceeded`] or
/// [`IndexError::TokenLengthExceeded`] when the text exceeds the budget. The
/// field name in a budget error is `<query>`.
pub fn tokenize_text(
    identity: TokenizerIdentity,
    text: &str,
    budget: TokenBudget,
) -> Result<Vec<Token>, IndexError> {
    tokenize_field("<query>", identity, text, budget)
}

/// Tokenizes one named field with the tokenizer named by the identity.
///
/// # Errors
///
/// Returns [`IndexError::TokenizerUnknown`] when the identity is not
/// registered. Returns [`IndexError::TokenCountExceeded`] or
/// [`IndexError::TokenLengthExceeded`] when the text exceeds the budget.
pub fn tokenize_field(
    field: &str,
    identity: TokenizerIdentity,
    text: &str,
    budget: TokenBudget,
) -> Result<Vec<Token>, IndexError> {
    identity.validate()?;
    let result = match identity.id() {
        TokenizerId::AsciiWord => AsciiWordTokenizer::new().tokenize(text, budget),
        TokenizerId::UnicodeWord => UnicodeWordTokenizer::new().tokenize(text, budget),
    };
    result.map_err(|error| error.into_index_error(field))
}

pub(crate) fn push_token(
    tokens: &mut Vec<Token>,
    text: String,
    budget: TokenBudget,
) -> Result<(), TokenizeError> {
    let count = u64::try_from(tokens.len()).unwrap_or(u64::MAX) + 1;
    if count > u64::from(budget.max_tokens_per_field) {
        return Err(TokenizeError::TokenCountExceeded {
            count,
            limit: u64::from(budget.max_tokens_per_field),
        });
    }
    let length = u64::try_from(text.len()).unwrap_or(u64::MAX);
    if length > u64::from(budget.max_token_bytes) {
        return Err(TokenizeError::TokenLengthExceeded {
            length,
            limit: u64::from(budget.max_token_bytes),
        });
    }
    let position = u32::try_from(tokens.len()).map_err(|_| TokenizeError::TokenCountExceeded {
        count,
        limit: u64::from(u32::MAX),
    })?;
    tokens.push(Token { text, position });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_pins_every_contract_field() {
        let identity = TokenizerIdentity::unicode_word();
        assert_eq!(identity.id(), TokenizerId::UnicodeWord);
        assert_eq!(identity.version().revision(), 1);
        assert_eq!(identity.unicode_version(), UNICODE_DATA_VERSION);
        assert_eq!(identity.normalization(), UNICODE_NORMALIZATION);
        assert_eq!(identity.case_folding(), UNICODE_CASE_FOLDING);
        assert_eq!(identity.segmentation(), UNICODE_SEGMENTATION);
        assert_eq!(identity.stop_words(), &[] as &[&str]);
    }

    #[test]
    fn an_unknown_tokenizer_fails() {
        assert_eq!(
            TokenizerIdentity::from_parts("unknown-word", 1, "none"),
            Err(IndexError::TokenizerUnknown {
                id: "unknown-word".to_owned()
            })
        );
        assert_eq!(
            TokenizerIdentity::from_parts("unicode-word", 2, UNICODE_DATA_VERSION),
            Err(IndexError::TokenizerUnknown {
                id: "unicode-word/v2 (16.0.0)".to_owned()
            })
        );
        assert_eq!(
            TokenizerIdentity::from_parts("unicode-word", 1, "15.0.0"),
            Err(IndexError::TokenizerUnknown {
                id: "unicode-word/v1 (15.0.0)".to_owned()
            })
        );
    }

    #[test]
    fn the_fingerprint_covers_the_identity() {
        let unicode = TokenizerIdentity::unicode_word();
        let ascii = TokenizerIdentity::ascii_word();
        assert_ne!(unicode.fingerprint(), ascii.fingerprint());
        assert_eq!(
            unicode.fingerprint(),
            TokenizerIdentity::unicode_word().fingerprint()
        );
    }

    #[test]
    fn a_token_budget_zero_rejects_a_token() {
        let budget = TokenBudget::new(0, 64);
        let error = tokenize_text(TokenizerIdentity::unicode_word(), "a", budget)
            .expect_err("the token is over budget");
        assert_eq!(error.code(), "search.token_budget_exceeded");
    }

    #[test]
    fn a_token_length_budget_is_enforced() {
        let budget = TokenBudget::new(4, 1);
        let error = tokenize_text(TokenizerIdentity::unicode_word(), "ab", budget)
            .expect_err("the token is too long");
        assert_eq!(error.code(), "search.token_budget_exceeded");
    }
}
