//! Replay and re-validation helpers.

use oxkv_contracts::version::CommitVersion;

use crate::batch::Transaction;
use crate::error::HookError;
use crate::pipeline::{CommitOutcome, HookPipeline, ValidatedBatch};

/// One recorded operation log entry and its commit position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRecord {
    transaction: Transaction,
    commit_version: CommitVersion,
}

impl ReplayRecord {
    /// Creates one replay record.
    #[must_use]
    pub const fn new(transaction: Transaction, commit_version: CommitVersion) -> Self {
        Self {
            transaction,
            commit_version,
        }
    }

    /// Returns the recorded transaction.
    #[must_use]
    pub const fn transaction(&self) -> &Transaction {
        &self.transaction
    }

    /// Returns the recorded commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }
}

/// Replays a recorded operation log.
///
/// # Errors
///
/// Returns the first named hook failure from the recorded entries.
pub fn replay(
    pipeline: &HookPipeline,
    records: &[ReplayRecord],
) -> Result<Vec<CommitOutcome>, HookError> {
    records
        .iter()
        .map(|record| pipeline.execute(record.transaction(), record.commit_version()))
        .collect()
}

/// Runs pre-write validation again for one transaction.
///
/// # Errors
///
/// Returns the first named hook failure.
pub fn revalidate<'a>(
    pipeline: &HookPipeline,
    transaction: &'a Transaction,
) -> Result<ValidatedBatch<'a>, HookError> {
    pipeline.validate(transaction)
}
