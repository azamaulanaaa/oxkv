//! The client outbox.
//!
//! The outbox is separate from the mirror. It holds write intents, idempotency
//! keys, base versions, scope identities, payload digests, and last receipts.
//! The outbox never applies a mutation to the mirror.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::{ContentDigest, KeyVersion, ScopeIdentity};

use crate::codec::as_u64;
use crate::error::ReplicaError;

/// One write intent in the outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRecord {
    /// The client idempotency key.
    pub idempotency_key: String,
    /// The base key version of the write.
    pub base_version: KeyVersion,
    /// The scope identity of the write.
    pub scope_identity: ScopeIdentity,
    /// The digest of the write payload.
    pub payload_digest: ContentDigest,
    /// The last receipt state. `None` means the write is not submitted.
    pub last_receipt: Option<ReceiptState>,
}

impl OutboxRecord {
    /// Creates an outbox record.
    #[must_use]
    pub const fn new(
        idempotency_key: String,
        base_version: KeyVersion,
        scope_identity: ScopeIdentity,
        payload_digest: ContentDigest,
        last_receipt: Option<ReceiptState>,
    ) -> Self {
        Self {
            idempotency_key,
            base_version,
            scope_identity,
            payload_digest,
            last_receipt,
        }
    }

    /// Returns the resident byte count of the record.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        as_u64(self.idempotency_key.len())
            .saturating_add(40)
            .saturating_add(32)
    }
}

/// The client outbox.
#[derive(Debug, Clone, Default)]
pub struct Outbox {
    records: BTreeMap<String, OutboxRecord>,
}

impl Outbox {
    /// Creates an empty outbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one write intent.
    ///
    /// A repeated key with the same payload is a no-op. A repeated key with
    /// another payload is an error.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::IdempotencyKeyReuse`] when the key already names
    /// another payload.
    pub fn enqueue(&mut self, record: OutboxRecord) -> Result<(), ReplicaError> {
        if let Some(existing) = self.records.get(&record.idempotency_key) {
            if existing.payload_digest != record.payload_digest {
                return Err(ReplicaError::IdempotencyKeyReuse);
            }
            return Ok(());
        }
        self.records.insert(record.idempotency_key.clone(), record);
        Ok(())
    }

    /// Returns one write intent.
    #[must_use]
    pub fn get(&self, idempotency_key: &str) -> Option<&OutboxRecord> {
        self.records.get(idempotency_key)
    }

    /// Returns the record count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns true when the outbox is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Returns the resident byte count.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.records.values().fold(0_u64, |total, record| {
            total.saturating_add(record.resident_bytes())
        })
    }

    /// Iterates over the records in key order.
    pub fn iter(&self) -> impl Iterator<Item = &OutboxRecord> {
        self.records.values()
    }
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::receipt::ReceiptState;
    use oxkv_contracts::version::{ContentDigest, KeyVersion, ScopeIdentity};

    use super::{Outbox, OutboxRecord};

    fn record(key: &str, digest: u8) -> OutboxRecord {
        OutboxRecord::new(
            key.to_owned(),
            KeyVersion::new(1),
            ScopeIdentity::from_bytes([2; 32]),
            ContentDigest::from_bytes([digest; 32]),
            Some(ReceiptState::Queued),
        )
    }

    #[test]
    fn a_repeated_key_with_the_same_payload_is_a_no_op() {
        let mut outbox = Outbox::new();
        outbox.enqueue(record("a", 3)).expect("the record fits");
        outbox.enqueue(record("a", 3)).expect("the record repeats");
        assert_eq!(outbox.len(), 1);
    }

    #[test]
    fn a_repeated_key_with_another_payload_fails() {
        let mut outbox = Outbox::new();
        outbox.enqueue(record("a", 3)).expect("the record fits");
        assert_eq!(
            outbox
                .enqueue(record("a", 4))
                .expect_err("the payload differs")
                .code(),
            "write.idempotency_key_reuse"
        );
    }

    #[test]
    fn the_outbox_counts_its_bytes() {
        let mut outbox = Outbox::new();
        assert!(outbox.is_empty());
        assert_eq!(outbox.resident_bytes(), 0);
        outbox.enqueue(record("a", 3)).expect("the record fits");
        assert!(outbox.resident_bytes() > 0);
    }
}
