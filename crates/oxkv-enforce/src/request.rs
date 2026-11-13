//! Write requests, row mutations, and the request input digest.
//!
//! The request never names the policy version, the principal, or the shard
//! map. The authority supplies those values at commit.

use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
use oxkv_contracts::version::{Hash32, KeyVersion, ShardId};

use crate::codec::{push_bytes, push_option_u64, push_tag, push_u64};

/// The maximum number of rows in one write batch.
///
/// The limit bounds the audit decision list.
pub const MAX_BATCH_ROWS: usize = 1024;

/// The domain tag for the request input digest.
const INPUT_TAG: &[u8] = b"oxkv/enforce-input/v1";

/// A row key inside one shard.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowKey(Vec<u8>);

impl RowKey {
    /// Creates a row key from raw bytes.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Returns the raw key bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for RowKey {
    fn from(bytes: Vec<u8>) -> Self {
        Self::new(bytes)
    }
}

/// One proposed row change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowMutation {
    /// The row key inside the target shard.
    pub key: RowKey,
    /// The shard that owns the row.
    pub shard_id: ShardId,
    /// The proposed value bytes.
    pub value: Vec<u8>,
}

impl RowMutation {
    /// Creates one proposed row change.
    #[must_use]
    pub fn new(key: RowKey, shard_id: ShardId, value: Vec<u8>) -> Self {
        Self {
            key,
            shard_id,
            value,
        }
    }
}

/// One client write request.
///
/// The request carries an idempotency key and one base version. The base
/// version applies to every mutated key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    request_id: RequestId,
    idempotency_key: IdempotencyKey,
    base_version: Option<KeyVersion>,
    rows: Vec<RowMutation>,
}

impl WriteRequest {
    /// Creates a request with a base version.
    #[must_use]
    pub fn new(
        request_id: RequestId,
        idempotency_key: IdempotencyKey,
        base_version: KeyVersion,
        rows: Vec<RowMutation>,
    ) -> Self {
        Self {
            request_id,
            idempotency_key,
            base_version: Some(base_version),
            rows,
        }
    }

    /// Creates a request without a base version.
    ///
    /// The authority rejects this request with `write.missing_base_version`.
    #[must_use]
    pub fn without_base_version(
        request_id: RequestId,
        idempotency_key: IdempotencyKey,
        rows: Vec<RowMutation>,
    ) -> Self {
        Self {
            request_id,
            idempotency_key,
            base_version: None,
            rows,
        }
    }

    /// Returns the request identity.
    #[must_use]
    pub const fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Returns the idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    /// Returns the base version.
    #[must_use]
    pub const fn base_version(&self) -> Option<KeyVersion> {
        self.base_version
    }

    /// Returns the proposed row changes.
    #[must_use]
    pub fn rows(&self) -> &[RowMutation] {
        &self.rows
    }

    /// Returns the canonical input digest.
    ///
    /// The digest covers the request identity, the idempotency key, the base
    /// version, and every row. The digest does not cover the principal, the
    /// policy version, or the shard map.
    #[must_use]
    pub fn input_digest(&self) -> Hash32 {
        let mut buffer = Vec::new();
        push_tag(&mut buffer, INPUT_TAG);
        push_bytes(&mut buffer, self.request_id.as_bytes());
        push_bytes(&mut buffer, self.idempotency_key.as_bytes());
        push_option_u64(&mut buffer, self.base_version.map(KeyVersion::get));
        push_u64(
            &mut buffer,
            u64::try_from(self.rows.len()).unwrap_or(u64::MAX),
        );
        for row in &self.rows {
            push_bytes(&mut buffer, row.shard_id.as_bytes());
            push_bytes(&mut buffer, row.key.as_bytes());
            push_bytes(&mut buffer, &row.value);
        }
        Hash32::of(&buffer)
    }
}
