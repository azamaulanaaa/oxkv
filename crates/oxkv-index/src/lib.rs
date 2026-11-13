//! Per-shard inverted index and frozen tokenizer contract for oxkv search.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses integer arithmetic only. It does not read a clock, draw randomness,
//! or perform I/O on a semantic path.
//!
//! The tokenizer contract fixes the identifier, version, Unicode data
//! version, normalization form, case folding rule, segmentation rule, and
//! stop-word list. Golden vectors pin the behaviour. A write and its postings
//! commit together. A replica builds its own index from scope members and
//! never mirrors the authority index.
//!
//! The modules are:
//!
//! - [`tokenizer`] holds the frozen tokenizer seam.
//! - [`posting`] holds postings, flags, the document map, and the `0x05` key.
//! - [`manifest`] holds the field, scorer, and budget declarations.
//! - [`index`] holds the immutable segments and the write transaction.
//! - [`search`] holds the pinned search over the inverted index.
//! - [`derive`] holds the derive-on-read oracle.
//! - [`scope`] holds the scope predicate and the replica build.
//! - [`score`] holds the document-local integer scorer.
//! - [`error`] holds the named errors.
//!
//! # Example
//!
//! ```
//! use oxkv_index::tokenizer::{TokenBudget, TokenizerIdentity, tokenize_text};
//!
//! let tokens = tokenize_text(
//!     TokenizerIdentity::unicode_word(),
//!     "Café, world!",
//!     TokenBudget::DEFAULT,
//! )
//! .expect("the text fits the token budget");
//! assert_eq!(tokens[0].text(), "café");
//! assert_eq!(tokens[1].text(), "world");
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

mod canonical;

pub mod derive;
pub mod error;
pub mod index;
pub mod manifest;
pub mod posting;
pub mod scope;
pub mod score;
pub mod search;
pub mod segment;
pub mod tokenizer;

pub use derive::{DerivedIndex, build_derived_index};
pub use error::IndexError;
pub use index::{CommitOutcome, ShardIndex, WriteTransaction};
pub use manifest::{FieldSpec, IndexManifest};
pub use posting::{
    DocumentField, FlagDecl, FlagSet, INDEX_NAMESPACE, IndexDocument, Posting, PostingFlags,
    PostingKey, StoredDocument, StoredField, TermFrequency,
};
pub use scope::{AllScope, Scope, build_scope_index};
pub use score::{ScorerKind, ScorerSpec};
pub use search::{MatchMode, SearchHit, SearchRequest, SearchResult, TermMatch};
pub use segment::Segment;
pub use tokenizer::{
    AsciiWordTokenizer, STOP_WORDS, TOKENIZER_CONTRACT_VERSION, Token, TokenBudget, TokenizeError,
    Tokenizer, TokenizerId, TokenizerIdentity, UnicodeWordTokenizer, tokenize_field, tokenize_text,
};
