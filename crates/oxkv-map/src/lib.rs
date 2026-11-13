//! Static shard map v1 for oxkv.
//!
//! The map assigns every user table to one data shard. It assigns `_catalog`
//! to the catalog shard. A table never splits across shards. Each shard entry
//! owns the half-open range `[P, successor(P))` of its table prefix.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses no clock, randomness, I/O, floating point, or `unsafe`.
//!
//! The map is a static configuration. It is not a placement control plane.
//!
//! # Boundary
//!
//! The key encoding crate is not in this worktree. [`bounds`] implements the
//! key encoding v1 successor rule. Replace that module with the key encoding
//! crate when the crate lands. The rule and the bytes do not change.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::{MapVersion, ShardId};
//! use oxkv_contracts::TableName;
//! use oxkv_map::ShardMap;
//!
//! let accounts = TableName::parse_user("accounts").expect("the name is valid");
//! let map = ShardMap::from_assignments(
//!     MapVersion::new(1),
//!     ShardId::new(vec![0]),
//!     [(ShardId::new(vec![1]), accounts)],
//! )
//! .expect("the map is valid");
//! assert_eq!(map.entries().len(), 2);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod bounds;
pub mod canonical;
pub mod entry;
pub mod error;
pub mod map;

pub use bounds::{successor, table_bounds, table_prefix};
pub use canonical::{CANONICAL_FORMAT_VERSION, MAP_MAGIC};
pub use entry::{ShardEntry, ShardRole};
pub use error::MapError;
pub use map::ShardMap;

use oxkv_contracts::version::MapSchemaVersion;

/// The canonical map schema text.
pub const MAP_SCHEMA: &str = "oxkv/map/v1";

/// The map schema version.
pub const MAP_SCHEMA_VERSION: MapSchemaVersion = MapSchemaVersion::new(1);
