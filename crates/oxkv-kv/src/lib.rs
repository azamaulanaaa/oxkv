//! The portable ordered key-value substrate for oxkv.
//!
//! A shard is one totally ordered map from immutable byte keys to versioned
//! byte values. The comparator is lexicographic over unsigned bytes. The
//! order does not depend on locale, platform, pointer width, or engine.
//!
//! The crate provides:
//!
//! - Versioned point reads and atomic single-shard batches.
//! - Inclusive range scans in both directions.
//! - Bidirectional cursors with a pinned commit version and a resume key.
//! - Two in-tree engines behind [`OrderedMap`]: [`BTreeEngine`] and
//!   [`LsmEngine`].
//!
//! The crate is pure. It does not read a clock, draw randomness, use floating
//! point, or perform I/O on the semantic path. It has no `unsafe` code.
//!
//! # Persistence
//!
//! This crate does not persist data. It converts a batch to canonical bytes
//! with [`Batch::to_bytes`] and converts a cursor to canonical bytes with
//! [`Cursor::to_token`]. Persistence is owned by the existing LSM backend in
//! the `oxkv` crate. A caller that needs a durable store uses that backend.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::{CommitVersion, ShardId};
//! use oxkv_contracts::CursorDirection;
//! use oxkv_kv::{Batch, BTreeEngine, KeyRange, Limits, Operation, OrderedMap};
//!
//! let shard = ShardId::new(b"shard-1".to_vec());
//! let mut engine = BTreeEngine::new(shard.clone(), Limits::default());
//! engine
//!     .commit(Batch::new(
//!         shard,
//!         CommitVersion::new(1),
//!         vec![Operation::put(b"a".to_vec(), b"1".to_vec())],
//!     ))
//!     .expect("the batch commits");
//!
//! let value = engine
//!     .get(CommitVersion::new(1), b"a")
//!     .expect("the read succeeds");
//! assert_eq!(value, Some(b"1".to_vec()));
//!
//! let entries = engine
//!     .scan(CommitVersion::new(1), &KeyRange::all(), CursorDirection::Ascending)
//!     .expect("the scan succeeds");
//! assert_eq!(entries.len(), 1);
//! ```

#![forbid(unsafe_code)]

mod codec;

pub mod batch;
pub mod cursor;
pub mod engine;
pub mod entry;
pub mod error;
pub mod limits;
pub mod operation;
pub mod page;
pub mod range;

pub use batch::Batch;
pub use cursor::Cursor;
pub use engine::{BTreeEngine, LsmEngine, OrderedMap};
pub use entry::Entry;
pub use error::KvError;
pub use limits::Limits;
pub use operation::Operation;
pub use page::Page;
pub use range::KeyRange;
