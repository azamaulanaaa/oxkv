//! Policy fact projection for oxkv.
//!
//! A shard evaluates a rule against a local projection of foreign facts. The
//! projection carries an epoch and one watermark per fact source. The
//! authorization bound is a version distance. Time is monitoring data only.
//!
//! The crate is portable. It performs no storage, clock, randomness, or I/O.
//! It uses no floating point and no `unsafe`.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::version::{FactVersion, PolicyVersion, ProjectionEpoch, ShardId};
//! use oxkv_projection::{
//!     CanonicalType, FactDeclaration, FactId, FactOwner, FactSet, FactVersionDistance,
//!     Projection, ProjectionEpochState,
//! };
//!
//! let declaration = FactDeclaration::new(
//!     FactId::new("account_owner"),
//!     CanonicalType::Text,
//!     FactOwner::Shard(ShardId::new(vec![2])),
//!     4,
//!     FactVersionDistance::new(2),
//! );
//! let facts = FactSet::from_declarations([declaration]).expect("one declaration");
//! let epoch = ProjectionEpochState::new(
//!     ProjectionEpoch::new(1),
//!     PolicyVersion::from_bytes([1; 32]),
//! );
//! let projection = Projection::new(facts, epoch);
//! assert!(projection.is_valid());
//! assert_eq!(projection.epoch(), ProjectionEpoch::new(1));
//! # let _ = FactVersion::new(0);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

/// The reserved `0x04` system identifier for a stored projection.
///
/// This crate owns the logical projection state only. A storage layer writes
/// the state under `0xFF || 0x04`.
pub const PROJECTION_SYSTEM_ID: u8 = oxkv_contracts::SYSTEM_ID_PROJECTION;

pub mod codec;
pub mod epoch;
pub mod error;
pub mod fact;
pub mod feed;
pub mod invalidate;
pub mod projection;
pub mod staleness;
pub mod verdict;

pub use codec::{CanonicalEncode, hex};
pub use epoch::{PolicyCohort, ProjectionEpochState};
pub use error::{
    AUTHORIZATION_STALE_FACTS, PROJECTION_CHECKSUM_FAILED, PROJECTION_CLIENT_WRITE_REJECTED,
    PROJECTION_EPOCH_INVALID, PROJECTION_FAN_IN_EXCEEDED, PROJECTION_LAGGING,
    PROJECTION_READ_SET_EXCEEDED, PROJECTION_WATERMARK_REGRESSION, ProjectionError,
    WRITE_QUEUE_TIMEOUT,
};
pub use fact::{
    CanonicalType, FactDeclaration, FactId, FactOwner, FactSet, FactValue, FactVersionDistance,
    StoredFact,
};
pub use feed::{
    FactChange, FeedResume, ProjectionDiff, ProjectionFeed, ProjectionFeedCursor,
    ProjectionSnapshot, validate_cursor,
};
pub use invalidate::{
    InvalidationCause, InvalidationDecision, InvalidationRecord, PolicyChange,
    classify_fact_set_change, evaluate_invalidation,
};
pub use projection::{ApplyOutcome, Projection, ReadSet, ReadSetLimits};
pub use staleness::{
    AuthorizationDecision, AuthorizationMode, AuthorizationReport, FactSourceVersions,
    LagObservation, QueueDeadline, StalenessReport, authorize,
};
pub use verdict::{Decision, FactRead, RuleId, Verdict};
