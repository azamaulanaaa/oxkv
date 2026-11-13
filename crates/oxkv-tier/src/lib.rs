//! Tiered storage placement, authority leases, and content seals for oxkv.
//!
//! The crate holds four seams:
//!
//! - [`tier`] defines the RAM, disk, and object ladder. A [`TierDriver`] supplies
//!   storage only. It does not choose a tier or change a byte.
//! - [`lease`] defines the single-writer authority lease and its monotonic
//!   [`LeaseTerm`] fence. A replica cannot hold an authority write lease.
//! - [`seal`] defines the content seal. The seal covers the content digest and
//!   the lease term. The lease term never enters the content digest.
//! - [`placement`], [`movement`], and [`warmup`] define the deterministic
//!   placement policy, tier movement, eviction, and bounded warm-up.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It reads no clock, draws no randomness, performs no I/O, uses no floating
//! point, and holds no `unsafe` code. The caller injects lease time, lease
//! duration, and tier state.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::{ContentDigest, PageSequence, ShardId};
//! use oxkv_tier::{
//!     ArtifactId, LeaseAuthority, LeaseDuration, LeaseTime, SealedContent, ShardRole,
//!     Tier, TierStack, WarmupBudget, WarmupCursor, WarmupItem, WriterId,
//! };
//!
//! let mut authority = LeaseAuthority::new(ShardId::new(vec![1]));
//! let grant = authority
//!     .acquire(
//!         ShardRole::Authority,
//!         WriterId::new("writer-a"),
//!         LeaseTime::new(0),
//!         LeaseDuration::new(10),
//!     )
//!     .expect("the first lease succeeds");
//!
//! let page = SealedContent::seal(b"page-bytes".to_vec(), grant.term());
//! page.verify_read().expect("the sealed page is valid");
//!
//! let artifact = ArtifactId::new(ShardId::new(vec![1]), 0, PageSequence::new(1));
//! let digest = ContentDigest::of_bytes(b"page-bytes");
//! let mut stack = TierStack::memory();
//! stack
//!     .put(Tier::Object, artifact.clone(), b"page-bytes".to_vec())
//!     .expect("the object tier accepts the page");
//! let outcome = stack.warmup(
//!     &[WarmupItem::new(artifact, digest)],
//!     WarmupCursor::start(),
//!     WarmupBudget::new(1, 16),
//! );
//! assert!(outcome.is_complete());
//! ```
//!
//! [`LeaseTerm`]: oxkv_contracts::version::LeaseTerm

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod error;
pub mod lease;
pub mod movement;
pub mod placement;
pub mod replica;
pub mod seal;
pub mod tier;
pub mod warmup;

pub use error::TierError;
pub use lease::{
    LeaseAuthority, LeaseDuration, LeaseFence, LeaseGrant, LeaseRecord, LeaseTime, ShardRole,
    WriterId,
};
pub use movement::Movement;
pub use placement::{
    DurabilityClass, Placement, PlacementFacts, PlacementPlan, PlacementPolicy, PlacementRule,
};
pub use replica::{LocalTabLease, ReplicaId};
pub use seal::SealedContent;
pub use tier::{
    ArtifactId, MemoryTierDriver, Tier, TierAvailability, TierDriver, TierStack, TierStatus,
};
pub use warmup::{WarmupBudget, WarmupCursor, WarmupItem, WarmupOutcome};
