//! The receipt ledger.
//!
//! The ledger stores receipts under their idempotency key. The same key and
//! the same body return the same receipt. The same key and a different body
//! fail with `write.idempotency_key_reuse` and create no second receipt.
//!
//! A terminal receipt is immutable. The ledger retains it under its key.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptEnvelope, ReceiptOutcome, ReceiptState};
use oxkv_contracts::{CommitVersion, KeyVersion, ReceiptId, ReceiptSequence};

use crate::cursor::ReceiptCursor;
use crate::entry::RequestDigest;

/// One stored receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptRecord {
    receipt: ReceiptEnvelope,
    body_digest: RequestDigest,
    sequence: ReceiptSequence,
}

impl ReceiptRecord {
    /// Returns the receipt envelope.
    #[must_use]
    pub const fn receipt(&self) -> &ReceiptEnvelope {
        &self.receipt
    }

    /// Returns the receipt identity.
    #[must_use]
    pub const fn receipt_id(&self) -> ReceiptId {
        self.receipt.receipt_id()
    }

    /// Returns the idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        self.receipt.idempotency_key()
    }

    /// Returns the receipt state.
    #[must_use]
    pub const fn state(&self) -> ReceiptState {
        self.receipt.state()
    }

    /// Returns true when the receipt is terminal.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        self.receipt.is_terminal()
    }

    /// Returns the request body digest.
    #[must_use]
    pub const fn body_digest(&self) -> RequestDigest {
        self.body_digest
    }

    /// Returns the ledger sequence.
    #[must_use]
    pub const fn sequence(&self) -> ReceiptSequence {
        self.sequence
    }

    /// Returns the shard commit version of a `Committed` receipt.
    #[must_use]
    pub const fn commit_version(&self) -> Option<CommitVersion> {
        self.receipt.commit_version()
    }

    /// Returns the current key version of a `Conflict` receipt.
    #[must_use]
    pub const fn current_key_version(&self) -> Option<KeyVersion> {
        self.receipt.current_key_version()
    }

    /// Returns the reason code.
    #[must_use]
    pub const fn reason_code(&self) -> Option<&'static str> {
        self.receipt.reason_code()
    }

    /// Returns a cursor that points at this record.
    #[must_use]
    pub const fn cursor(&self, expires_at: ReceiptSequence) -> ReceiptCursor {
        ReceiptCursor::new(self.receipt_id(), self.sequence, expires_at)
    }
}

/// An error from the receipt ledger.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    /// The key already names a different body.
    #[error("The idempotency key names a different body.")]
    KeyReuse {
        /// The reused key.
        key: IdempotencyKey,
    },
    /// The receipt identity is not in the ledger.
    #[error("The receipt is not found.")]
    NotFound {
        /// The unknown receipt identity.
        receipt_id: ReceiptId,
    },
    /// The cursor is past receipt retention.
    #[error("The receipt cursor is past retention.")]
    Expired {
        /// The cursor sequence.
        cursor: ReceiptSequence,
        /// The oldest retained sequence.
        oldest_retained: ReceiptSequence,
    },
    /// The receipt cannot make the requested state move.
    #[error("The receipt cannot move from {current} to {next}.")]
    StateInvalid {
        /// The current state.
        current: ReceiptState,
        /// The requested state.
        next: ReceiptState,
    },
}

impl LedgerError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::KeyReuse { .. } => "write.idempotency_key_reuse",
            Self::NotFound { .. } => "receipt.not_found",
            Self::Expired { .. } => "receipt.expired",
            Self::StateInvalid { .. } => "receipt.state_invalid",
        }
    }
}

/// The receipt ledger.
#[derive(Debug, Clone)]
pub struct ReceiptLedger {
    entries: BTreeMap<IdempotencyKey, ReceiptRecord>,
    next_sequence: u64,
    retention_floor: ReceiptSequence,
}

impl ReceiptLedger {
    /// Creates an empty ledger.
    ///
    /// `retention_floor` is the oldest sequence that the authority retains.
    /// The caller injects the value.
    #[must_use]
    pub fn new(retention_floor: ReceiptSequence) -> Self {
        Self {
            entries: BTreeMap::new(),
            next_sequence: 1,
            retention_floor,
        }
    }

    /// Returns the oldest retained receipt sequence.
    #[must_use]
    pub const fn retention_floor(&self) -> ReceiptSequence {
        self.retention_floor
    }

    /// Sets the oldest retained receipt sequence.
    ///
    /// The caller injects the value. The ledger never reads a clock.
    pub const fn set_retention_floor(&mut self, retention_floor: ReceiptSequence) {
        self.retention_floor = retention_floor;
    }

