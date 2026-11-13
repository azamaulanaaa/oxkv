//! Coverage descriptors for opt-in coverage results.
//!
//! A coverage descriptor records the shards that answered a search, the shards
//! that did not answer, and the commit version that each answering shard
//! served. A caller can detect missing data from the descriptor. A caller
//! cannot infer completeness from a row count.

use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId, VersionVector};

/// One answering shard and the commit version that it served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardServing {
    shard_id: ShardId,
    served_version: CommitVersion,
}

impl ShardServing {
    /// Creates one answering shard record.
    #[must_use]
    pub const fn new(shard_id: ShardId, served_version: CommitVersion) -> Self {
        Self {
            shard_id,
            served_version,
        }
    }

    /// Returns the shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the commit version that the shard served.
    #[must_use]
    pub const fn served_version(&self) -> CommitVersion {
        self.served_version
    }
}

/// A record of answered shards, missing shards, and served versions.
///
/// The constructor sorts both shard lists by shard identifier. Two descriptors
/// with the same entries therefore have the same order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageDescriptor {
    answered: Vec<ShardServing>,
    missing: Vec<ShardId>,
}

impl CoverageDescriptor {
    /// Creates a coverage descriptor.
    #[must_use]
    pub fn new(mut answered: Vec<ShardServing>, mut missing: Vec<ShardId>) -> Self {
        answered.sort_by(|left, right| left.shard_id.cmp(&right.shard_id));
        answered.dedup_by(|left, right| left.shard_id == right.shard_id);
        missing.sort();
        missing.dedup();
        Self { answered, missing }
    }

    /// Returns the answering shards in shard identifier order.
    #[must_use]
    pub fn answered(&self) -> &[ShardServing] {
        &self.answered
    }

    /// Returns the missing shards in shard identifier order.
    #[must_use]
    pub fn missing_shards(&self) -> &[ShardId] {
        &self.missing
    }

    /// Iterates over the answered shard identifiers.
    pub fn answered_shards(&self) -> impl Iterator<Item = &ShardId> {
        self.answered.iter().map(ShardServing::shard_id)
    }

    /// Returns the number of answered shards.
    #[must_use]
    pub fn answered_count(&self) -> usize {
        self.answered.len()
    }

    /// Returns the number of missing shards.
    #[must_use]
    pub fn missing_count(&self) -> usize {
        self.missing.len()
    }

    /// Returns true when no shard is missing.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    /// Returns true when at least one shard is missing.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        !self.is_complete()
    }

    /// Builds the served version vector from the answering shards.
    #[must_use]
    pub fn served_versions(&self) -> ScopeVersionVector {
        let entries = self
            .answered
            .iter()
            .map(|serving| (serving.shard_id.clone(), serving.served_version));
        ScopeVersionVector::from_vector(VersionVector::from_entries(entries))
    }
}
