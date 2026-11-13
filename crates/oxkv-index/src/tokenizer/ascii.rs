//! The ASCII word tokenizer.
//!
//! The tokenizer starts a token at an ASCII letter, an ASCII digit, or the low
//! line. It continues the token over the same set and lowercases ASCII
//! letters. Every other byte separates tokens. A non-ASCII byte never starts a
//! token.

use super::{
    Token, TokenBudget, TokenizeError, Tokenizer, TokenizerId, TokenizerIdentity, push_token,
};

/// The ASCII word tokenizer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AsciiWordTokenizer;

impl AsciiWordTokenizer {
    /// Creates the tokenizer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Tokenizer for AsciiWordTokenizer {
    fn identity(&self) -> TokenizerIdentity {
        TokenizerIdentity::v1(TokenizerId::AsciiWord)
    }

    fn tokenize(&self, text: &str, budget: TokenBudget) -> Result<Vec<Token>, TokenizeError> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        for byte in text.bytes() {
            if is_ascii_word(byte) {
                current.push(char::from(byte.to_ascii_lowercase()));
            } else if !current.is_empty() {
                push_token(&mut tokens, core::mem::take(&mut current), budget)?;
            }
        }
        if !current.is_empty() {
            push_token(&mut tokens, current, budget)?;
        }
        Ok(tokens)
    }
}

const fn is_ascii_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str) -> Vec<String> {
        AsciiWordTokenizer::new()
            .tokenize(text, TokenBudget::DEFAULT)
            .expect("the text fits the default budget")
            .into_iter()
            .map(|token| token.text().to_owned())
            .collect()
    }

    #[test]
    fn empty_text_has_no_tokens() {
        assert_eq!(texts(""), Vec::<String>::new());
    }

    #[test]
    fn ascii_words_are_lowercased() {
        assert_eq!(texts("Hello, WORLD!"), ["hello", "world"]);
    }

    #[test]
    fn non_ascii_bytes_separate_tokens() {
        assert_eq!(texts("caf\u{00e9}"), ["caf"]);
        assert_eq!(texts("\u{65e5}\u{672c}\u{8a9e}"), Vec::<String>::new());
    }

    #[test]
    fn underscore_is_a_word_character() {
        assert_eq!(texts("foo_bar"), ["foo_bar"]);
    }
}
