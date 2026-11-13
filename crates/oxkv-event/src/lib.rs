//! Business events for the oxkv store.
//!
//! The crate holds the `oxkv/event/v1` envelope, the deterministic event id,
//! the per-key consumer cursor, the at-least-once effect consumer, retention,
//! the dead-letter record, and the separate operational feed.
//!
//! [`envelope`] defines the canonical envelope. [`id`] computes the
//! deterministic identity. [`commit`] builds authority-only business events
//! from one committed write. [`cursor`] and [`consumer`] serve the effect
//! consumer. [`feed`] carries operational signals. [`log`] provides the
//! reference outbox record.
//!
//! The crate is portable. It compiles for native and
//! `wasm32-unknown-unknown`. It performs no network, storage, clock,
//! randomness, or floating-point work. It contains no `unsafe` code.
//!
//! A business event commits with its write. A rejected write and a
//! conflicting write emit no business event. The consumer partitions work by
//! key. One unacknowledged event blocks its key only.
//!
//! # Example
//!
//! ```
//! use oxkv_event::{CommitVersion, EmissionIndex, RuleId, ShardId, compute_event_id};
//!
//! let id = compute_event_id(
//!     &ShardId::new(vec![1]),
//!     b"accounts/42",
//!     CommitVersion::new(18),
//!     &RuleId::new("welcome_mail"),
//!     EmissionIndex::new(0),
//! );
//! assert_eq!(id.as_bytes().len(), 32);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod codec;
pub mod commit;
pub mod consumer;
pub mod cursor;
pub mod envelope;
pub mod error;
pub mod feed;
pub mod id;
pub mod log;
pub mod retention;

pub use commit::{BusinessEvent, CommittedWrite, EventDraft, WriteOutcome, events_for_write};
pub use consumer::{
    CompletionRecord, Consumer, DeadLetterRecord, Deadline, Delivery, EffectHandler, EventSource,
    HandlerOutcome, ProviderIdempotencyKey, RetryPolicy,
};
pub use cursor::ConsumerCursor;
pub use envelope::{
    DEFAULT_PAYLOAD_BOUND, EVENT_VERSION, EVENT_VERSION_TEXT, EventEnvelope, EventEnvelopeParts,
    EventPayload, ObjectReference, ObjectVersion, PayloadBound, PayloadValue,
};
pub use error::EventError;
pub use feed::{
    OPERATIONAL_FEED_VERSION_TEXT, OperationalEnvelope, OperationalFeed, OperationalFeedVersion,
    OperationalPayload, OperationalSignalKind, SignalId, SignalSchema,
};
pub use id::{
    CommittedAt, EffectField, EmissionIndex, EventId, OriginRequestId, PayloadSchema, RuleId,
    compute_event_id,
};
pub use log::EventLog;
pub use oxkv_contracts::cursor::{EventPosition, EventSequence};
pub use oxkv_contracts::keyspace::TableName;
pub use oxkv_contracts::version::{
    CommitVersion, ContentDigest, KeyVersion, PolicyVersion, ShardId,
};
pub use retention::{RetentionClass, RetentionFloor, position_minimum};
