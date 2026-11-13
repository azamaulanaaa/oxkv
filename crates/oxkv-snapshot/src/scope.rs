//! Deterministic `ScopeSnapshot` projection.

use std::collections::BTreeMap;

use oxkv_contracts::keyspace::{KEY_ENCODING_VERSION, SYSTEM_ID_PROJECTION, SYSTEM_MARKER};
use oxkv_contracts::version::{CommitVersion, PageId, ShardId};

use crate::error::SnapshotError;
use crate::page::Record;
use crate::snapshot::{ScopeShard, ScopeSnapshot, ScopeSnapshotParams};
use crate::{MAX_KEY_BYTES, MAX_VALUE_BYTES};

/// Answers whether one scope admits one key and value.
///
/// The snapshot crate does not depend on the policy or scope crate. A caller
/// supplies the predicate for its own scope.
pub trait ScopeAdmission {
    /// Returns true when the scope admits the key and value.
    fn admits(&self, key: &[u8], value: &[u8]) -> bool;
}

impl<F> ScopeAdmission for F
where
    F: Fn(&[u8], &[u8]) -> bool,
{
    fn admits(&self, key: &[u8], value: &[u8]) -> bool {
        self(key, value)
    }
}

/// Returns the system identifier of a v1 key.
///
/// The function reads the key encoding version, the table terminator, and the
/// system marker. It returns `None` for a user key or a malformed key.
#[must_use]
pub fn system_namespace_id(key: &[u8]) -> Option<u8> {
    if key.first().copied() != Some(KEY_ENCODING_VERSION) {
        return None;
    }
    let rest = key.get(1..)?;
    let terminator = rest.iter().position(|byte| *byte == 0x00)?;
    let marker = terminator.checked_add(2)?;
    if key.get(marker).copied() != Some(SYSTEM_MARKER) {
        return None;
    }
    key.get(marker.checked_add(1)?).copied()
}

/// Returns the forbidden system namespace of a scope key.
///
/// A scope snapshot must not carry receipt, event, audit, lease, index,
/// statistics, or unknown system data. It can carry a projection fact.
#[must_use]
pub fn forbidden_namespace(key: &[u8]) -> Option<u8> {
    match system_namespace_id(key)? {
        SYSTEM_ID_PROJECTION => None,
        id => Some(id),
    }
}

