//! Durable client outbox and receipt ledger for oxkv.
//!
//! The crate holds two pure state machines:
//!
//! - [`Outbox`] records an intent before the first send.
//! - [`ReceiptLedger`] records every authority verdict under its idempotency
//!   key.
//!
//! The crate also holds [`OutboxEntry`] and [`ReceiptCursor`].
//!
//! # Receipt states
//!
//! An authority receipt has four states. [`ReceiptState::Queued`] is live. The
//! other three states are terminal. A 1.x release does not add a fifth state.
//!
//! # Local dispositions
//!
//! An outbox entry has a local disposition. A local disposition is not a
//! receipt. A local cancel becomes [`LocalDisposition::CancelRequested`] and
//! then [`LocalDisposition::Abandoned`]. It never appears in a receipt vector.
//! A late authority verdict wins for the data.
//!
//! # Ordering
//!
//! The outbox orders entries per key with FIFO. At most one entry per key is in
//! flight. The scheduler states are `queued`, `in_flight`, and `terminal`.
//!
//! # Purity
//!
//! The crate has no clock, randomness, I/O, floating point, or `unsafe` code.
//! Retention and deadlines are injected values.
//!
//! # Example
//!
//! ```
//! use oxkv_outbox::{IdempotencyKey, Outbox, OutboxEntry, RequestDigest, ShardId};
//!
//! let mut outbox = Outbox::new(4);
//! let entry = OutboxEntry::new(
//!     IdempotencyKey::new(vec![1]),
//!     RequestDigest::from_bytes([2; 32]),
//!     ShardId::new(vec![3]),
//! );
//! outbox.admit(entry).expect("the outbox has room");
//! assert_eq!(outbox.len(), 1);
//! ```
//!
//! The ledger starts empty and uses an injected retention floor.
//!
//! ```
//! use oxkv_outbox::{ReceiptLedger, ReceiptSequence, ReceiptState};
//!
//! let ledger = ReceiptLedger::new(ReceiptSequence::new(0));
//! assert_eq!(ledger.receipt_states(), Vec::<ReceiptState>::new());
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

mod admit;
mod cursor;
mod entry;
mod outbox;
mod receipt;

pub use admit::AdmissionError;
pub use cursor::ReceiptCursor;
pub use entry::{BaseVersion, LocalDisposition, OutboxEntry, RequestDigest, SchedulerState};
pub use outbox::{Outbox, OutboxError, Reconciliation};
pub use receipt::{LedgerError, ReceiptLedger, ReceiptRecord};

pub use oxkv_contracts::{
    AuditReference, AuditSequence, CommitVersion, IdempotencyKey, KeyVersion, ManifestDigest,
    PolicyVersion, ReceiptEnvelope, ReceiptId, ReceiptOutcome, ReceiptSequence, ReceiptState,
    RequestId, ShardId,
};
