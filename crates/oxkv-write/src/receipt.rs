//! The durable receipt ledger.
//!
//! The ledger holds one receipt for each client and idempotency key pair. A
//! receipt has four states. `Queued` is live. The other three states are
//! terminal. A terminal receipt cannot change state.
//!
//! Two clients can use the same idempotency key. The ledger keeps the two
//! receipts apart. A retry by the same client returns the original receipt.
//!
//! The ledger stores the body digest beside the receipt. The authority uses the
//! digest to tell a retry from a reused key.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::{
    IdempotencyKey, ReceiptEnvelope, ReceiptOutcome, ReceiptState, ReceiptTransitionError,
};
use oxkv_contracts::version::Hash32;

use crate::request::ClientId;

/// One stored receipt and the body digest that it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReceipt {
    body_digest: Hash32,
    envelope: ReceiptEnvelope,
}

impl StoredReceipt {
    /// Creates a stored receipt.
    #[must_use]
    pub const fn new(body_digest: Hash32, envelope: ReceiptEnvelope) -> Self {
        Self {
            body_digest,
            envelope,
        }
    }

    /// Returns the body digest.
    #[must_use]
    pub const fn body_digest(&self) -> Hash32 {
        self.body_digest
    }

    /// Returns the receipt envelope.
    #[must_use]
    pub const fn envelope(&self) -> &ReceiptEnvelope {
        &self.envelope
    }
}

/// An error from the receipt ledger.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptLedgerError {
    /// The client already has a receipt for the idempotency key.
    #[error("The client already has a receipt for the idempotency key.")]
    DuplicateKey {
        /// The client that owns the receipt.
        client_id: ClientId,
        /// The duplicated key.
        idempotency_key: IdempotencyKey,
    },
    /// The receipt is not queued.
    #[error("The receipt is not queued. The state is {state}.")]
    NotQueued {
        /// The current state.
        state: ReceiptState,
    },
    /// The receipt is not terminal.
    #[error("The receipt is not terminal. The state is {state}.")]
    NotTerminal {
        /// The current state.
        state: ReceiptState,
    },
    /// The client has no receipt for the idempotency key.
    #[error("The client has no receipt for the idempotency key.")]
    UnknownKey {
        /// The client that owns the receipt.
        client_id: ClientId,
        /// The unknown key.
        idempotency_key: IdempotencyKey,
    },
    /// The receipt transition failed.
    #[error("{source}")]
    Transition {
        /// The transition error.
        #[from]
        source: ReceiptTransitionError,
    },
}

impl ReceiptLedgerError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DuplicateKey { .. } => "write.idempotency_key_reuse",
            Self::NotQueued { .. } | Self::NotTerminal { .. } | Self::Transition { .. } => {
                "receipt.state_invalid"
            }
            Self::UnknownKey { .. } => "receipt.not_found",
        }
    }
}

/// One receipt for each client and idempotency key pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReceiptLedger {
    entries: BTreeMap<(ClientId, IdempotencyKey), StoredReceipt>,
}

impl ReceiptLedger {
    /// Creates an empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of receipts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the ledger has no receipt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns one stored receipt.
    #[must_use]
    pub fn get(
        &self,
        client_id: &ClientId,
        idempotency_key: &IdempotencyKey,
    ) -> Option<&StoredReceipt> {
        self.entries
            .get(&(client_id.clone(), idempotency_key.clone()))
    }

    /// Returns one receipt envelope.
    #[must_use]
    pub fn receipt(
        &self,
        client_id: &ClientId,
        idempotency_key: &IdempotencyKey,
    ) -> Option<&ReceiptEnvelope> {
        self.get(client_id, idempotency_key)
            .map(StoredReceipt::envelope)
    }

    /// Returns true when the client has a receipt for the key.
    #[must_use]
    pub fn contains(&self, client_id: &ClientId, idempotency_key: &IdempotencyKey) -> bool {
        self.entries
            .contains_key(&(client_id.clone(), idempotency_key.clone()))
    }

    /// Iterates over the receipts in canonical client and key order.
    pub fn iter(&self) -> impl Iterator<Item = ((&ClientId, &IdempotencyKey), &StoredReceipt)> {
        self.entries
            .iter()
            .map(|((client_id, idempotency_key), stored)| ((client_id, idempotency_key), stored))
    }

    /// Queues one live receipt.
    ///
    /// # Errors
    ///
    /// Returns [`ReceiptLedgerError::DuplicateKey`] when the client already has
    /// a receipt for the key. Returns [`ReceiptLedgerError::NotQueued`] when the
    /// receipt is terminal.
    pub fn queue(
        &mut self,
        client_id: &ClientId,
        body_digest: Hash32,
        envelope: ReceiptEnvelope,
    ) -> Result<(), ReceiptLedgerError> {
        if envelope.state() != ReceiptState::Queued {
            return Err(ReceiptLedgerError::NotQueued {
                state: envelope.state(),
            });
        }
        self.insert_new(client_id, body_digest, envelope)
    }

    /// Inserts one terminal receipt.
    ///
    /// # Errors
    ///
    /// Returns [`ReceiptLedgerError::DuplicateKey`] when the client already has
    /// a receipt for the key. Returns [`ReceiptLedgerError::NotTerminal`] when
    /// the receipt is live.
    pub fn insert_terminal(
        &mut self,
        client_id: &ClientId,
        body_digest: Hash32,
        envelope: ReceiptEnvelope,
    ) -> Result<(), ReceiptLedgerError> {
        if !envelope.is_terminal() {
            return Err(ReceiptLedgerError::NotTerminal {
                state: envelope.state(),
            });
        }
        self.insert_new(client_id, body_digest, envelope)
    }

    /// Closes one queued receipt with a terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns [`ReceiptLedgerError::UnknownKey`] when the client has no receipt
    /// for the key. Returns [`ReceiptLedgerError::Transition`] when the receipt
    /// is already terminal.
    pub fn close(
        &mut self,
        client_id: &ClientId,
        idempotency_key: &IdempotencyKey,
        outcome: ReceiptOutcome,
    ) -> Result<(), ReceiptLedgerError> {
        let entry = self
            .entries
            .get_mut(&(client_id.clone(), idempotency_key.clone()))
            .ok_or_else(|| ReceiptLedgerError::UnknownKey {
                client_id: client_id.clone(),
                idempotency_key: idempotency_key.clone(),
            })?;
        entry
            .envelope
            .close(outcome)
            .map_err(|source| ReceiptLedgerError::Transition { source })?;
        Ok(())
    }

    /// Inserts a new receipt.
    fn insert_new(
        &mut self,
        client_id: &ClientId,
        body_digest: Hash32,
        envelope: ReceiptEnvelope,
    ) -> Result<(), ReceiptLedgerError> {
        let idempotency_key = envelope.idempotency_key().clone();
        let key = (client_id.clone(), idempotency_key.clone());
        if self.entries.contains_key(&key) {
            return Err(ReceiptLedgerError::DuplicateKey {
                client_id: client_id.clone(),
                idempotency_key,
            });
        }
        self.entries
            .insert(key, StoredReceipt::new(body_digest, envelope));
        Ok(())
    }
}
