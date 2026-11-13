//! The portable security boundary for oxkv.
//!
//! The crate provides:
//!
//! - [`auth`] verifies a credential and applies the revocation bound.
//! - [`signature`] binds the body, idempotency key, base version, nonce, and
//!   expiry of one request.
//! - [`gate`] runs authentication before authorization and records one audit
//!   record for every write verdict.
//! - [`audit`] holds the per-shard hash chain and signed checkpoints.
//! - [`witness`] models asynchronous checkpoint delivery as a queue.
//!
//! A production deployment supplies a real [`CheckpointSigner`]. The
//! [`StructuralSigner`] is for tests and local runs only.
//! - [`access`] holds the separate read access log.
//! - [`policy`] rejects regular expressions and charges named step budgets.
//! - [`secret`] keeps key material out of policy and audit output.
//!
//! The crate is portable. It has no network, clock, randomness, or file I/O on
//! a semantic path. Time, nonces, the revocation bound, and witness delivery
//! are injected.
//!
//! # Example
//!
//! ```
//! use oxkv_security::auth::{CredentialDirectory, RevocationPolicy};
//! use oxkv_security::policy::{PolicyAst, PolicyLimits, StepBudget, compile_policy};
//!
//! let directory = CredentialDirectory::new(RevocationPolicy::new(5));
//! assert!(directory.revocation_policy().staleness_bound() == 5);
//!
//! let policy = compile_policy(
//!     &PolicyAst::new(),
//!     PolicyLimits::new(8, 4, 1024),
//! )
//! .expect("an empty policy compiles");
//! let mut budget = StepBudget::new(8);
//! assert!(policy.evaluate(&mut budget).expect("the policy is in budget"));
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod access;
pub mod audit;
pub mod auth;
pub mod error;
pub mod gate;
pub mod policy;
pub mod secret;
pub mod signature;
pub mod witness;

pub use access::{
    AccessDecision, AccessEntry, AccessEvent, AccessLog, AccessReference, RetentionClass,
};
pub use audit::{
    AuditChain, AuditCheckpoint, AuditDecision, AuditEntry, AuditExport, AuditRecord,
    AuditVerificationReport, CheckpointSigner, GENESIS_HASH, RuleIdentity, SignerKind,
    StructuralSigner,
};
pub use auth::{
    CredentialDirectory, CredentialId, CredentialProof, CredentialVerifier, LogicalTime,
    PrincipalId, RevocationPolicy, Subject,
};
pub use error::SecurityError;
pub use gate::{
    AllowAllAuthorizer, Authorizer, DenyAllAuthorizer, EnforcementInput, ReadAccess, ReadOutcome,
    ReadRequest, SecurityGate, WriteAccess, WriteOutcome,
};
pub use policy::{
    CompiledPolicy, CompiledSearch, PolicyAst, PolicyLimits, PolicyNode, SearchNode, SearchQuery,
    StepBudget, compile_policy, compile_search,
};
pub use secret::{KeyIdentifier, Secret, SecretReference};
pub use signature::{
    Nonce, RequestFields, RequestKey, RequestSignature, SignatureVerifier, SignedRequest,
};
pub use witness::{WitnessQueue, WitnessSink};
