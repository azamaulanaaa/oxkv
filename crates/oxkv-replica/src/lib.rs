//! Read-only client replica for oxkv.
//!
//! A replica holds one policy-filtered scope. It stores authority
//! `ScopeSnapshot` pages and the scope-projected user rows and projection facts.
//! It builds a local search index from scope members. It keeps a monotonic
//! applied version vector and answers reads and searches at that local pin.
//!
//! The replica is portable. It compiles for native and
//! `wasm32-unknown-unknown`. It uses no network, clock, randomness, I/O,
//! floating point, or `unsafe`. The caller injects time and transport.
//!
//! # The hydration journal seam
//!
//! The core takes a [`HydrationJournalStore`] seam. It holds no I/O. The
//! [`MemoryJournalStore`] adapter serves tests. The `FileJournalStore` adapter
//! is native-only and uses `std::fs` with a caller-supplied path. A wasm32
//! caller supplies its own store over the host storage API.
//!
//! # What the replica does not store
//!
//! - A user row outside its scope.
//! - A receipt that belongs to another client.
//! - An authority event, audit record, or lease record.
//! - The authority per-shard search index.
//!
//! # What the replica does not do
//!
//! - It does not write. It has no write engine.
//! - It does not hold an authority write lease.
//! - It does not mirror authority index bytes.
//! - It does not re-encode or re-block an authority page.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::{
//!     ManifestDigest, MapDigest, MapVersion, PolicyVersion, ScopeIdentity, TokenizerVersion,
//! };
//! use oxkv_replica::{
//!     DeviceBudget, Pin, ReadRequest, Replica, ReplicaConfig, ReplicaIdentity, Scope, ScopePin,
//!     StalenessBound,
//! };
//!
//! let pin = ScopePin::new(
//!     ManifestDigest::from_bytes([1; 32]),
//!     PolicyVersion::from_bytes([2; 32]),
//!     MapVersion::new(3),
//!     MapDigest::from_bytes([4; 32]),
//!     ScopeIdentity::from_bytes([5; 32]),
//! );
//! let scope = Scope::new(pin, TokenizerVersion::new(1), [], []);
//! let config = ReplicaConfig::new(
//!     ReplicaIdentity::new(b"origin".to_vec()),
//!     DeviceBudget::new(1 << 20, 1 << 20),
//! );
//! let replica = Replica::new(config, scope);
//! let request = ReadRequest::new(
//!     b"\x01accounts\x00\x10alice".to_vec(),
//!     Pin::at_least(Default::default()),
//!     StalenessBound::Unbounded,
//! );
//! assert_eq!(
//!     replica.get(&request).expect_err("a cold replica is not empty").code(),
//!     "read.not_hydrated"
//! );
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

mod codec;
pub mod error;
pub mod index;
pub mod journal;
pub mod mirror;
pub mod outbox;
pub mod page;
pub mod read;
pub mod replica;
pub mod residency;
pub mod scope;
pub mod tabs;
mod versions;

pub use error::ReplicaError;
pub use index::{
    INDEX_VERSION, IndexHeader, IndexVersion, ReplicaIndex, Scorer, SearchHit, tokenize,
};
#[cfg(not(target_arch = "wasm32"))]
pub use journal::FileJournalStore;
pub use journal::{HydrationJournal, HydrationJournalStore, HydrationPhase, MemoryJournalStore};
pub use mirror::{DiffFrame, FactEntry, Ingest, Mirror, RowEntry};
pub use outbox::{Outbox, OutboxRecord};
pub use page::{
    MirroredRecord, PageDescriptor, PageHeader, RecordBody, SUPPORTED_PAGE_VERSION, ScopeSnapshot,
    ScopeSnapshotPage, snapshot_digest,
};
pub use read::{
    HydrationState, Pin, ReadRequest, ReadResponse, ReplicaStatus, SearchRequest, SearchResponse,
    StalenessBound,
};
pub use replica::{HydrationReport, HydrationTarget, Replica, ReplicaConfig};
pub use residency::{DeviceBudget, EvictionReport, ResidencyReport, ScopeFootprint};
pub use scope::{Scope, ScopePin, user_key_table};
pub use tabs::{CursorAdvancer, LocalTabLease, ReplicaIdentity, TabId};
pub use versions::version_lag;
