//! Canonical key encoding v1 for oxkv.
//!
//! One logical key has one byte string. Lexicographic byte order equals the
//! semantic tuple order. A table with prefix `P` owns the half-open interval
//! `[P, successor(P))`.
//!
//! The encoding has this form:
//!
//! ```text
//! 0x01 || table-name || 0x00 || component...
//! 0x01 || table-name || 0x00 || 0xFF || system-id || component...
//! ```
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses no clock, randomness, I/O, floating point, or `unsafe`.
//!
//! # Example
//!
//! ```
//! use oxkv_key::{Component, Key, Table, TableBounds};
//!
//! let table = Table::parse_user("accounts").expect("the table name is valid");
//! let key = Key::new_user(
//!     table.clone(),
//!     vec![Component::text("alice"), Component::u64(42)],
//! )
//! .expect("the key is valid");
//! let bytes = key.encode().expect("the key encodes");
//! assert_eq!(Key::decode(&bytes).expect("the key decodes"), key);
//! assert!(TableBounds::new(&table).contains(&bytes));
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod bounds;
pub mod component;
pub mod error;
pub mod key;
pub mod schema;
pub mod table;

pub use bounds::{TableBounds, after_key, successor};
pub use component::{Component, ComponentType};
pub use error::{KeyError, KeyLimit};
pub use key::Key;
pub use schema::KeySchema;
pub use table::Table;

pub use oxkv_contracts::keyspace::{
    CATALOG_TABLE, KEY_ENCODING_VERSION, SYSTEM_ID_AUDIT, SYSTEM_ID_EVENT, SYSTEM_ID_INDEX,
    SYSTEM_ID_LEASE, SYSTEM_ID_PROJECTION, SYSTEM_ID_RECEIPT, SYSTEM_ID_STATISTICS, SYSTEM_MARKER,
    USER_COMPONENT_BOOL, USER_COMPONENT_BYTES, USER_COMPONENT_I64, USER_COMPONENT_TEXT,
    USER_COMPONENT_U64,
};

/// The maximum key size in bytes. The validator checks this limit first.
pub const MAX_KEY_BYTES: usize = 4096;
/// The maximum component count in one key.
pub const MAX_COMPONENTS: usize = 16;
/// The maximum payload size of one component in bytes.
pub const MAX_COMPONENT_BYTES: usize = 1024;
/// The maximum table name size in bytes.
pub const MAX_TABLE_NAME_BYTES: usize = 63;
