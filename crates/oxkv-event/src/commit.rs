//! Authority-only business events.
//!
//! A business event commits with its write or not at all. A rejected write and
//! a conflicting write emit no business event. Only the authority creates a
//! [`CommittedWrite`], and only a committed write can produce a
//! [`BusinessEvent`].

use oxkv_contracts::cursor::EventSequence;
use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::version::{CommitVersion, KeyVersion, PolicyVersion, ShardId};

use crate::envelope::{
    DEFAULT_PAYLOAD_BOUND, EventEnvelope, EventEnvelopeParts, EventPayload, PayloadBound,
};
use crate::error::EventError;
use crate::id::{
    CommittedAt, EffectField, EmissionIndex, EventId, OriginRequestId, PayloadSchema, RuleId,
};
use crate::retention::RetentionClass;

/// The context of one committed write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedWrite {
    shard_id: ShardId,
    table: TableName,
    key: Vec<u8>,
    commit_version: CommitVersion,
    committed_at: CommittedAt,
    policy_version: PolicyVersion,
    origin_request_id: OriginRequestId,
}

impl CommittedWrite {
    /// Creates the context of one committed write.
    ///
    /// The authority calls this constructor after the write commits. A
    /// rejected write and a conflicting write must not create this value.
    #[must_use]
    pub fn new(
        shard_id: ShardId,
        table: TableName,
        key: Vec<u8>,
        commit_version: CommitVersion,
        committed_at: CommittedAt,
        policy_version: PolicyVersion,
        origin_request_id: OriginRequestId,
    ) -> Self {
        Self {
            shard_id,
            table,
            key,
            commit_version,
            committed_at,
            policy_version,
            origin_request_id,
        }
    }

    /// Returns the shard id.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the user table name.
    #[must_use]
    pub const fn table(&self) -> &TableName {
        &self.table
    }

    /// Returns the canonical key bytes.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Returns the shard commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the injected commit timestamp.
    #[must_use]
    pub const fn committed_at(&self) -> CommittedAt {
        self.committed_at
    }

    /// Returns the pinned policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the opaque origin request identity.
    #[must_use]
    pub const fn origin_request_id(&self) -> &OriginRequestId {
        &self.origin_request_id
    }
}

/// The terminal outcome of one write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The write is durable.
    Committed(CommittedWrite),
    /// The authority refused the write.
    Rejected {
        /// The canonical reason code.
        reason: &'static str,
    },
    /// The base version did not match the current key version.
    Conflict {
        /// The current key version.
        current_key_version: KeyVersion,
    },
}

impl WriteOutcome {
    /// Returns the committed write context.
    #[must_use]
    pub const fn committed(&self) -> Option<&CommittedWrite> {
        match self {
            Self::Committed(write) => Some(write),
            Self::Rejected { .. } | Self::Conflict { .. } => None,
        }
    }

    /// Returns true when the write is committed.
    #[must_use]
    pub const fn is_committed(&self) -> bool {
        self.committed().is_some()
    }
}

/// One business event draft for a committed write.
///
/// The draft holds the rule-specific values. The commit holds the shared
/// values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventDraft {
    key_sequence: EventSequence,
    rule_id: RuleId,
    payload: EventPayload,
    payload_schema: PayloadSchema,
    required_effects: Vec<EffectField>,
    retention: RetentionClass,
    payload_bound: PayloadBound,
}

impl EventDraft {
    /// Creates an empty event draft.
    #[must_use]
    pub fn new(
        key_sequence: EventSequence,
        rule_id: RuleId,
        payload_schema: PayloadSchema,
    ) -> Self {
        Self {
            key_sequence,
            rule_id,
            payload: EventPayload::new(),
            payload_schema,
            required_effects: Vec::new(),
            retention: RetentionClass::time(86_400),
            payload_bound: DEFAULT_PAYLOAD_BOUND,
        }
    }

    /// Sets the payload.
    #[must_use]
    pub fn with_payload(mut self, payload: EventPayload) -> Self {
        self.payload = payload;
        self
    }

    /// Sets the required effect fields.
    #[must_use]
    pub fn with_required_effects(mut self, required_effects: Vec<EffectField>) -> Self {
        self.required_effects = required_effects;
        self
    }

    /// Sets the retention class.
    #[must_use]
    pub const fn with_retention(mut self, retention: RetentionClass) -> Self {
        self.retention = retention;
        self
    }

    /// Sets the payload bound.
    #[must_use]
    pub const fn with_payload_bound(mut self, payload_bound: PayloadBound) -> Self {
        self.payload_bound = payload_bound;
        self
    }

    /// Returns the per-key event sequence.
    #[must_use]
    pub const fn key_sequence(&self) -> EventSequence {
        self.key_sequence
    }

    /// Returns the rule identity.
    #[must_use]
    pub const fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    /// Returns the payload.
    #[must_use]
    pub const fn payload(&self) -> &EventPayload {
        &self.payload
    }

    /// Returns the payload schema.
    #[must_use]
    pub const fn payload_schema(&self) -> &PayloadSchema {
        &self.payload_schema
    }

    /// Returns the required effect fields.
    #[must_use]
    pub fn required_effects(&self) -> &[EffectField] {
        &self.required_effects
    }

    /// Returns the retention class.
    #[must_use]
    pub const fn retention(&self) -> RetentionClass {
        self.retention
    }

