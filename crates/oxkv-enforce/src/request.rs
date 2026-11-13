//! Write requests, row mutations, and the request input digest.
//!
//! The request never names the policy version, the principal, or the shard
//! map. The authority supplies those values at commit.

use oxkv_contracts::keyspace::{KEY_ENCODING_VERSION, SYSTEM_ID_PROJECTION, SYSTEM_MARKER};
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

    /// Returns true when the key is in the projection system namespace.
    ///
    /// A projection key has the form `0x01 || table || 0x00 || 0xFF || 0x04`.
    #[must_use]
    pub fn is_projection(&self) -> bool {
        let Some(separator) = self.0.iter().skip(1).position(|byte| *byte == 0x00) else {
            return false;
        };
        if separator == 0 {
            return false;
        }
        let marker = separator + 2;
        self.0.first() == Some(&KEY_ENCODING_VERSION)
            && self.0.get(marker) == Some(&SYSTEM_MARKER)
            && self.0.get(marker + 1) == Some(&SYSTEM_ID_PROJECTION)
    }
}

impl From<Vec<u8>> for RowKey {
    fn from(bytes: Vec<u8>) -> Self {
        Self::new(bytes)
    }
}

/// One proposed row change.
///
/// Each row carries its own base version. A batch can mutate keys at
/// different versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowMutation {
    /// The row key inside the target shard.
    pub key: RowKey,
    /// The shard that owns the row.
    pub shard_id: ShardId,
    /// The base version of the row.
    ///
    /// A value of `None` means the request is malformed. The authority
    /// rejects the request with `write.missing_base_version`.
    pub base_version: Option<KeyVersion>,
    /// The proposed value bytes.
    pub value: Vec<u8>,
}

impl RowMutation {
    /// Creates one proposed row change with a base version.
    #[must_use]
    pub fn new(key: RowKey, shard_id: ShardId, base_version: KeyVersion, value: Vec<u8>) -> Self {
        Self {
            key,
            shard_id,
            base_version: Some(base_version),
            value,
        }
    }

    /// Creates one proposed row change without a base version.
    ///
    /// Use this constructor only to model a malformed request. The authority
    /// rejects the request with `write.missing_base_version`.
    #[must_use]
    pub fn without_base(key: RowKey, shard_id: ShardId, value: Vec<u8>) -> Self {
        Self {
            key,
            shard_id,
            base_version: None,
            value,
        }
    }
}

/// One client write request.
///
/// The request carries an idempotency key and one base version for each row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    request_id: RequestId,
    idempotency_key: IdempotencyKey,
    rows: Vec<RowMutation>,
}

impl WriteRequest {
    /// Creates a request from its parts.
    #[must_use]
    pub fn new(
        request_id: RequestId,
        idempotency_key: IdempotencyKey,
        rows: Vec<RowMutation>,
    ) -> Self {
        Self {
            request_id,
            idempotency_key,
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

    /// Returns the first row that has no base version.
    #[must_use]
    pub fn first_missing_base(&self) -> Option<&RowMutation> {
        self.rows.iter().find(|row| row.base_version.is_none())
    }

    /// Returns the proposed row changes.
    #[must_use]
    pub fn rows(&self) -> &[RowMutation] {
        &self.rows
    }

    /// Returns the canonical body digest.
    ///
    /// The digest covers the idempotency key and every row with its base
    /// version. The digest does not cover the request identity, the principal,
    /// the policy version, or the shard map. A retry with the same key and
    /// body matches, even when the request identity changes.
    #[must_use]
    pub fn input_digest(&self) -> Hash32 {
        let mut buffer = Vec::new();
        push_tag(&mut buffer, INPUT_TAG);
        push_bytes(&mut buffer, self.idempotency_key.as_bytes());
        push_u64(
            &mut buffer,
            u64::try_from(self.rows.len()).unwrap_or(u64::MAX),
        );
        for row in &self.rows {
            push_bytes(&mut buffer, row.shard_id.as_bytes());
            push_bytes(&mut buffer, row.key.as_bytes());
            push_option_u64(&mut buffer, row.base_version.map(KeyVersion::get));
            push_bytes(&mut buffer, &row.value);
        }
        Hash32::of(&buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_digest_ignores_the_request_identity() {
        let row = RowMutation::new(
            RowKey::new(b"row:1".to_vec()),
            ShardId::new(vec![1]),
            KeyVersion::new(0),
            b"value".to_vec(),
        );
        let first = WriteRequest::new(
            RequestId::from_bytes([1; 32]),
            IdempotencyKey::new(vec![1]),
            vec![row.clone()],
        );
        let second = WriteRequest::new(
            RequestId::from_bytes([2; 32]),
            IdempotencyKey::new(vec![1]),
            vec![row],
        );
        assert_eq!(first.input_digest(), second.input_digest());
    }
}
