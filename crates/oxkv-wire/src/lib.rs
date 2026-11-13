//! The sans-IO sync wire protocol v1 for oxkv.
//!
//! The crate defines frames, events, commands, diff semantics, cursors, resume
//! decisions, and credit-based flow control. A transport driver moves bytes.
//! The core does not open a socket, spawn a task, read a clock, draw
//! randomness, or perform I/O.
//!
//! The frame format is canonical:
//!
//! ```text
//! frame_version (u8) | message_type (u8) | payload_length (u32 BE)
//! | payload | BLAKE3-256 integrity
//! ```
//!
//! The same input transcript produces the same byte sequence on native and
//! `wasm32-unknown-unknown`.
//!
//! # Protocol notes
//!
//! `SessionHello` carries `manifest_revision` so the authority can compare it
//! with a minimum. The specification names this check but not the field.
//! A second hello closes the session with `sync.negotiation_required` because
//! the frozen registry has no renegotiation code.
//! Snapshot pages use CRC32C. Frames use BLAKE3.
//!
//! # Example
//!
//! ```
//! use oxkv_wire::frame::{Frame, MessageType};
//! use oxkv_wire::types::SubscriptionId;
//! use oxkv_wire::diff::{DiffBatch, DiffEntry, DiffOperation};
//! use oxkv_wire::types::MessageSequence;
//! use oxkv_contracts::cursor::ScopeGeneration;
//! use oxkv_contracts::version::{CommitVersion, ShardId};
//!
//! let entry = DiffEntry {
//!     subscription_id: SubscriptionId::new(7),
//!     scope_generation: ScopeGeneration::new(1),
//!     sequence: MessageSequence::new(0),
//!     shard_id: ShardId::new(vec![1]),
//!     key: b"account:1".to_vec(),
//!     operation: DiffOperation::Put(b"value".to_vec()),
//!     commit_version: CommitVersion::new(9),
//! };
//! let frame = Frame::DiffBatch(DiffBatch::new(
//!     SubscriptionId::new(7),
//!     vec![entry],
//! ));
//! let bytes = frame.encode();
//! let decoded = Frame::decode(&bytes, 1_048_576).expect("the frame round trips");
//! assert_eq!(decoded.message_type(), MessageType::DiffBatch);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

mod codec;
mod crc32c;

pub mod cursor;
pub mod diff;
pub mod error;
pub mod frame;
pub mod session;
pub mod types;

pub use cursor::{CursorContext, CursorDecision, CursorPolicy, CursorSigner, cursor_error_code};
pub use diff::{
    ApplyOutcome, DiffApplier, DiffApplyError, DiffBatch, DiffEntry, DiffOperation, key_in_scope,
};
pub use error::ErrorCode;
pub use frame::{
    Ack, CloseFrame, Credit, FRAME_HEADER_LENGTH, FRAME_INTEGRITY_LENGTH, FRAME_VERSION, Frame,
    FrameError, Keepalive, MessageType, ProtocolErrorFrame, ReceiptFrame, ReceiptResume, Resume,
    Resumed, SessionHello, SessionReady, SnapshotComplete, SnapshotOffer, SnapshotPage,
    SnapshotTail, Subscribe, Subscribed,
};
pub use session::{
    Command, DiffsReady, Event, Negotiated, Session, SessionConfig, SessionPhase, SnapshotReady,
};
pub use types::{
    CAPABILITY_DIFF_V1, CAPABILITY_FLOW_CONTROL_V1, CAPABILITY_KEEPALIVE_V1,
    CAPABILITY_POLICY_OPAQUE_V1, CAPABILITY_RECEIPT_V1, CAPABILITY_RESUME_V1,
    CAPABILITY_SNAPSHOT_V1, Capabilities, Limits, MessageSequence, SubscriptionId,
};
