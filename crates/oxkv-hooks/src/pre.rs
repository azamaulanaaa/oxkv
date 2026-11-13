//! The pre-write hook contract and its validator view.

use oxkv_contracts::version::ShardId;

use crate::batch::{Key, PriorRow, StagedOp, Transaction};
use crate::error::HookError;
use crate::rule::RuleId;

/// A deterministic pre-write validator.
///
/// The pipeline calls one validator for each operation in declaration order.
/// The validator returns an accepted verdict or one named error. It cannot
/// rewrite the batch, read a clock, draw randomness, perform I/O, or keep
/// state.
pub trait PreWriteHook: Send + Sync {
    /// Returns the stable rule id.
    fn rule_id(&self) -> &RuleId;

    /// Validates the current operation.
    ///
    /// # Errors
    ///
    /// Returns one named [`HookError`] to reject the batch.
    fn validate(&self, view: &ValidatorView<'_>) -> Result<(), HookError>;
}

/// The read-only envelope for one pre-write validation.
///
/// The view exposes the final staged value for every touched key. The key
/// under validation has one exception. The view returns its pre-transaction
/// value, even when the transaction also stages that key.
pub struct ValidatorView<'a> {
    transaction: &'a Transaction,
    op_index: u32,
    index: usize,
}

impl<'a> ValidatorView<'a> {
    pub(crate) fn new(transaction: &'a Transaction, index: usize) -> Self {
        Self {
            transaction,
            op_index: u32::try_from(index).unwrap_or(u32::MAX),
            index,
        }
    }

    /// Returns the current operation.
    #[must_use]
    pub fn op(&self) -> &'a StagedOp {
        &self.transaction.ops()[self.index]
    }

    /// Returns the current operation index in declaration order.
    #[must_use]
    pub const fn op_index(&self) -> u32 {
        self.op_index
    }

    /// Returns the number of operations in the batch.
    #[must_use]
    pub fn batch_len(&self) -> usize {
        self.transaction.op_count()
    }

    /// Returns the key under validation.
    #[must_use]
    pub fn current_key(&self) -> &'a Key {
        self.op().key()
    }

    /// Returns the pre-transaction value for the key under validation.
    #[must_use]
    pub fn pre_value(&self) -> Option<&'a PriorRow> {
        self.transaction
            .pre_state()
            .get(self.current_key())
            .and_then(Option::as_ref)
    }

    /// Returns the final staged value for the key under validation.
    #[must_use]
    pub fn staged_value(&self) -> Option<&'a [u8]> {
        match self.transaction.staged_state().get(self.current_key()) {
            Some(Some(value)) => Some(value),
            Some(None) | None => None,
        }
    }

    /// Reads one touched key.
    ///
    /// The key under validation returns its pre-transaction value. Every
    /// other key returns its final staged value.
    ///
    /// # Errors
    ///
    /// Returns `policy.unknown_field` when the key is not in the envelope.
    pub fn value(&self, key: &Key) -> Result<Option<&'a [u8]>, HookError> {
        if key == self.current_key() {
            Ok(self.pre_value().map(PriorRow::value))
        } else {
            self.staged_value_for(key)
        }
    }

    /// Reads the final staged value of one touched key.
    ///
    /// # Errors
    ///
    /// Returns `policy.unknown_field` when the key is not in the envelope.
    pub fn staged_value_for(&self, key: &Key) -> Result<Option<&'a [u8]>, HookError> {
        match self.transaction.staged_state().get(key) {
            Some(Some(value)) => Ok(Some(value)),
            Some(None) => Ok(None),
            None => Err(HookError::unknown_field(
                "key",
                "The rule reads a key outside the transaction envelope.",
            )
            .with_key(key.clone())),
        }
    }

    /// Returns true when the key is in the envelope.
    #[must_use]
    pub fn contains_key(&self, key: &Key) -> bool {
        self.transaction.pre_state().contains_key(key)
    }

    /// Returns the shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &'a ShardId {
        self.transaction.shard_id()
    }
}
