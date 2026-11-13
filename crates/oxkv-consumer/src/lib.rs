//! Reference effect consumer for `oxkv`.
//!
//! The crate performs one effect for one committed business event. It keeps a
//! per-key cursor, a completion record, and a dead-letter record. It is a
//! library, not a runtime. The deployment supplies the provider client, the
//! clock, and the limits.
//!
//! # Shape
//!
//! - Work partitions by key. One unacknowledged event blocks only its key.
//! - Acknowledgement is per key. A per-shard cursor is not the contract.
//! - Delivery is at least once. A completion record and a provider
//!   idempotency key make the effect idempotent.
//! - A handler failure schedules a retry with a deterministic backoff.
//! - A terminal failure writes one dead-letter record and advances the key
//!   cursor in the same step.
//! - The consumer builds every required effect field from the event payload.
//!   It never reads the authority.
//!
//! # Purity
//!
//! The crate uses no network, clock, randomness, file I/O, floating point, or
//! `unsafe` code. The caller injects the current time, the handler, and the
//! retry limits. The crate compiles for native and `wasm32-unknown-unknown`.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::cursor::EventSequence;
//! use oxkv_contracts::keyspace::TableName;
//! use oxkv_contracts::version::{CommitVersion, ShardId};
//! use oxkv_consumer::event::{EffectPayload, EventEnvelope, EventId};
//!
//! let payload = EffectPayload::new(
//!     "ada@example.test".to_owned(),
//!     "Invoice 1042 approved".to_owned(),
//!     "The invoice is approved.".to_owned(),
//! );
//! let event_id = EventId::derive(
//!     &ShardId::new(b"invoice/".to_vec()),
//!     b"invoice/1042",
//!     CommitVersion::new(91),
//!     "invoice.approved",
//!     0,
//! );
//! let envelope = EventEnvelope::new(
//!     event_id,
//!     ShardId::new(b"invoice/".to_vec()),
//!     TableName::parse_user("invoice").unwrap(),
//!     b"invoice/1042".to_vec(),
//!     EventSequence::new(1),
//!     payload,
//! );
//! assert_eq!(envelope.key, b"invoice/1042");
//! ```
//!
//! # Related essays
//!
//! Essay 24 supplies the wire envelope. [`event::EventEnvelope`] is the
//! consumer-side view of that envelope. Essay 28 reads
//! [`dlq::DeadLetterRecord`] for the operator redrive path.
//!
//! The crate directory holds `CONTRACT.md`. That file describes the state
//! machine and the error codes.

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod backoff;
pub mod consumer;
pub mod dlq;
pub mod event;
pub mod handler;

pub use backoff::RetryPolicy;
pub use consumer::{
    Attempt, Begin, CompletionRecord, ConsumerCheckpoint, ConsumerConfig, ConsumerError, Delivery,
    DuplicateCause, EffectConsumer, Lag, RedriveOutcome,
};
pub use dlq::{DEAD_LETTER_RECORD_VERSION, DeadLetterReason, DeadLetterRecord, RedriveAudit};
pub use event::{
    EFFECT_ENVELOPE_VERSION, EFFECT_PAYLOAD_SCHEMA, Effect, EffectPayload, EffectPayloadError,
    EventEnvelope, EventId, ObjectReference,
};
pub use handler::{Deadline, EffectHandler, EffectRequest, HandlerOutcome, ProviderIdempotencyKey};

pub use oxkv_contracts::cursor::EventSequence;
