//! The per-shard, hash-linked, signed audit record.
//!
//! One terminal verdict writes one audit record. The record carries the
//! request identity, the idempotency key, the authenticated principal, the
//! shard, the base version of each row, the pinned policy version, the
//! manifest digest, the map version, the projection epoch, the fact versions,
//! the rule identity, the bounded decision list, the verdict, the reason, the
//! receipt identity, and the input digest. The chain links each record to the
//! previous record in the same shard.
//!
//! The crate uses the signed chain from `oxkv-security`. The chain signs one
//! checkpoint for each terminal verdict. The authority sends each checkpoint
//! to the asynchronous witness after the commit.

pub use oxkv_security::audit::{
    AuditChain, AuditCheckpoint, AuditDecision, AuditEntry, AuditRecord, AuditRowDecision,
    CheckpointSigner, GENESIS_HASH, StructuralSigner,
};

/// The outcome that one audit record covers.
pub type AuditVerdict = AuditDecision;

/// The body of one audit record.
pub type AuditRecordBody = AuditEntry;
