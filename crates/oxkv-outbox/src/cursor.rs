//! Receipt cursor with an explicit expiry.
//!
//! The cursor names the last receipt that the client read. It carries the
//! receipt identity, the receipt sequence, and the retention sequence at which
//! the cursor stops being valid.
//!
//! A cursor past receipt retention fails with `receipt.expired`.

use oxkv_contracts::ReceiptCursor as ContractReceiptCursor;
use oxkv_contracts::{ReceiptId, ReceiptSequence};

/// A cursor over the receipt ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReceiptCursor {
    last_receipt_id: ReceiptId,
    sequence: ReceiptSequence,
    expires_at: ReceiptSequence,
}

impl ReceiptCursor {
    /// The named error for an expired cursor.
    pub const EXPIRY_ERROR: &'static str = "receipt.expired";

    /// Creates a receipt cursor.
    #[must_use]
    pub const fn new(
        last_receipt_id: ReceiptId,
        sequence: ReceiptSequence,
        expires_at: ReceiptSequence,
    ) -> Self {
        Self {
            last_receipt_id,
            sequence,
            expires_at,
        }
    }

    /// Creates the cursor that starts before the first receipt.
    #[must_use]
    pub const fn start(expires_at: ReceiptSequence) -> Self {
        Self::new(
            ReceiptId::from_bytes([0; 32]),
            ReceiptSequence::new(0),
            expires_at,
        )
    }

    /// Returns the last receipt identity.
    #[must_use]
    pub const fn last_receipt_id(&self) -> ReceiptId {
        self.last_receipt_id
    }

    /// Returns the last receipt sequence.
    #[must_use]
    pub const fn sequence(&self) -> ReceiptSequence {
        self.sequence
    }

    /// Returns the retention sequence at which the cursor stops being valid.
    #[must_use]
    pub const fn expires_at(&self) -> ReceiptSequence {
        self.expires_at
    }

    /// Returns the named error for an expired cursor.
    #[must_use]
    pub const fn expiry_error() -> &'static str {
        Self::EXPIRY_ERROR
    }

    /// Returns true when the cursor is past retention.
    ///
    /// The cursor is expired when the oldest retained receipt sequence is
    /// above the cursor sequence or above the cursor expiry.
    #[must_use]
    pub const fn is_expired_at(&self, oldest_retained: ReceiptSequence) -> bool {
        self.sequence.get() < oldest_retained.get() || self.expires_at.get() < oldest_retained.get()
    }

    /// Returns true when the cursor is still readable.
    #[must_use]
    pub const fn is_readable_at(&self, oldest_retained: ReceiptSequence) -> bool {
        !self.is_expired_at(oldest_retained)
    }

    /// Converts the contract cursor into a cursor with an expiry.
    #[must_use]
    pub const fn from_contract(cursor: ContractReceiptCursor, expires_at: ReceiptSequence) -> Self {
        Self::new(cursor.last_receipt_id, cursor.sequence, expires_at)
    }

    /// Converts the cursor into the contract cursor.
    #[must_use]
    pub const fn to_contract(self) -> ContractReceiptCursor {
        ContractReceiptCursor::new(self.last_receipt_id, self.sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_at_the_retention_floor_is_readable() {
        let cursor = ReceiptCursor::new(
            ReceiptId::from_bytes([1; 32]),
            ReceiptSequence::new(4),
            ReceiptSequence::new(4),
        );
        assert!(cursor.is_readable_at(ReceiptSequence::new(4)));
        assert!(!cursor.is_expired_at(ReceiptSequence::new(4)));
    }

    #[test]
    fn a_cursor_below_the_retention_floor_is_expired() {
        let cursor = ReceiptCursor::new(
            ReceiptId::from_bytes([1; 32]),
            ReceiptSequence::new(4),
            ReceiptSequence::new(4),
        );
        assert!(cursor.is_expired_at(ReceiptSequence::new(5)));
        assert_eq!(ReceiptCursor::expiry_error(), "receipt.expired");
    }

    #[test]
    fn a_start_cursor_carries_no_receipt_identity() {
        let cursor = ReceiptCursor::start(ReceiptSequence::new(9));
        assert_eq!(cursor.sequence(), ReceiptSequence::new(0));
        assert_eq!(cursor.last_receipt_id(), ReceiptId::from_bytes([0; 32]));
        assert_eq!(cursor.expires_at(), ReceiptSequence::new(9));
    }

    #[test]
    fn the_contract_cursor_round_trips() {
        let cursor = ReceiptCursor::new(
            ReceiptId::from_bytes([7; 32]),
            ReceiptSequence::new(3),
            ReceiptSequence::new(8),
        );
        let contract = cursor.to_contract();
        assert_eq!(contract.sequence, ReceiptSequence::new(3));
        assert_eq!(
            ReceiptCursor::from_contract(contract, ReceiptSequence::new(8)),
            cursor
        );
    }
}
