//! The post-write hook contract and its committed view.

use std::collections::BTreeMap;

use oxkv_contracts::version::{CommitVersion, ShardId};

use crate::batch::{Key, PriorRow, StagedOp, Transaction};
use crate::error::HookError;
use crate::event::EventDraft;
use crate::rule::RuleId;

/// A pure and replayable post-write rule.
///
/// The pipeline calls one rule for each operation in declaration order. The
/// rule returns one notification, no notification, or one named error. It
/// cannot perform I/O. A rule failure aborts the batch.
pub trait PostWriteHook: Send + Sync {
    /// Returns the stable rule id.
    fn rule_id(&self) -> &RuleId;

    /// Derives one notification for the current committed operation.
    ///
    /// # Errors
    ///
    /// Returns one named [`HookError`] to abort the batch.
    fn derive(&self, view: &CommittedView<'_>) -> Result<Option<EventDraft>, HookError>;
}

/// The read-only envelope for one post-write derivation.
///
/// The view exposes the committed value of every touched key. The value is
/// the final staged value for the batch.
pub struct CommittedView<'a> {
    transaction: &'a Transaction,
    final_state: &'a BTreeMap<Key, Option<Vec<u8>>>,
    index: usize,
    op_index: u32,
    commit_version: CommitVersion,
}

impl<'a> CommittedView<'a> {
    pub(crate) fn new(
        transaction: &'a Transaction,
        final_state: &'a BTreeMap<Key, Option<Vec<u8>>>,
        index: usize,
        commit_version: CommitVersion,
    ) -> Self {
        Self {
            transaction,
            final_state,
            index,
            op_index: u32::try_from(index).unwrap_or(u32::MAX),
            commit_version,
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

    /// Returns the key of the current operation.
    #[must_use]
    pub fn key(&self) -> &'a Key {
        self.op().key()
    }

    /// Returns the pre-transaction value of the key.
    #[must_use]
    pub fn prior_value(&self) -> Option<&'a PriorRow> {
        self.transaction
            .pre_state()
            .get(self.key())
            .and_then(Option::as_ref)
    }

    /// Returns the committed value of the key.
    #[must_use]
    pub fn value_after(&self) -> Option<&'a [u8]> {
        match self.final_state.get(self.key()) {
            Some(Some(value)) => Some(value),
            Some(None) | None => None,
        }
    }

    /// Reads the committed value of one touched key.
    ///
    /// # Errors
    ///
    /// Returns `policy.unknown_field` when the key is not in the envelope.
    pub fn value_for(&self, key: &Key) -> Result<Option<&'a [u8]>, HookError> {
        match self.final_state.get(key) {
            Some(Some(value)) => Ok(Some(value)),
            Some(None) => Ok(None),
            None => Err(HookError::unknown_field(
                "key",
                "The rule reads a key outside the transaction envelope.",
            )
            .with_key(key.clone())),
        }
    }

    /// Returns the commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &'a ShardId {
        self.transaction.shard_id()
    }
}
