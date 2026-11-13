//! Snapshot-consistent search for oxkv.
//!
//! This crate implements the search read contract from essay 19. It provides:
//!
//! - A [`SearchPin`] with one version vector and one query fingerprint.
//! - Exact and coverage result modes with an explicit [`CoverageDescriptor`].
//! - One relevance comparator `(score descending, canonical key ascending)`.
//! - One declared comparator `(sort value ascending, canonical key ascending)`.
//! - Keyset pagination with a [`SearchCursor`] over the full sort tuple.
//! - A deterministic and bounded k-way merge over shard streams.
//! - Named errors for pin expiry, cursor mismatch, fan-out, index, tokenizer,
//!   token budget, and query budget.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It does not use floating point, a clock, randomness, I/O, or `unsafe`.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId, VersionVector};
//! use oxkv_search::{
//!     execute, QueryIdentity, QueryOrder, SearchHit, SearchPin, SearchRequest, ShardResponse,
//! };
//!
//! let shard = ShardId::new(vec![1]);
//! let versions = VersionVector::from_entries([(shard.clone(), CommitVersion::new(7))]);
//! let identity = QueryIdentity::new(
//!     b"term".to_vec(),
//!     b"scope".to_vec(),
//!     QueryOrder::Relevance,
//!     b"score".to_vec(),
//! );
//! let pin = SearchPin::from_identity(ScopeVersionVector::from_vector(versions), &identity);
//! let request = SearchRequest::new(pin, [shard.clone()]);
//! let hit = SearchHit::new(b"a".to_vec(), 10, shard.clone(), CommitVersion::new(5));
//! let responses = [ShardResponse::served(
//!     shard,
//!     CommitVersion::new(7),
//!     vec![hit],
//! )];
//!
//! let page = execute(&request, &responses).expect("the pin is readable");
//! assert_eq!(page.hits().len(), 1);
//! assert!(!page.is_partial());
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod budget;
pub mod coverage;
pub mod cursor;
pub mod engine;
pub mod error;
pub mod hit;
pub mod index;
pub mod merge;
pub mod pin;
pub mod result;
pub mod view;

pub use budget::QueryBudget;
pub use coverage::{CoverageDescriptor, ShardServing};
pub use cursor::{decode_cursor, encode_cursor, validate_cursor};
pub use engine::{SearchRequest, ShardResponse, execute};
pub use error::{CursorInvalidReason, SearchError};
pub use hit::{QueryOrder, SearchHit, SortPosition, compare_hits, compare_sort_values};
pub use index::{TokenBudget, verify_index_digest, verify_tokenizer_versions};
pub use oxkv_contracts::cursor::{QueryFingerprint, SearchCursor, SortTuple, SortValue};
pub use pin::{PinRetention, QUERY_FINGERPRINT_DOMAIN, QueryIdentity, SearchPin};
pub use result::{ResultMode, SearchPage};
pub use view::{RowVersion, get_at, get_value_at};
