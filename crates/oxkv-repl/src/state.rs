//! The deterministic state machine that applies the committed log.

use core::fmt;
use std::collections::BTreeMap;

use oxkv_contracts::receipt::IdempotencyKey;
use oxkv_contracts::version::{CommitVersion, LeaseTerm};

use crate::driver::TerminalReceipt;
use crate::error::ReplError;
use crate::log::{Log, LogIndex};
use crate::read::{KeyValue, ReadBarrier, ReadView};

/// The result of one apply call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyReport {
    from: LogIndex,
    to: LogIndex,
    commit_version: CommitVersion,
}

impl ApplyReport {
    /// Creates an apply report.
    #[must_use]
    pub const fn new(from: LogIndex, to: LogIndex, commit_version: CommitVersion) -> Self {
        Self {
            from,
            to,
            commit_version,
        }
    }

    /// Returns the first applied index. Zero means that no record was applied.
    #[must_use]
    pub const fn from(self) -> LogIndex {
        self.from
    }

    /// Returns the last applied index.
    #[must_use]
    pub const fn to(self) -> LogIndex {
        self.to
    }

    /// Returns the commit version after the apply.
    #[must_use]
    pub const fn commit_version(self) -> CommitVersion {
        self.commit_version
    }
}

/// One applied data record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEntry {
    index: LogIndex,
    term: LeaseTerm,
    commit_version: CommitVersion,
    idempotency: IdempotencyKey,
    key: Vec<u8>,
    value: Vec<u8>,
}

impl AppliedEntry {
    /// Returns the log index.
    #[must_use]
    pub const fn index(&self) -> LogIndex {
        self.index
    }

    /// Returns the term of the record.
    #[must_use]
    pub const fn term(&self) -> LeaseTerm {
        self.term
    }

    /// Returns the commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the idempotency key.
    #[must_use]
    pub const fn idempotency(&self) -> &IdempotencyKey {
        &self.idempotency
    }

    /// Returns the user key.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Returns the user value.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

impl fmt::Display for AppliedEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "index={} term={} version={}",
            self.index, self.term, self.commit_version
        )
    }
}

/// The materialized state of one authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvState {
    applied: LogIndex,
    version: CommitVersion,
    entries: Vec<AppliedEntry>,
}

impl Default for KvState {
    fn default() -> Self {
        Self::new()
    }
}

impl KvState {
    /// Creates an empty state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            applied: LogIndex::ZERO,
            version: CommitVersion::new(0),
            entries: Vec::new(),
        }
    }

    /// Drops every applied record.
    ///
    /// A crash uses this method. The durable log stays. A later `apply` call
    /// rebuilds the same state from the log.
    pub fn reset(&mut self) {
        self.applied = LogIndex::ZERO;
        self.version = CommitVersion::new(0);
        self.entries.clear();
    }

    /// Returns the applied log index.
    #[must_use]
    pub const fn applied_index(&self) -> LogIndex {
        self.applied
    }

    /// Returns the current commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.version
    }

    /// Returns every applied data record in apply order.
    #[must_use]
    pub fn entries(&self) -> &[AppliedEntry] {
        &self.entries
    }

    /// Applies the log up to one index.
    ///
    /// The caller must not apply an index above the commit index. The apply is
    /// idempotent. A duplicate idempotency key does not get a second commit
    /// version.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the log has a gap, or
    /// when the target index is not in the log.
    pub fn apply(&mut self, log: &Log, up_to: LogIndex) -> Result<ApplyReport, ReplError> {
        let from = self.applied;
        if up_to < self.applied {
            return Ok(ApplyReport::new(from, self.applied, self.version));
        }
        let mut index = self.applied.next()?;
        while index <= up_to {
            let record = log.record(index).ok_or(ReplError::InvariantViolation {
                reason: "the apply log has a gap",
            })?;
            if let Some(idempotency) = record.payload().idempotency() {
                let duplicate = self
                    .entries
                    .iter()
                    .any(|entry| entry.idempotency == *idempotency);
                if !duplicate {
                    let next_version =
                        self.version
                            .get()
                            .checked_add(1)
                            .ok_or(ReplError::InvariantViolation {
                                reason: "the commit version overflowed",
                            })?;
                    self.version = CommitVersion::new(next_version);
                    if let crate::log::EntryPayload::Data { key, value, .. } = record.payload() {
                        self.entries.push(AppliedEntry {
                            index,
                            term: record.term(),
                            commit_version: self.version,
                            idempotency: idempotency.clone(),
                            key: key.clone(),
                            value: value.clone(),
                        });
                    }
                }
            }
            self.applied = index;
            index = index.next()?;
        }
        Ok(ApplyReport::new(from, self.applied, self.version))
    }

    /// Returns the state at one applied index.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::ReadBehindCommit`] when the index is above the
    /// applied index.
    pub fn read_at(&self, at: LogIndex) -> Result<ReadView, ReplError> {
        ReadBarrier::new(self.applied).admit(at)?;
        let mut values: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        let mut version = CommitVersion::new(0);
        let mut term = LeaseTerm::new(0);
        for entry in &self.entries {
            if entry.index > at {
                break;
            }
            values.insert(entry.key.clone(), entry.value.clone());
            if entry.commit_version > version {
                version = entry.commit_version;
            }
            term = entry.term;
        }
        let records = values
            .into_iter()
            .map(|(key, value)| KeyValue { key, value })
            .collect();
        Ok(ReadView::new(at, version, term, records))
    }

    /// Returns the committed receipt for one idempotency key.
    #[must_use]
    pub fn receipt_for(&self, idempotency: &IdempotencyKey) -> Option<TerminalReceipt> {
        self.entries
            .iter()
            .find(|entry| entry.idempotency == *idempotency)
            .map(|entry| TerminalReceipt::committed(entry.term, entry.index, entry.commit_version))
    }
}
