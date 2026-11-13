//! The Unicode word tokenizer.
//!
//! The tokenizer normalizes text to NFC. It starts a token at a letter, a
//! number, or connector punctuation. It continues the token over letters,
//! marks, numbers, and connector punctuation. It applies full Unicode case
//! folding to each token. Every other character separates tokens.

use super::{
    Token, TokenBudget, TokenizeError, Tokenizer, TokenizerId, TokenizerIdentity, normalize,
    push_token,
};

/// The Unicode word tokenizer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnicodeWordTokenizer;

impl UnicodeWordTokenizer {
    /// Creates the tokenizer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Tokenizer for UnicodeWordTokenizer {
    fn identity(&self) -> TokenizerIdentity {
        TokenizerIdentity::v1(TokenizerId::UnicodeWord)
    }

    fn tokenize(&self, text: &str, budget: TokenBudget) -> Result<Vec<Token>, TokenizeError> {
        let normalized = normalize::normalize_nfc(text);
        let mut tokens = Vec::new();
        let mut current = String::new();
        for character in normalized.chars() {
            if normalize::is_word_continue(character) {
                if current.is_empty() && !normalize::is_word_start(character) {
                    continue;
                }
                current.push(character);
            } else if !current.is_empty() {
                push_token(&mut tokens, fold_token(&current), budget)?;
                current.clear();
            }
        }
        if !current.is_empty() {
            push_token(&mut tokens, fold_token(&current), budget)?;
        }
        Ok(tokens)
    }
}

fn fold_token(raw: &str) -> String {
    let mut folded = String::with_capacity(raw.len());
    for character in raw.chars() {
        normalize::fold_into(character, &mut folded);
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str) -> Vec<String> {
        UnicodeWordTokenizer::new()
            .tokenize(text, TokenBudget::DEFAULT)
            .expect("the text fits the default budget")
            .into_iter()
            .map(|token| token.text().to_owned())
            .collect()
    }

    #[test]
    fn empty_text_has_no_tokens() {
        assert_eq!(texts(""), Vec::<String>::new());
        assert_eq!(texts("   ,.!?   "), Vec::<String>::new());
    }

    #[test]
    fn ascii_words_are_lowercased() {
        assert_eq!(texts("Hello, WORLD!"), ["hello", "world"]);
    }

    #[test]
    fn multibyte_words_are_one_token() {
        assert_eq!(texts("日本語 テスト"), ["日本語", "テスト"]);
        assert_eq!(texts("Café"), ["café"]);
    }

    #[test]
    fn combining_marks_stay_in_the_token() {
        assert_eq!(texts("e\u{0301}"), ["\u{00e9}"]);
        assert_eq!(texts("e\u{0301}\u{0323}"), ["\u{1eb9}\u{0301}"]);
        assert_eq!(texts("\u{0915}\u{093c}"), ["\u{0915}\u{093c}"]);
    }

    #[test]
    fn case_folding_is_full() {
        assert_eq!(texts("Straße"), ["strasse"]);
        assert_eq!(texts("MASSE"), ["masse"]);
        assert_eq!(texts("Maße"), ["masse"]);
        assert_eq!(texts("\u{0130}stanbul"), ["i\u{0307}stanbul"]);
    }

    #[test]
    fn segmentation_uses_word_runs() {
        assert_eq!(texts("foo_bar one2three"), ["foo_bar", "one2three"]);
        assert_eq!(texts("hello,world"), ["hello", "world"]);
        assert_eq!(texts("a-b_c"), ["a", "b_c"]);
    }

    #[test]
    fn positions_are_sequential() {
        let tokens = UnicodeWordTokenizer::new()
            .tokenize("one two three", TokenBudget::DEFAULT)
            .expect("the text fits the default budget");
        assert_eq!(
            tokens.iter().map(Token::position).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }
}
