//! Resumable and verified snapshot hydration for an oxkv replica.
//!
//! The crate implements one hydration state machine. The machine streams
//! snapshot pages into a new generation beside the served generation. It
//! verifies every page before the page becomes visible. It publishes the new
//! generation with one pointer swap. A durable journal records every boundary,
//! so a restart resumes at the last durable boundary.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses no network, clock, randomness, I/O, floating point, or `unsafe`.
//! The transport and the durable store are trait seams. The crate includes
//! in-memory adapters for tests.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::LeaseTerm;
//! use oxkv_hydrate::{
//!     HydrationRequest, Hydrator, MemoryStore, MemoryTransport, PrefixBounds,
//!     Replica, SnapshotDraft, SnapshotPage, SnapshotPageVersion,
//! };
//!
//! let bounds = PrefixBounds::new(vec![b'a'], vec![b'z']);
//! let page = SnapshotPage::with_records(
//!     0,
//!     vec![(b"alpha".to_vec(), b"one".to_vec())],
//!     bounds.clone(),
//! );
//! let snapshot = SnapshotDraft::sample(bounds, LeaseTerm::new(1))
//!     .seal(vec![page])
//!     .expect("the sample snapshot is valid");
//! let request = HydrationRequest::from_snapshot(&snapshot, 0);
//!
//! let mut store = MemoryStore::default();
//! let mut transport = MemoryTransport::from_snapshot(&snapshot);
//! let mut hydrator = Hydrator::new(&mut store, &mut transport);
//! hydrator.start(&request).expect("hydration starts");
//! hydrator.run().expect("hydration completes");
//!
//! let replica = Replica::new(&store);
//! assert_eq!(replica.read(b"alpha").expect("the replica is warm"), Some(b"one".to_vec()));
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod budget;
pub mod error;
pub mod hydrate;
pub mod journal;
pub mod page;
pub mod replica;
pub mod snapshot;
pub mod store;
pub mod tail;
pub mod transport;

mod encoding;

pub use budget::{BudgetPlan, ScopeBudget};
pub use error::{HydrationError, HydrationProgress};
pub use hydrate::{
    FrameOutcome, HydrationReport, HydrationRequest, HydrationTrace, Hydrator, StartOutcome,
    StepOutcome,
};
pub use journal::{GenerationId, Journal, Phase};
pub use oxkv_contracts::version::{
    CommitVersion, ContentDigest, LeaseTerm, ManifestDigest, PageId, PageSequence, PolicyVersion,
    PrefixBounds, ScopeIdentity, ScopeVersionVector, ShardId, SnapshotIdentity,
    SnapshotPageVersion, VersionVector,
};
pub use page::{
    KNOWN_REQUIRED_FEATURES, PAGE_FORMAT_MAJOR, PAGE_FORMAT_MINOR, SnapshotPage, crc32c,
};
pub use replica::Replica;
pub use snapshot::{SealedSnapshot, SnapshotDescriptor, SnapshotDirectoryEntry, SnapshotDraft};
pub use store::{DurableStore, Generation, GenerationReader, MemoryStore, StoredPage};
pub use tail::DiffFrame;
pub use transport::{MemoryTransport, Transport};
