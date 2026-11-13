//! Deterministic `ScopeSnapshot` projection.

use std::collections::BTreeMap;

use oxkv_contracts::keyspace::{
    SYSTEM_ID_AUDIT, SYSTEM_ID_EVENT, SYSTEM_ID_LEASE, SYSTEM_ID_RECEIPT, SYSTEM_MARKER,
};
use oxkv_contracts::version::{CommitVersion, PageId, ShardId};

use crate::error::SnapshotError;
use crate::page::Record;
use crate::snapshot::{ScopeShard, ScopeSnapshot, ScopeSnapshotParams};
use crate::{MAX_KEY_BYTES, MAX_VALUE_BYTES};

/// Returns the forbidden system namespace of a scope key.
///
/// A scope snapshot must not carry receipt, event, audit, or lease data.
#[must_use]
pub fn forbidden_namespace(key: &[u8]) -> Option<u8> {
    if key.first().copied() != Some(SYSTEM_MARKER) || key.len() < 2 {
        return None;
    }
    let id = key[1];
    if matches!(
        id,
        SYSTEM_ID_RECEIPT | SYSTEM_ID_EVENT | SYSTEM_ID_AUDIT | SYSTEM_ID_LEASE
    ) {
        Some(id)
    } else {
        None
    }
}

/// A deterministic builder for one `ScopeSnapshot`.
///
/// The builder keeps one `BTreeMap` per shard. Insert order cannot change the
/// output bytes. A duplicate key is a hard error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeProjectionBuilder {
    params: ScopeSnapshotParams,
    shards: BTreeMap<ShardId, ScopeShardBuilder>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopeShardBuilder {
    commit_version: CommitVersion,
    records: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl ScopeProjectionBuilder {
    /// Creates a builder for one scope.
    #[must_use]
    pub const fn new(params: ScopeSnapshotParams) -> Self {
        Self {
            params,
            shards: BTreeMap::new(),
        }
    }

    /// Adds one admitted user row or needed projection fact.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::ForbiddenNamespace`] for receipt, event, audit,
    /// or lease data. Returns [`SnapshotError::CorruptPage`] when the shard is
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

    /// Adds many admitted rows or needed projection facts.
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
}
