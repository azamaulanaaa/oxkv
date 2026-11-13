//! Point-read helpers that use the same pin as a search.
//!
//! The helpers are pure functions over a finite row list. They do not read
//! storage. A caller uses them to prove that a search hit and a get agree at
//! one pin.

use oxkv_contracts::version::{CommitVersion, ShardId};

use crate::pin::SearchPin;

/// One committed row version.
///
/// A value of `None` is a tombstone. A tombstone hides every older version of
/// the same key at and after its commit version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowVersion {
    key: Vec<u8>,
    shard_id: ShardId,
    commit_version: CommitVersion,
    value: Option<Vec<u8>>,
}

impl RowVersion {
    /// Creates a live row version.
    #[must_use]
    pub fn put(
        key: impl Into<Vec<u8>>,
        shard_id: ShardId,
        commit_version: CommitVersion,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            key: key.into(),
            shard_id,
            commit_version,
            value: Some(value.into()),
        }
    }

    /// Creates a tombstone row version.
    #[must_use]
    pub fn delete(
        key: impl Into<Vec<u8>>,
        shard_id: ShardId,
        commit_version: CommitVersion,
    ) -> Self {
        Self {
            key: key.into(),
            shard_id,
            commit_version,
            value: None,
        }
    }

    /// Returns the canonical key bytes.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Returns the owning shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the commit version of this row version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the value, or `None` for a tombstone.
    #[must_use]
    pub fn value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }

    /// Returns true when the row version is visible at the pin.
    #[must_use]
    pub fn is_visible_at(&self, pin: &SearchPin) -> bool {
        pin.version_vector()
            .get(&self.shard_id)
            .is_some_and(|version| self.commit_version <= version)
    }
}

/// Returns the newest visible row version for one key.
///
/// The result can be a tombstone. Use [`get_value_at`] for a live value.
#[must_use]
pub fn get_at<'a>(pin: &SearchPin, rows: &'a [RowVersion], key: &[u8]) -> Option<&'a RowVersion> {
    rows.iter()
        .filter(|row| row.key() == key && row.is_visible_at(pin))
        .max_by_key(|row| row.commit_version())
}

/// Returns the live value for one key at the pin.
#[must_use]
pub fn get_value_at<'a>(pin: &SearchPin, rows: &'a [RowVersion], key: &[u8]) -> Option<&'a [u8]> {
    get_at(pin, rows, key).and_then(RowVersion::value)
}
