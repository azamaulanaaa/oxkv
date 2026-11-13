//! Versioned write requests and a durable authority for oxkv.
//!
//! A write request carries one client identity, one idempotency key, and one
//! base version for each mutated key. The authority owns the per-key version
//! sequence and the shard commit sequence. The client never assigns a version.
//!
//! The authority applies five checks in order:
//!
//! 1. receipt lookup for the idempotency key;
//! 2. cross-shard check;
//! 3. base-version check;
//! 4. policy check;
//! 5. commit.
//!
//! Authentication is outside this crate. The commit path is portable and
//! pure. It uses no clock, randomness, I/O, floating point, or `unsafe`. The
//! native state file and the native witness outbox are the only file I/O
//! paths.
//!
//! An authority can save its state to a directory and load it again after a
//! restart. The state covers the key records, the receipt ledger, the commit
//! version, the audit records, the signed checkpoints, and the chain tip. The
//! save path writes a temporary file and renames it over the state file.
//!
//! The authority appends one signed audit record for each terminal verdict.
//! The audit chain comes from `oxkv-security`. The authority sends each signed
//! checkpoint to an asynchronous witness outbox after the commit. The commit
//! path does not wait for the witness.
//!
//! Production mode requires a real [`CheckpointSigner`]. This tree has no
//! production signer. A production configuration that uses
//! [`StructuralSigner`] fails at startup. The structural signer is for tests
//! and local runs.
//!
//! A native deployment stores checkpoints in a `FileWitnessOutbox`. The
//! outbox survives a restart and resumes delivery. Tests and local runs use
//! the in-memory `WitnessQueue`.
//!
//! # Example
//!
//! ```
//! use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
//! use oxkv_contracts::version::{
//!     KeyVersion, ManifestDigest, MapVersion, PolicyVersion, ProjectionEpoch,
//!     ProjectionWatermarks, ShardId,
//! };
//! use oxkv_security::secret::KeyIdentifier;
//! use oxkv_write::{AllowAll, Authority, AuthorityConfig, ClientId, Key, Mutation, WriteRequest};
//!
//! let mut authority = Authority::local(AuthorityConfig::new(
//!     ShardId::new(vec![1]),
//!     PolicyVersion::from_bytes([4; 32]),
//!     ManifestDigest::from_bytes([5; 32]),
//!     MapVersion::new(6),
//!     ProjectionEpoch::new(7),
//!     ProjectionWatermarks::new(),
//!     KeyIdentifier::new("checkpoint-key-1"),
//! ))
//! .expect("the local authority starts");
//! let request = WriteRequest::new(
//!     RequestId::from_bytes([2; 32]),
//!     ClientId::new(vec![7]),
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
#[cfg(not(target_arch = "wasm32"))]
mod state;
mod util;
#[cfg(not(target_arch = "wasm32"))]
pub mod witness_outbox;

pub use audit::{
    AuditChain, AuditCheckpoint, AuditRecord, AuditRecordBody, AuditVerdict, CheckpointSigner,
    SignerKind, StructuralSigner,
};
pub use authority::{
    Authority, AuthorityConfig, AuthorityInitError, AuthorityMode, SubmitError, SubmitOutcome,
};
pub use conflict::{ConflictDetail, RebaseError, rebase_request};
pub use hook::{CommitHook, HookEffect, HookError};
pub use policy::{
    AllowAll, DecisionVerdict, Policy, PolicyDecision, PolicyEvaluation, PolicyRow, PolicyVerdict,
    RuleIdentity,
};
pub use receipt::{ReceiptLedger, ReceiptLedgerError, StoredReceipt};
pub use record::{CommitSummary, CommitView, KeyChange, KeyRecord, KeyValue};
pub use request::{ClientId, Key, Mutation, WriteOperation, WriteRequest};
#[cfg(not(target_arch = "wasm32"))]
pub use state::{AuthorityStateError, STATE_FILE};
#[cfg(not(target_arch = "wasm32"))]
pub use witness_outbox::FileWitnessOutbox;
