//! The frozen tokenizer identity and its versioned contract fields.
//!
//! The identity records the identifier, the contract version, and the Unicode
//! data version. The contract also fixes the normalization form, the case
//! folding rule, the segmentation rule, and the stop-word list.
//!
//! A new behaviour requires a new identifier or a new version. A reader must
//! reject an identity that it does not know.

use core::fmt;

use oxkv_contracts::version::{Hash32, TokenizerVersion};

use crate::canonical;
use crate::error::IndexError;

/// The Unicode Character Database version that the Unicode tokenizer uses.
pub const UNICODE_DATA_VERSION: &str = "16.0.0";

/// The Unicode data version text for a tokenizer that does not use Unicode.
pub const NO_UNICODE_DATA_VERSION: &str = "none";

/// The normalization form of the ASCII tokenizer.
pub const ASCII_NORMALIZATION: &str = "none";

/// The normalization form of the Unicode tokenizer.
pub const UNICODE_NORMALIZATION: &str = "nfc";

/// The case folding rule of the ASCII tokenizer.
pub const ASCII_CASE_FOLDING: &str = "ascii-lower";

/// The case folding rule of the Unicode tokenizer.
pub const UNICODE_CASE_FOLDING: &str = "full";

/// The segmentation rule of the ASCII tokenizer.
pub const ASCII_SEGMENTATION: &str = "ascii-word-run";

/// The segmentation rule of the Unicode tokenizer.
pub const UNICODE_SEGMENTATION: &str = "unicode-word-run";

/// The stop-word list. Tokenizer version 1 has no stop words.
pub const STOP_WORDS: &[&str] = &[];

/// The frozen tokenizer contract version.
pub const TOKENIZER_CONTRACT_VERSION: u16 = 1;

/// A registered tokenizer identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenizerId {
    /// The ASCII word tokenizer.
    AsciiWord,
    /// The Unicode word tokenizer.
    UnicodeWord,
}

impl TokenizerId {
    /// Every registered tokenizer identifier.
    pub const ALL: &'static [Self] = &[Self::AsciiWord, Self::UnicodeWord];

    /// Returns the canonical identifier text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AsciiWord => "ascii-word",
            Self::UnicodeWord => "unicode-word",
        }
    }

    /// Returns the identifier for a canonical name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "ascii-word" => Some(Self::AsciiWord),
            "unicode-word" => Some(Self::UnicodeWord),
            _ => None,
        }
    }

    /// Returns the Unicode data version of the tokenizer.
    #[must_use]
    pub const fn unicode_version(self) -> &'static str {
        match self {
            Self::AsciiWord => NO_UNICODE_DATA_VERSION,
            Self::UnicodeWord => UNICODE_DATA_VERSION,
        }
    }

    /// Returns the normalization form of the tokenizer.
    #[must_use]
    pub const fn normalization(self) -> &'static str {
        match self {
            Self::AsciiWord => ASCII_NORMALIZATION,
            Self::UnicodeWord => UNICODE_NORMALIZATION,
        }
    }

    /// Returns the case folding rule of the tokenizer.
    #[must_use]
    pub const fn case_folding(self) -> &'static str {
        match self {
            Self::AsciiWord => ASCII_CASE_FOLDING,
            Self::UnicodeWord => UNICODE_CASE_FOLDING,
        }
    }

    /// Returns the segmentation rule of the tokenizer.
    #[must_use]
    pub const fn segmentation(self) -> &'static str {
        match self {
            Self::AsciiWord => ASCII_SEGMENTATION,
            Self::UnicodeWord => UNICODE_SEGMENTATION,
        }
    }

    /// Returns the stop-word list of the tokenizer.
    #[must_use]
    #[allow(clippy::unused_self)] // Version 1 has no stop words for any tokenizer.
    pub const fn stop_words(self) -> &'static [&'static str] {
        STOP_WORDS
    }
}

impl fmt::Display for TokenizerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One frozen tokenizer identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenizerIdentity {
    id: TokenizerId,
    version: TokenizerVersion,
    unicode_version: &'static str,
}