    /// Returns the payload bound.
    #[must_use]
    pub const fn payload_bound(&self) -> PayloadBound {
        self.payload_bound
    }
}

/// One authority-only business event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessEvent {
    envelope: EventEnvelope,
}

impl BusinessEvent {
    /// Builds one business event from a committed write and one draft.
    ///
    /// Only the authority has a [`CommittedWrite`]. A rejected write and a
    /// conflicting write have no committed write, so they emit no event.
    ///
    /// # Errors
    ///
    /// Returns any [`EventError`] from [`EventEnvelope::new`].
    pub fn from_commit(
        commit: &CommittedWrite,
        draft: &EventDraft,
        emission_index: EmissionIndex,
    ) -> Result<Self, EventError> {
        let envelope = EventEnvelope::new(EventEnvelopeParts {
            shard_id: commit.shard_id.clone(),
            table: commit.table.clone(),
            key: commit.key.clone(),
            key_sequence: draft.key_sequence,
            commit_version: commit.commit_version,
            committed_at: commit.committed_at,
            rule_id: draft.rule_id.clone(),
            policy_version: commit.policy_version,
            emission_index,
            payload: draft.payload.clone(),
            payload_schema: draft.payload_schema.clone(),
            origin_request_id: commit.origin_request_id.clone(),
            required_effects: draft.required_effects.clone(),
            retention: draft.retention,
            payload_bound: draft.payload_bound,
        })?;
        Ok(Self { envelope })
    }

    /// Returns the envelope.
    #[must_use]
    pub const fn envelope(&self) -> &EventEnvelope {
        &self.envelope
    }

    /// Returns the event id.
    #[must_use]
    pub const fn event_id(&self) -> EventId {
        self.envelope.event_id()
    }

    /// Encodes the envelope in the canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.envelope.encode()
    }
}

/// Builds the business events for one write outcome.
///
/// A rejected write and a conflicting write return an empty list. A committed
/// write returns one event for each draft in order. The list index is the
/// emission index.
///
/// # Errors
///
/// Returns any [`EventError`] from [`BusinessEvent::from_commit`].
pub fn events_for_write(
    outcome: &WriteOutcome,
    drafts: &[EventDraft],
) -> Result<Vec<BusinessEvent>, EventError> {
    let Some(commit) = outcome.committed() else {
        return Ok(Vec::new());
    };
    let mut events = Vec::with_capacity(drafts.len());
    for (index, draft) in drafts.iter().enumerate() {
        let emission_index = EmissionIndex::new(u64::try_from(index).unwrap_or(u64::MAX));
        events.push(BusinessEvent::from_commit(commit, draft, emission_index)?);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::PayloadValue;

    fn commit() -> CommittedWrite {
        CommittedWrite::new(
            ShardId::new(vec![1, 2]),
            TableName::parse_user("accounts").expect("the table name is valid"),
            b"accounts/42".to_vec(),
            CommitVersion::new(18),
            CommittedAt::new(1_760_000_000),
            PolicyVersion::from_bytes([4; 32]),
            OriginRequestId::new(vec![9, 9]),
        )
    }

    fn draft() -> EventDraft {
        EventDraft::new(
            EventSequence::new(1),
            RuleId::new("welcome_mail"),
            PayloadSchema::new("welcome_mail/v1"),
        )
        .with_payload(
            EventPayload::new()
                .with_field("recipient", PayloadValue::Text("a@example.com".to_owned())),
        )
        .with_required_effects(vec![EffectField::new("recipient")])
    }

    #[test]
    fn one_committed_write_emits_one_event_with_a_stable_id() {
        let outcome = WriteOutcome::Committed(commit());
        let first = events_for_write(&outcome, &[draft()]).expect("the event is valid");
        let second = events_for_write(&outcome, &[draft()]).expect("the event is valid");
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].event_id(), second[0].event_id());
        assert_eq!(first[0].encode(), second[0].encode());
    }

    #[test]
    fn a_rejected_write_emits_no_event() {
        let outcome = WriteOutcome::Rejected {
            reason: "write.stale_base_version",
        };
        let events = events_for_write(&outcome, &[draft()]).expect("the call succeeds");
        assert_eq!(events, Vec::<BusinessEvent>::new());
    }

    #[test]
    fn a_conflict_write_emits_no_event() {
        let outcome = WriteOutcome::Conflict {
            current_key_version: KeyVersion::new(7),
        };
        let events = events_for_write(&outcome, &[draft()]).expect("the call succeeds");
        assert_eq!(events, Vec::<BusinessEvent>::new());
    }

    #[test]
    fn the_emission_index_follows_the_draft_order() {
        let outcome = WriteOutcome::Committed(commit());
        let events = events_for_write(&outcome, &[draft(), draft()]).expect("the events are valid");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].envelope().emission_index(), EmissionIndex::new(0));
        assert_eq!(events[1].envelope().emission_index(), EmissionIndex::new(1));
        assert_ne!(events[0].event_id(), events[1].event_id());
    }

    #[test]
    fn the_commit_carries_the_origin_request_id() {
        let outcome = WriteOutcome::Committed(commit());
        let events = events_for_write(&outcome, &[draft()]).expect("the event is valid");
        assert_eq!(events[0].envelope().origin_request_id().as_bytes(), &[9, 9]);
    }
}