    /// Returns the receipt count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the ledger holds no receipt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records one receipt and returns the stored record.
    ///
    /// A duplicate of the same key and body returns the stored record. A
    /// terminal stored receipt never changes. A queued stored receipt closes
    /// when the incoming receipt is terminal.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::KeyReuse`] when the key names a different body.
    /// Returns [`LedgerError::StateInvalid`] when a terminal envelope has no
    /// terminal outcome.
    pub fn submit(
        &mut self,
        body_digest: RequestDigest,
        receipt: ReceiptEnvelope,
    ) -> Result<ReceiptRecord, LedgerError> {
        let key = receipt.idempotency_key().clone();
        if let Some(existing) = self.entries.get_mut(&key) {
            if existing.body_digest != body_digest {
                return Err(LedgerError::KeyReuse { key });
            }
            if existing.receipt.is_terminal() || !receipt.is_terminal() {
                return Ok(existing.clone());
            }
            let current = existing.receipt.state();
            let outcome = receipt_outcome(&receipt).ok_or_else(|| LedgerError::StateInvalid {
                current,
                next: receipt.state(),
            })?;
            existing
                .receipt
                .close(outcome)
                .map_err(|_| LedgerError::StateInvalid {
                    current,
                    next: receipt.state(),
                })?;
            return Ok(existing.clone());
        }

        let sequence = ReceiptSequence::new(self.next_sequence);
        self.next_sequence = self.next_sequence.saturating_add(1);
        let record = ReceiptRecord {
            receipt,
            body_digest,
            sequence,
        };
        self.entries.insert(key, record.clone());
        Ok(record)
    }

    /// Returns one record by idempotency key.
    #[must_use]
    pub fn get(&self, key: &IdempotencyKey) -> Option<&ReceiptRecord> {
        self.entries.get(key)
    }

    /// Returns one record by receipt identity.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::NotFound`] when the identity is absent.
    pub fn read(&self, receipt_id: ReceiptId) -> Result<&ReceiptRecord, LedgerError> {
        self.entries
            .values()
            .find(|record| record.receipt_id() == receipt_id)
            .ok_or(LedgerError::NotFound { receipt_id })
    }

    /// Returns the receipt states in sequence order.
    ///
    /// A local disposition never appears in this vector.
    #[must_use]
    pub fn receipt_states(&self) -> Vec<ReceiptState> {
        let mut records: Vec<&ReceiptRecord> = self.entries.values().collect();
        records.sort_by_key(|record| record.sequence());
        records.iter().map(|record| record.state()).collect()
    }

    /// Returns every retained record in sequence order.
    #[must_use]
    pub fn records(&self) -> Vec<ReceiptRecord> {
        let mut records: Vec<ReceiptRecord> = self.entries.values().cloned().collect();
        records.sort_by_key(ReceiptRecord::sequence);
        records
    }

    /// Returns a cursor that points at the newest record.
    #[must_use]
    pub fn latest_cursor(&self, expires_at: ReceiptSequence) -> ReceiptCursor {
        self.entries
            .values()
            .max_by_key(|record| record.sequence())
            .map_or_else(
                || ReceiptCursor::start(expires_at),
                |record| ReceiptCursor::new(record.receipt_id(), record.sequence(), expires_at),
            )
    }

    /// Returns every record after the cursor position.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::Expired`] when the cursor is past retention.
    /// Returns [`LedgerError::NotFound`] when the cursor names an unknown
    /// receipt.
    pub fn resume(&self, cursor: &ReceiptCursor) -> Result<Vec<ReceiptRecord>, LedgerError> {
        if cursor.is_expired_at(self.retention_floor) {
            return Err(LedgerError::Expired {
                cursor: cursor.sequence(),
                oldest_retained: self.retention_floor,
            });
        }
        if cursor.sequence().get() > 0
            && !self
                .entries
                .values()
                .any(|record| record.receipt_id() == cursor.last_receipt_id())
        {
            return Err(LedgerError::NotFound {
                receipt_id: cursor.last_receipt_id(),
            });
        }
        let mut records: Vec<ReceiptRecord> = self
            .entries
            .values()
            .filter(|record| record.sequence() > cursor.sequence())
            .cloned()
            .collect();
        records.sort_by_key(ReceiptRecord::sequence);
        Ok(records)
    }
}

/// Reads the terminal outcome from a receipt envelope.
pub(crate) fn receipt_outcome(receipt: &ReceiptEnvelope) -> Option<ReceiptOutcome> {
    match receipt.state() {
        ReceiptState::Queued => None,
        ReceiptState::Committed => receipt
            .commit_version()
            .map(|commit_version| ReceiptOutcome::Committed { commit_version }),
        ReceiptState::Rejected => receipt
            .reason_code()
            .map(|reason| ReceiptOutcome::Rejected { reason }),
        ReceiptState::Conflict => match (receipt.current_key_version(), receipt.reason_code()) {
            (Some(current_key_version), Some(reason)) => Some(ReceiptOutcome::Conflict {
                current_key_version,
                reason,
            }),
            _ => None,
        },
    }
}
