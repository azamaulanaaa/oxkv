#![allow(missing_docs)]

//! Shared helpers for the outbox golden vectors.

use oxkv_contracts::version::{Hash32, KeyVersion};
use oxkv_contracts::{
    AuditReference, AuditSequence, IdempotencyKey, ManifestDigest, PolicyVersion, ReceiptEnvelope,
    ReceiptId, ReceiptOutcome, RequestId, ShardId,
};

use oxkv_outbox::{OutboxEntry, RequestDigest};

pub(crate) fn key(value: u8) -> IdempotencyKey {
    IdempotencyKey::new(vec![value])
}

pub(crate) fn digest(value: u8) -> RequestDigest {
    RequestDigest::from_bytes([value; 32])
}

pub(crate) fn shard(value: u8) -> ShardId {
    ShardId::new(vec![value])
}

pub(crate) fn entry(
    key_value: u8,
    digest_value: u8,
    shard_value: u8,
    order_key: &[u8],
) -> OutboxEntry {
    OutboxEntry::new(key(key_value), digest(digest_value), shard(shard_value))
        .with_request_id(RequestId::from_bytes([key_value; 32]))
        .with_base_version(order_key.to_vec(), KeyVersion::new(1))
}

pub(crate) fn envelope(
    idempotency_key: IdempotencyKey,
    receipt_value: u8,
    outcome: Option<ReceiptOutcome>,
) -> ReceiptEnvelope {
    let mut receipt = ReceiptEnvelope::queued(
        ReceiptId::from_bytes([receipt_value; 32]),
        RequestId::from_bytes([receipt_value; 32]),
        idempotency_key,
        AuditReference::new(shard(1), AuditSequence::new(1), Hash32::from_bytes([3; 32])),
        PolicyVersion::from_bytes([4; 32]),
        ManifestDigest::from_bytes([5; 32]),
        "projection.lagging",
    );
    if let Some(outcome) = outcome {
        receipt
            .close(outcome)
            .expect("a queued receipt closes once");
    }
    receipt
}