impl TokenizerIdentity {
    /// Returns the frozen version 1 identity for one tokenizer.
    #[must_use]
    pub const fn v1(id: TokenizerId) -> Self {
        Self {
            id,
            version: TokenizerVersion::new(TOKENIZER_CONTRACT_VERSION),
            unicode_version: id.unicode_version(),
        }
    }

    /// Returns the frozen Unicode word identity.
    #[must_use]
    pub const fn unicode_word() -> Self {
        Self::v1(TokenizerId::UnicodeWord)
    }

    /// Returns the frozen ASCII word identity.
    #[must_use]
    pub const fn ascii_word() -> Self {
        Self::v1(TokenizerId::AsciiWord)
    }

    /// Creates an identity from the stored parts.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::TokenizerUnknown`] when the identifier, the
    /// contract version, or the Unicode data version is not registered.
    pub fn from_parts(id: &str, version: u16, unicode_version: &str) -> Result<Self, IndexError> {
        let Some(tokenizer_id) = TokenizerId::from_name(id) else {
            return Err(IndexError::TokenizerUnknown { id: id.to_owned() });
        };
        if version != TOKENIZER_CONTRACT_VERSION
            || unicode_version != tokenizer_id.unicode_version()
        {
            return Err(IndexError::TokenizerUnknown {
                id: format!("{id}/v{version} ({unicode_version})"),
            });
        }
        Ok(Self {
            id: tokenizer_id,
            version: TokenizerVersion::new(version),
            unicode_version: tokenizer_id.unicode_version(),
        })
    }

    /// Checks that the identity is registered.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::TokenizerUnknown`] when the contract version or
    /// the Unicode data version is not registered.
    pub fn validate(self) -> Result<(), IndexError> {
        if self.version.revision() != TOKENIZER_CONTRACT_VERSION
            || self.unicode_version != self.id.unicode_version()
        {
            return Err(IndexError::TokenizerUnknown {
                id: self.to_string(),
            });
        }
        Ok(())
    }

    /// Returns the tokenizer identifier.
    #[must_use]
    pub const fn id(self) -> TokenizerId {
        self.id
    }

    /// Returns the contract version.
    #[must_use]
    pub const fn version(self) -> TokenizerVersion {
        self.version
    }

    /// Returns the Unicode data version.
    #[must_use]
    pub const fn unicode_version(self) -> &'static str {
        self.unicode_version
    }

    /// Returns the normalization form.
    #[must_use]
    pub const fn normalization(self) -> &'static str {
        self.id.normalization()
    }

    /// Returns the case folding rule.
    #[must_use]
    pub const fn case_folding(self) -> &'static str {
        self.id.case_folding()
    }

    /// Returns the segmentation rule.
    #[must_use]
    pub const fn segmentation(self) -> &'static str {
        self.id.segmentation()
    }

    /// Returns the stop-word list.
    #[must_use]
    pub const fn stop_words(self) -> &'static [&'static str] {
        self.id.stop_words()
    }

    /// Returns a deterministic fingerprint of every contract field.
    #[must_use]
    pub fn fingerprint(self) -> Hash32 {
        let mut bytes = Vec::new();
        push_text(&mut bytes, "oxkv/tokenizer/v1");
        push_text(&mut bytes, self.id.as_str());
        bytes.extend_from_slice(&self.version.revision().to_be_bytes());
        push_text(&mut bytes, self.unicode_version);
        push_text(&mut bytes, self.normalization());
        push_text(&mut bytes, self.case_folding());
        push_text(&mut bytes, self.segmentation());
        let stop_words = self.stop_words();
        let count = u32::try_from(stop_words.len()).unwrap_or(u32::MAX);
        bytes.extend_from_slice(&count.to_be_bytes());
        for stop_word in stop_words {
            push_text(&mut bytes, stop_word);
        }
        Hash32::of(&bytes)
    }

    /// Encodes the identity for a segment header.
    pub(crate) fn encode(self, output: &mut Vec<u8>) {
        canonical::text(output, self.id.as_str());
        canonical::u16(output, self.version.revision());
        canonical::text(output, self.unicode_version);
    }
}

impl fmt::Display for TokenizerIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.id, self.version)
    }
}

fn push_text(bytes: &mut Vec<u8>, text: &str) {
    let length = u32::try_from(text.len()).unwrap_or(u32::MAX);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(text.as_bytes());
}
