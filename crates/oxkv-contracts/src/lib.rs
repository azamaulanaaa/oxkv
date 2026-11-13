//! Shared 1.0 contract foundation for oxkv.
//!
//! This crate holds the registries that every essay consumes. The first
//! registry is [`version`]. It defines the qualified version axes and the
//! provenance block. [`receipt`] defines the four receipt states and the
//! receipt envelope. [`error`] defines one canonical error code per cause.
//!
//! The crate is portable. It compiles for native and `wasm32-unknown-unknown`.
//! It uses no floating point. It does not read a clock, draw randomness, or
//! perform I/O on a semantic path.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::{ContentDigest, ContentSeal, LeaseTerm};
//!
//! let digest = ContentDigest::of_bytes(b"immutable bytes");
//! let seal = ContentSeal::new(digest, LeaseTerm::new(9), None);
//! assert!(seal.covers(digest));
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod error;
pub mod receipt;
pub mod version;

pub use error::{
    ERROR_REGISTRY, ErrorCode, ErrorKind, ErrorRegistryError, ErrorSpec, RetryClass, lookup,
};
pub use receipt::{
    AuditReference, AuditSequence, IdempotencyKey, ReceiptEnvelope, ReceiptId, ReceiptOutcome,
    ReceiptState, ReceiptTransitionError, RequestId,
};
pub use version::{
    ArtifactKind, CommitVersion, ContentDigest, ContentSeal, CrateVersion, DescriptorDigest,
    DescriptorVersion, EventVersion, FactSourceId, FactVersion, Hash32, KeyEncodingVersion,
    KeyVersion, LeaseTerm, ManifestDigest, ManifestRevision, ManifestSchemaVersion, MapDigest,
    MapSchemaVersion, MapVersion, PageId, PageSequence, PolicyAstVersion, PolicyVersion,
    PrefixBounds, ProjectionEpoch, ProjectionVersion, ProjectionWatermarks, Provenance,
    ProvenanceError, ProvenanceField, ReceiptErrorVersion, ReplicaHighWater, ScopeIdentity,
    ScopeVersion, ScopeVersionVector, SessionMinimum, ShardId, Signature, SnapshotIdentity,
    SnapshotPageVersion, SnapshotVersion, StatisticsVersion, SyncWireVersion, TokenizerVersion,
    TokenizerVersions, VersionAxis, VersionGroup, VersionTextError, VersionVector,
};
