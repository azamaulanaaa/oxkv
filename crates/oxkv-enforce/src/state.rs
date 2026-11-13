//! In-memory shard state for the commit model.
//!
//! The enforcement crate owns no durable storage. This module models the data,
//! index, and business events that one shard transaction writes.

use std::collections::BTreeMap;

use oxkv_contracts::version::{CommitVersion, Hash32, KeyVersion, PolicyVersion, ShardId};
use oxkv_event::{EmissionIndex, EventId, RuleId, compute_event_id};

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
///
/// The event carries the envelope identity fields. The event id is a
/// deterministic hash of the shard, the key, the commit version, the rule
/// identity, and the emission index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessEvent {
    /// The deterministic event identity.
    pub event_id: EventId,
    /// The shard that owns the commit.
    pub shard_id: ShardId,
    /// The row key.
    pub key: RowKey,
    /// The digest of the row value.
    pub value_digest: Hash32,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The manifest rule identity.
    pub rule_id: RuleId,
    /// The pinned policy version.
    pub policy_version: PolicyVersion,
    /// The emission index inside one commit.
    pub emission_index: EmissionIndex,
}

impl BusinessEvent {
    /// Creates one business event and computes its event id.
    #[must_use]
    pub fn new(
        shard_id: ShardId,
        key: RowKey,
        value_digest: Hash32,
        commit_version: CommitVersion,
        rule_id: RuleId,
        policy_version: PolicyVersion,
        emission_index: EmissionIndex,
    ) -> Self {
        let event_id = compute_event_id(
            &shard_id,
            key.as_bytes(),
            commit_version,
            &rule_id,
            emission_index,
        );
        Self {
            event_id,
            shard_id,
            key,
            value_digest,
            commit_version,
            rule_id,
            policy_version,
            emission_index,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> BusinessEvent {
        BusinessEvent::new(
            ShardId::new(vec![1]),
            RowKey::new(b"row:1".to_vec()),
            Hash32::of(b"value"),
            CommitVersion::new(4),
            RuleId::new("rule.payload"),
            PolicyVersion::from_bytes([4; 32]),
            EmissionIndex::new(0),
        )
    }

    #[test]
    fn a_business_event_id_is_stable() {
        assert_eq!(event().event_id, event().event_id);
    }

    #[test]
    fn a_business_event_carries_the_envelope_fields() {
        let event = event();
        assert_eq!(event.shard_id, ShardId::new(vec![1]));
        assert_eq!(event.key.as_bytes(), b"row:1");
        assert_eq!(event.commit_version, CommitVersion::new(4));
        assert_eq!(event.rule_id.as_str(), "rule.payload");
        assert_eq!(event.policy_version, PolicyVersion::from_bytes([4; 32]));
        assert_eq!(event.emission_index, EmissionIndex::new(0));
    }

    #[test]
    fn a_business_event_id_covers_each_identity_field() {
        let base = event();
        let changed_shard = BusinessEvent::new(
            ShardId::new(vec![2]),
            base.key.clone(),
            base.value_digest,
            base.commit_version,
            base.rule_id.clone(),
            base.policy_version,
            base.emission_index,
        );
        let changed_key = BusinessEvent::new(
            base.shard_id.clone(),
            RowKey::new(b"row:2".to_vec()),
            base.value_digest,
            base.commit_version,
            base.rule_id.clone(),
            base.policy_version,
            base.emission_index,
        );
        let changed_commit = BusinessEvent::new(
            base.shard_id.clone(),
            base.key.clone(),
            base.value_digest,
            CommitVersion::new(5),
            base.rule_id.clone(),
            base.policy_version,
            base.emission_index,
        );
        let changed_rule = BusinessEvent::new(
            base.shard_id.clone(),
            base.key.clone(),
            base.value_digest,
            base.commit_version,
            RuleId::new("rule.other"),
            base.policy_version,
            base.emission_index,
        );
        let changed_index = BusinessEvent::new(
            base.shard_id.clone(),
            base.key.clone(),
            base.value_digest,
            base.commit_version,
            base.rule_id.clone(),
            base.policy_version,
            EmissionIndex::new(1),
        );
        assert_ne!(base.event_id, changed_shard.event_id);
        assert_ne!(base.event_id, changed_key.event_id);
        assert_ne!(base.event_id, changed_commit.event_id);
        assert_ne!(base.event_id, changed_rule.event_id);
        assert_ne!(base.event_id, changed_index.event_id);
    }
}