/// A deterministic builder for one `ScopeSnapshot`.
///
/// The builder keeps one `BTreeMap` per shard. Insert order cannot change the
/// output bytes. A duplicate key is a hard error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeProjectionBuilder<A> {
    params: ScopeSnapshotParams,
    admission: A,
    shards: BTreeMap<ShardId, ScopeShardBuilder>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopeShardBuilder {
    commit_version: CommitVersion,
    records: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl<A: ScopeAdmission> ScopeProjectionBuilder<A> {
    /// Creates a builder for one scope.
    #[must_use]
    pub const fn new(params: ScopeSnapshotParams, admission: A) -> Self {
        Self {
            params,
            admission,
            shards: BTreeMap::new(),
        }
    }

    /// Adds one admitted user row.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::ForbiddenNamespace`] for receipt, event, audit,
    /// lease, index, statistics, or unknown system data. Returns
    /// [`SnapshotError::SystemKeyNotAllowed`] for a projection key. Use
    /// [`ScopeProjectionBuilder::insert_projection`] for a projection fact.
    /// Returns [`SnapshotError::OutOfScopeRecord`] when the scope does not
    /// admit the row. Returns [`SnapshotError::CorruptPage`] when the shard is
    /// outside the version vector or uses a different commit version. Returns
    /// [`SnapshotError::DuplicateKey`] for a duplicate key. Returns
    /// [`SnapshotError::PageTooLarge`] for a length over its limit.
    pub fn insert(
        &mut self,
        shard_id: ShardId,
        commit_version: CommitVersion,
        record: Record,
    ) -> Result<(), SnapshotError> {
        if let Some(id) = forbidden_namespace(&record.key) {
            return Err(SnapshotError::ForbiddenNamespace { id });
        }
        if let Some(id) = system_namespace_id(&record.key) {
            return Err(SnapshotError::SystemKeyNotAllowed { id });
        }
        if !self.admission.admits(&record.key, &record.value) {
            return Err(SnapshotError::OutOfScopeRecord { key: record.key });
        }
        self.insert_record(shard_id, commit_version, record)
    }

    /// Adds one admitted projection fact.
    ///
    /// The method is the only path for the projection namespace.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::ForbiddenNamespace`] for receipt, event, audit,
    /// lease, index, statistics, or unknown system data. Returns
    /// [`SnapshotError::NotProjectionFact`] when the key is not a projection
    /// fact. Returns [`SnapshotError::OutOfScopeRecord`] when the scope does
    /// not need the fact. Returns the same shard, duplicate, and size errors as
    /// [`ScopeProjectionBuilder::insert`].
    pub fn insert_projection(
        &mut self,
        shard_id: ShardId,
        commit_version: CommitVersion,
        record: Record,
    ) -> Result<(), SnapshotError> {
        if let Some(id) = forbidden_namespace(&record.key) {
            return Err(SnapshotError::ForbiddenNamespace { id });
        }
        if system_namespace_id(&record.key) != Some(SYSTEM_ID_PROJECTION) {
            return Err(SnapshotError::NotProjectionFact { key: record.key });
        }
        if !self.admission.admits(&record.key, &record.value) {
            return Err(SnapshotError::OutOfScopeRecord { key: record.key });
        }
        self.insert_record(shard_id, commit_version, record)
    }

    /// Adds many admitted user rows.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`ScopeProjectionBuilder::insert`].
    pub fn extend(
        &mut self,
        records: impl IntoIterator<Item = (ShardId, CommitVersion, Record)>,
    ) -> Result<(), SnapshotError> {
        for (shard_id, commit_version, record) in records {
            self.insert(shard_id, commit_version, record)?;
        }
        Ok(())
    }

    /// Adds many admitted projection facts.
    ///
    /// # Errors
    ///
    /// Returns the same errors as
    /// [`ScopeProjectionBuilder::insert_projection`].
    pub fn extend_projection(
        &mut self,
        records: impl IntoIterator<Item = (ShardId, CommitVersion, Record)>,
    ) -> Result<(), SnapshotError> {
        for (shard_id, commit_version, record) in records {
            self.insert_projection(shard_id, commit_version, record)?;
        }
        Ok(())
    }

    /// Builds the `ScopeSnapshot` bytes.
    ///
    /// # Errors
    ///
    /// Returns a named [`SnapshotError`] from the snapshot builder.
    pub fn build(self) -> Result<Vec<u8>, SnapshotError> {
        let shards: Vec<ScopeShard> = self
            .shards
            .into_iter()
            .map(|(shard_id, shard)| ScopeShard {
                shard_id,
                commit_version: shard.commit_version,
                records: shard
                    .records
                    .into_iter()
                    .map(|(key, value)| Record::new(key, value))
                    .collect(),
            })
            .collect();
        ScopeSnapshot::build(&self.params, &shards)
    }

    fn insert_record(
        &mut self,
        shard_id: ShardId,
        commit_version: CommitVersion,
        record: Record,
    ) -> Result<(), SnapshotError> {
        if record.key.len() > MAX_KEY_BYTES || record.value.len() > MAX_VALUE_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        match self.params.version_vector.get(&shard_id) {
            Some(expected) if expected == commit_version => {}
            _ => {
                return Err(SnapshotError::CorruptPage {
                    page_id: PageId::from_bytes([0; 32]),
                    offset: 0,
                    reason: "the scope shard is not in the version vector",
                });
            }
        }
        let shard = self
            .shards
            .entry(shard_id)
            .or_insert_with(|| ScopeShardBuilder {
                commit_version,
                records: BTreeMap::new(),
            });
        if shard.commit_version != commit_version {
            return Err(SnapshotError::CorruptPage {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
                reason: "the scope shard uses two commit versions",
            });
        }
        if shard.records.insert(record.key, record.value).is_some() {
            return Err(SnapshotError::DuplicateKey { offset: 0 });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::keyspace::SYSTEM_ID_RECEIPT;

    const RECEIPT_KEY: &[u8] = b"\x01accounts\x00\xff\x01\x10receipt\x00\x00";
    const PROJECTION_KEY: &[u8] = b"\x01accounts\x00\xff\x04\x10fact\x00\x00";
    const USER_KEY: &[u8] = b"\x01accounts\x00\x10alice\x00\x00";

    #[test]
    fn a_real_receipt_key_is_forbidden() {
        assert_eq!(system_namespace_id(RECEIPT_KEY), Some(SYSTEM_ID_RECEIPT));
        assert_eq!(forbidden_namespace(RECEIPT_KEY), Some(SYSTEM_ID_RECEIPT));
    }

    #[test]
    fn a_real_projection_key_is_allowed() {
        assert_eq!(
            system_namespace_id(PROJECTION_KEY),
            Some(SYSTEM_ID_PROJECTION)
        );
        assert_eq!(forbidden_namespace(PROJECTION_KEY), None);
    }

    #[test]
    fn a_user_key_is_not_a_system_key() {
        assert_eq!(system_namespace_id(USER_KEY), None);
        assert_eq!(forbidden_namespace(USER_KEY), None);
    }

    #[test]
    fn an_unknown_system_key_is_forbidden() {
        let unknown = b"\x01accounts\x00\xff\x08\x10data\x00\x00";
        assert_eq!(forbidden_namespace(unknown), Some(0x08));
    }
}
