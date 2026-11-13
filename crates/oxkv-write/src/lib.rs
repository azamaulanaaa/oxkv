//! Versioned write requests and an in-memory authority commit simulator.
//!
//! A write request carries one idempotency key and one base version for each
//! mutated key. The authority owns the per-key version sequence and the shard
//! commit sequence. The client never assigns a version.
//!
//! The authority applies five checks in order:
//!
//! 1. receipt lookup for the idempotency key;
//! 2. cross-shard check;
//! 3. base-version check;
//! 4. policy check;
//! 5. commit.
//!
//! Authentication is outside this crate. The crate is portable and pure. It
//! uses no clock, randomness, I/O, floating point, or `unsafe`.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
//! use oxkv_contracts::version::{KeyVersion, ManifestDigest, PolicyVersion, ShardId};
//! use oxkv_write::{AllowAll, Authority, Key, Mutation, WriteRequest};
//!
//! let mut authority = Authority::new(
//!     ShardId::new(vec![1]),
//!     PolicyVersion::from_bytes([4; 32]),
//!     ManifestDigest::from_bytes([5; 32]),
//! );
//! let request = WriteRequest::new(
//!     RequestId::from_bytes([2; 32]),
//!     IdempotencyKey::new(vec![1]),
//!     [Mutation::put(
//!         Key::new(b"alpha".to_vec()),
//!         ShardId::new(vec![1]),
//!         KeyVersion::new(0),
//!         b"one".to_vec(),
//!     )],
//! );
//!
//! let outcome = authority
//!     .submit(&request, &AllowAll, &[])
//!     .expect("the create commits");
//! assert_eq!(outcome.state().name(), "Committed");
//! assert_eq!(authority.key_version(request.mutations()[0].key()).get(), 1);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod audit;
pub mod authority;
pub mod conflict;
pub mod hook;
pub mod policy;
pub mod receipt;
pub mod record;
pub mod request;
mod util;

pub use audit::{AuditChain, AuditRecord, AuditRecordBody, AuditVerdict};
pub use authority::{Authority, SubmitError, SubmitOutcome};
pub use conflict::{ConflictDetail, RebaseError, rebase_request};
pub use hook::{CommitHook, HookEffect, HookError};
pub use policy::{AllowAll, Policy, PolicyRow, PolicyVerdict};
pub use receipt::{ReceiptLedger, ReceiptLedgerError, StoredReceipt};
pub use record::{CommitSummary, CommitView, KeyChange, KeyRecord, KeyValue};
pub use request::{Key, Mutation, WriteOperation, WriteRequest};
