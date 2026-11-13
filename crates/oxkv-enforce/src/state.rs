//! In-memory shard state for the commit model.
//!
//! The enforcement crate owns no durable storage. This module models the data,
//! index, and business events that one shard transaction writes.

use std::collections::BTreeMap;

use oxkv_contracts::version::{CommitVersion, Hash32, KeyVersion};

use crate::request::RowKey;

/// One stored row value and its key version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRow {
    value: Vec<u8>,
    key_version: KeyVersion,
}

impl StoredRow {
    /// Creates a stored row.
    #[must_use]
    pub fn new(value: Vec<u8>, key_version: KeyVersion) -> Self {
        Self { value, key_version }
    }

    /// Returns the stored value bytes.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }

    /// Returns the key version.
    #[must_use]
    pub const fn key_version(&self) -> KeyVersion {
        self.key_version
    }
}

/// One index entry that a committed row produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// The row key.
    pub row_key: RowKey,
    /// The digest of the row value.
    pub value_digest: Hash32,
}

/// One business event that a committed row produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessEvent {
    /// The row key.
    pub key: RowKey,
    /// The digest of the row value.
    pub value_digest: Hash32,
    /// The shard commit version.
    pub commit_version: CommitVersion,
}

/// The data, index, and event changes of one committed write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedEffects {
    /// The resulting shard commit version.
    pub commit_version: CommitVersion,
    /// The changed row keys.
    pub rows: Vec<RowKey>,
    /// The written index entries.
    pub index: Vec<IndexEntry>,
    /// The appended business events.
    pub events: Vec<BusinessEvent>,
}

/// The in-memory state of one shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardState {
    rows: BTreeMap<RowKey, StoredRow>,
    index: BTreeMap<RowKey, Hash32>,
    events: Vec<BusinessEvent>,
    commit_version: CommitVersion,
}

impl ShardState {
    /// Creates an empty shard state at commit version zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: BTreeMap::new(),
            index: BTreeMap::new(),
            events: Vec::new(),
            commit_version: CommitVersion::new(0),
        }
    }

    /// Returns one stored row.
    #[must_use]
    pub fn row(&self, key: &RowKey) -> Option<&StoredRow> {
        self.rows.get(key)
    }

    /// Returns the stored rows.
    #[must_use]
    pub const fn rows(&self) -> &BTreeMap<RowKey, StoredRow> {
        &self.rows
    }

    /// Returns the index entries.
    #[must_use]
    pub const fn index(&self) -> &BTreeMap<RowKey, Hash32> {
        &self.index
    }

    /// Returns the business events.
    #[must_use]
    pub fn events(&self) -> &[BusinessEvent] {
        &self.events
    }

    /// Returns the current shard commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Applies one committed transaction.
    pub(crate) fn apply(
        &mut self,
        commit_version: CommitVersion,
        rows: &[(RowKey, StoredRow)],
        index: &[IndexEntry],
        events: &[BusinessEvent],
    ) {
        for (key, row) in rows {
            self.rows.insert(key.clone(), row.clone());
        }
        for entry in index {
            self.index.insert(entry.row_key.clone(), entry.value_digest);
        }
        self.events.extend_from_slice(events);
        self.commit_version = commit_version;
    }
}

impl Default for ShardState {
    fn default() -> Self {
        Self::new()
    }
}
