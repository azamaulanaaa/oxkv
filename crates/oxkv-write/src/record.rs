//! Stored key records and the commit plan.

use oxkv_contracts::version::{CommitVersion, KeyVersion, ShardId};

use crate::request::{Key, WriteOperation};

/// The stored state of one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyValue {
    /// The key holds a value.
    Value(Vec<u8>),
    /// The key holds a tombstone.
    Tombstone,
}

impl KeyValue {
    /// Returns the value bytes. A tombstone returns `None`.
    #[must_use]
    pub fn value(&self) -> Option<&[u8]> {
        match self {
            Self::Value(value) => Some(value),
            Self::Tombstone => None,
        }
    }

    /// Returns true when the state is a tombstone.
    #[must_use]
    pub const fn is_tombstone(&self) -> bool {
        matches!(self, Self::Tombstone)
    }
}

/// One stored key record.
///
/// An absent key has no record. Its current key version is zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRecord {
    key_version: KeyVersion,
    value: KeyValue,
}

impl KeyRecord {
    /// Creates a key record.
    #[must_use]
    pub const fn new(key_version: KeyVersion, value: KeyValue) -> Self {
        Self { key_version, value }
    }

    /// Returns the key version.
    #[must_use]
    pub const fn key_version(&self) -> KeyVersion {
        self.key_version
    }

    /// Returns the stored value or tombstone.
    #[must_use]
    pub const fn value(&self) -> &KeyValue {
        &self.value
    }

    /// Returns the value bytes. A tombstone returns `None`.
    #[must_use]
    pub fn as_value(&self) -> Option<&[u8]> {
        self.value.value()
    }

    /// Returns true when the record is a tombstone.
    #[must_use]
    pub const fn is_tombstone(&self) -> bool {
        self.value.is_tombstone()
    }
}

/// One key change in a proposed commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChange {
    key: Key,
    base_version: KeyVersion,
    key_version: KeyVersion,
    previous_version: KeyVersion,
    previous_value: Option<Vec<u8>>,
    operation: WriteOperation,
}

impl KeyChange {
    /// Creates a key change.
    pub(crate) const fn new(
        key: Key,
        base_version: KeyVersion,
        key_version: KeyVersion,
        previous_version: KeyVersion,
        previous_value: Option<Vec<u8>>,
        operation: WriteOperation,
    ) -> Self {
        Self {
            key,
            base_version,
            key_version,
            previous_version,
            previous_value,
            operation,
        }
    }

    /// Returns the key.
    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the base version named by the request.
    #[must_use]
    pub const fn base_version(&self) -> KeyVersion {
        self.base_version
    }

    /// Returns the new key version.
    #[must_use]
    pub const fn key_version(&self) -> KeyVersion {
        self.key_version
    }

    /// Returns the key version before the commit.
    #[must_use]
    pub const fn previous_version(&self) -> KeyVersion {
        self.previous_version
    }

    /// Returns the live value before the commit.
    ///
    /// The value is `None` when the key was absent or held a tombstone.
    #[must_use]
    pub fn previous_value(&self) -> Option<&[u8]> {
        self.previous_value.as_deref()
    }

    /// Returns the committed operation.
    #[must_use]
    pub const fn operation(&self) -> &WriteOperation {
        &self.operation
    }
}

/// The durable summary of one committed write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSummary {
    shard_id: ShardId,
    commit_version: CommitVersion,
    changes: Vec<KeyChange>,
}

impl CommitSummary {
    /// Creates a commit summary.
    pub(crate) const fn new(
        shard_id: ShardId,
        commit_version: CommitVersion,
        changes: Vec<KeyChange>,
    ) -> Self {
        Self {
            shard_id,
            commit_version,
            changes,
        }
    }

    /// Returns the shard that owns the commit.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the shard commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the key changes in request order.
    #[must_use]
    pub fn changes(&self) -> &[KeyChange] {
        &self.changes
    }
}

/// The proposed changes that a policy or hook can inspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitView<'a> {
    shard_id: &'a ShardId,
    commit_version: CommitVersion,
    changes: &'a [KeyChange],
}

impl<'a> CommitView<'a> {
    /// Creates a commit view.
    pub(crate) const fn new(
        shard_id: &'a ShardId,
        commit_version: CommitVersion,
        changes: &'a [KeyChange],
    ) -> Self {
        Self {
            shard_id,
            commit_version,
            changes,
        }
    }

    /// Returns the shard that owns the commit.
    #[must_use]
    pub const fn shard_id(&self) -> &'a ShardId {
        self.shard_id
    }

    /// Returns the shard commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the proposed key changes.
    #[must_use]
    pub const fn changes(&self) -> &'a [KeyChange] {
        self.changes
    }
}
