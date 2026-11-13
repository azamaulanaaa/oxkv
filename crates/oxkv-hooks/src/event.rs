//! Ordered notifications from post-write derivation.

use core::fmt;

use oxkv_contracts::version::{CommitVersion, DigestDomain, Hash32, ShardId};

use crate::batch::Key;
use crate::rule::RuleId;

/// The manifest-declared kind of one notification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventKind(String);

impl EventKind {
    /// Creates an event kind.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// Returns the event kind text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns true when the event kind is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The part of one notification that a post-write hook selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventDraft {
    kind: EventKind,
    payload: Vec<u8>,
}

impl EventDraft {
    /// Creates an event draft.
    #[must_use]
    pub fn new(kind: EventKind, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind,
            payload: payload.into(),
        }
    }

    /// Returns the declared event kind.
    #[must_use]
    pub const fn kind(&self) -> &EventKind {
        &self.kind
    }

    /// Returns the bounded payload bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// The stable identity of one notification.
///
/// The identity derives from the shard, the key, the commit version, the rule
/// id, and the emission index. The derivation uses the event identity domain
/// of [`oxkv_contracts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventId(Hash32);

impl EventId {
    /// Derives one event identity.
    #[must_use]
    pub fn derive(
        shard_id: &ShardId,
        key: &Key,
        commit_version: CommitVersion,
        rule_id: &RuleId,
        emission_index: u32,
    ) -> Self {
        let mut bytes = Vec::new();
        push_len(&mut bytes, shard_id.as_bytes().len());
        bytes.extend_from_slice(shard_id.as_bytes());
        push_len(&mut bytes, key.len());
        bytes.extend_from_slice(key.as_bytes());
        bytes.extend_from_slice(&commit_version.get().to_be_bytes());
        push_len(&mut bytes, rule_id.as_str().len());
        bytes.extend_from_slice(rule_id.as_str().as_bytes());
        bytes.extend_from_slice(&emission_index.to_be_bytes());
        Self(Hash32::of_domain(DigestDomain::EventId, &bytes))
    }

    /// Returns the raw identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Returns the lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.to_hex())
    }
}

/// One ordered notification from the commit step.
///
/// The notification carries the stable event identity, the commit position,
/// the key, the declared kind, the rule id, the emission index, the batch
/// position, and a bounded payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    event_id: EventId,
    shard_id: ShardId,
    commit_version: CommitVersion,
    key: Key,
    kind: EventKind,
    rule_id: RuleId,
    emission_index: u32,
    op_index: u32,
    payload: Vec<u8>,
}

/// The fields that the pipeline supplies to build one notification.
pub(crate) struct NotificationSeed {
    pub(crate) shard_id: ShardId,
    pub(crate) commit_version: CommitVersion,
    pub(crate) key: Key,
    pub(crate) kind: EventKind,
    pub(crate) rule_id: RuleId,
    pub(crate) emission_index: u32,
    pub(crate) op_index: u32,
    pub(crate) payload: Vec<u8>,
}

impl Notification {
    pub(crate) fn new(seed: NotificationSeed) -> Self {
        let event_id = EventId::derive(
            &seed.shard_id,
            &seed.key,
            seed.commit_version,
            &seed.rule_id,
            seed.emission_index,
        );
        Self {
            event_id,
            shard_id: seed.shard_id,
            commit_version: seed.commit_version,
            key: seed.key,
            kind: seed.kind,
            rule_id: seed.rule_id,
            emission_index: seed.emission_index,
            op_index: seed.op_index,
            payload: seed.payload,
        }
    }

    /// Returns the stable event identity.
    #[must_use]
    pub const fn event_id(&self) -> EventId {
        self.event_id
    }

    /// Returns the shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the notification key.
    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the declared event kind.
    #[must_use]
    pub const fn kind(&self) -> &EventKind {
        &self.kind
    }

    /// Returns the rule id that emitted the notification.
    #[must_use]
    pub const fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    /// Returns the emission index in commit order.
    #[must_use]
    pub const fn emission_index(&self) -> u32 {
        self.emission_index
    }

    /// Returns the batch position that produced the notification.
    #[must_use]
    pub const fn op_index(&self) -> u32 {
        self.op_index
    }

    /// Returns the notification payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Encodes the notification as canonical bytes.
    ///
    /// The encoding uses one version byte and fixed-width big-endian lengths.
    /// It contains no platform-dependent field.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(1);
        bytes.extend_from_slice(self.event_id.as_bytes());
        push_len(&mut bytes, self.shard_id.as_bytes().len());
        bytes.extend_from_slice(self.shard_id.as_bytes());
        bytes.extend_from_slice(&self.commit_version.get().to_be_bytes());
        push_len(&mut bytes, self.key.len());
        bytes.extend_from_slice(self.key.as_bytes());
        push_len(&mut bytes, self.kind.as_str().len());
        bytes.extend_from_slice(self.kind.as_str().as_bytes());
        push_len(&mut bytes, self.rule_id.as_str().len());
        bytes.extend_from_slice(self.rule_id.as_str().as_bytes());
        bytes.extend_from_slice(&self.emission_index.to_be_bytes());
        bytes.extend_from_slice(&self.op_index.to_be_bytes());
        push_len(&mut bytes, self.payload.len());
        bytes.extend_from_slice(&self.payload);
        bytes
    }
}

fn push_len(bytes: &mut Vec<u8>, len: usize) {
    bytes.extend_from_slice(&u64::try_from(len).unwrap_or(u64::MAX).to_be_bytes());
}
