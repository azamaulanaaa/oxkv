//! The v1 `ShardSnapshot` and `ScopeSnapshot` byte contract.
//!
//! A snapshot has a canonical text header and a binary body. The body holds
//! independent pages and one page directory. Each page carries CRC32C and a
//! deterministic `page_id`. The directory carries a `BLAKE3` integrity value.
//! The content digest covers the directory and every page digest. A separate
//! content seal covers the content digest and the authority lease term.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses no floating point. It does not read a clock, draw randomness, use a
//! map order, or perform I/O. The same ordered records and parameters produce
//! the same bytes and digests.
//!
//! # Layout
//!
//! ```text
//! +------------------+  offset 0
//! | text header      |
//! +------------------+  directory_offset - body
//! | page 0           |
//! | page 1           |
//! +------------------+  directory_offset
//! | page directory   |
//! +------------------+  directory_offset + directory_length
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod codec;
pub mod directory;
pub mod error;
pub mod header;
pub mod page;
pub mod scope;
pub mod seal;
pub mod snapshot;

pub use codec::CodecId;
pub use directory::{DIRECTORY_MAGIC, Directory, DirectoryEntry};
pub use error::SnapshotError;
pub use header::{
    FEATURE_BLAKE3_CONTENT, FEATURE_CRC32C_PAGE, FEATURE_TEXT_HEADER, FORMAT_IDENTIFIER,
    FORMAT_MAJOR, FORMAT_MINOR, Header, HeaderIdentity, KNOWN_REQUIRED_FEATURES,
    SUPPORTED_KEY_ENCODING_VERSION, ScopeHeader, ShardHeader,
};
pub use page::{PAGE_MAGIC, Page, Record, page_id_for};
pub use scope::{ScopeAdmission, ScopeProjectionBuilder, forbidden_namespace, system_namespace_id};
pub use seal::{CONTENT_DIGEST_TAG, content_digest_for, verify_content_digest, verify_seal};
pub use snapshot::{
    ScopeShard, ScopeSnapshot, ScopeSnapshotParams, ShardSnapshot, ShardSnapshotParams,
    SnapshotView, read_snapshot, reseal,
};

/// The largest text header that a reader accepts.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

/// The largest encoded page frame that a reader accepts.
pub const MAX_PAGE_SIZE: u64 = 16 * 1024 * 1024;

/// The default target page payload size.
pub const DEFAULT_PAGE_SIZE: u64 = 64 * 1024;

/// The largest encoded key that a reader accepts.
pub const MAX_KEY_BYTES: usize = 4096;

/// The largest encoded value that a reader accepts.
pub const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;

/// The largest shard identifier that a reader accepts.
pub const MAX_SHARD_ID_BYTES: usize = 256;

/// The largest page count that a reader accepts.
pub const MAX_PAGES: u64 = 1_000_000;

/// The largest directory that a reader accepts.
pub const MAX_DIRECTORY_BYTES: u64 = 128 * 1024 * 1024;

/// The largest record count in one page that a reader accepts.
pub const MAX_RECORD_COUNT: u32 = 1_000_000;
