#![allow(missing_docs)]

//! Shared support for the security golden vectors.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
use oxkv_contracts::version::{
    FactSourceId, FactVersion, Hash32, KeyVersion, ManifestDigest, MapVersion, PolicyVersion,
    ProjectionEpoch, ShardId,
};
use oxkv_security::audit::{AuditDecision, AuditEntry, RuleIdentity};
use oxkv_security::auth::PrincipalId;
use oxkv_security::signature::{Nonce, RequestFields, RequestKey, SignedRequest};

pub(crate) fn sample_key() -> RequestKey {
    RequestKey::new([7; 32])
}

pub(crate) fn sample_fields() -> RequestFields {
    RequestFields {
        request_id: RequestId::from_bytes([1; 32]),
        body: b"account:42:update".to_vec(),
        idempotency_key: IdempotencyKey::new(b"idem-1".to_vec()),
        base_version: KeyVersion::new(7),
        shard_id: ShardId::new(vec![1]),
        nonce: Nonce::from_bytes([9; 32]),
        expires_at: oxkv_security::auth::LogicalTime::new(100),
    }
}

pub(crate) fn sample_request() -> SignedRequest {
    sample_key().sign(sample_fields())
}

pub(crate) fn sample_entry() -> AuditEntry {
    let mut fact_versions = BTreeMap::new();
    fact_versions.insert(FactSourceId::new("accounts"), FactVersion::new(3));
    AuditEntry {
        request_id: RequestId::from_bytes([1; 32]),
        idempotency_key: IdempotencyKey::new(b"idem-1".to_vec()),
        principal: PrincipalId::from_bytes(vec![2]),
        shard_id: ShardId::new(vec![1]),
        base_version: KeyVersion::new(7),
        base_versions: vec![(b"account/42".to_vec(), Some(KeyVersion::new(7)))],
        policy_version: PolicyVersion::from_bytes([3; 32]),
        manifest_digest: ManifestDigest::from_bytes([4; 32]),
        map_version: MapVersion::new(5),
        projection_epoch: ProjectionEpoch::new(6),
        fact_versions,
        rule_identity: RuleIdentity::new("accounts/write"),
        decisions: Vec::new(),
        decision: AuditDecision::Committed,
        reason_code: None,
        receipt_id: Some(oxkv_contracts::receipt::ReceiptId::from_bytes([10; 32])),
        input_digest: Hash32::from_bytes([8; 32]),
    }
}
