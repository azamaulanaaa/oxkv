//! The consumer-side business event envelope.
//!
//! The envelope mirrors the annex that essay 24 defines. The consumer reads
//! it and never writes it. The type carries the fields from section 9.9 of the
//! 1.0 specification:
//!
//! - `envelope_version`
//! - `event_id`
//! - `shard_id`
//! - `table`
//! - `key`
//! - `key_sequence`
//! - `commit_version`
//! - `committed_at`
//! - `rule_id`
//! - `policy_version`
//! - `emission_index`
//! - `payload`
//! - `payload_schema`
//! - `origin_request_id`
//!
//! The envelope carries `origin_request_id`, not the raw idempotency key.
//! `committed_at` is an injected commit parameter. The event id derives from
//! the shard, the key, the commit version, the rule identity, and the emission
//! index.
//!
//! [`EffectPayload`] holds every required effect field. An optional enrichment
//! object can travel by reference. The consumer never reads that object.

use core::fmt;

use oxkv_contracts::cursor::EventSequence;
use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::receipt::RequestId;
use oxkv_contracts::version::{
    CommitVersion, ContentDigest, DigestDomain, EventVersion, Hash32, PolicyVersion, ShardId,
};

/// The supported event envelope version.
pub const EFFECT_ENVELOPE_VERSION: EventVersion = EventVersion::new(1);

/// The payload schema of one effect request.
pub const EFFECT_PAYLOAD_SCHEMA: &str = "oxkv/effect/v1";

/// One deterministic business event identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventId(Hash32);

impl EventId {
    /// Creates an event identity from raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Hash32::from_bytes(bytes))
    }

    /// Derives the event identity from the section 8.8 inputs.
    ///
    /// The inputs are the shard, the key, the commit version, the rule
    /// identity, and the emission index. The function uses `BLAKE3` with the
    /// event id domain tag.
    #[must_use]
    pub fn derive(
        shard_id: &ShardId,
        key: &[u8],
        commit_version: CommitVersion,
        rule_id: &str,
        emission_index: u32,
    ) -> Self {
        let mut input = Vec::new();
        push_bytes(&mut input, shard_id.as_bytes());
        push_bytes(&mut input, key);
        input.extend_from_slice(&commit_version.get().to_be_bytes());
        push_bytes(&mut input, rule_id.as_bytes());
        input.extend_from_slice(&emission_index.to_be_bytes());
        Self(Hash32::of_domain(DigestDomain::EventId, &input))
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

impl fmt::Debug for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EventId")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
}

/// A reference to one immutable large object.
///
/// The reference names the object with a digest and a version. The consumer
/// treats the object as optional enrichment. It never reads the object on the
/// effect path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectReference {
    /// The content digest of the immutable object.
    pub digest: ContentDigest,
    /// The object version.
    pub version: u64,
}

impl ObjectReference {
    /// Creates an object reference.
    #[must_use]
    pub const fn new(digest: ContentDigest, version: u64) -> Self {
        Self { digest, version }
    }
}

/// Every required effect field and one optional enrichment reference.
///
/// The fields `recipient`, `subject`, and `body` are required. The field
/// `enrichment` is optional. The `Debug` form redacts every text field, so a
/// payload does not enter a log through a derived formatter.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EffectPayload {
    /// The effect recipient.
    pub recipient: String,
    /// The effect subject.
    pub subject: String,
    /// The effect body.
    pub body: String,
    /// An optional large object. The effect does not depend on it.
    pub enrichment: Option<ObjectReference>,
}

impl EffectPayload {
    /// Creates an effect payload with no enrichment reference.
    #[must_use]
    pub fn new(recipient: String, subject: String, body: String) -> Self {
        Self {
            recipient,
            subject,
            body,
            enrichment: None,
        }
    }

    /// Adds an optional enrichment reference.
    #[must_use]
    pub fn with_enrichment(mut self, reference: ObjectReference) -> Self {
        self.enrichment = Some(reference);
        self
    }

    /// Checks that every required field is present.
    ///
    /// # Errors
    ///
    /// Returns [`EffectPayloadError`] when one required field is empty.
    pub fn validate(&self) -> Result<(), EffectPayloadError> {
        if self.recipient.is_empty() {
            return Err(EffectPayloadError::Recipient);
        }
        if self.subject.is_empty() {
            return Err(EffectPayloadError::Subject);
        }
        if self.body.is_empty() {
            return Err(EffectPayloadError::Body);
        }
        Ok(())
    }
}

impl fmt::Debug for EffectPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectPayload")
            .field("recipient", &"<redacted>")
            .field("subject", &"<redacted>")
            .field("body", &"<redacted>")
            .field("enrichment", &self.enrichment)
            .finish()
    }
}

/// One effect request that the consumer builds from the event payload.
///
/// The consumer copies every field from [`EffectPayload`]. The effect holds no
/// authority handle. The `Debug` form redacts every text field.
#[derive(Clone, PartialEq, Eq)]
pub struct Effect {
    /// The effect recipient.
    pub recipient: String,
    /// The effect subject.
    pub subject: String,
    /// The effect body.
    pub body: String,
    /// An optional large object. The effect does not depend on it.
    pub enrichment: Option<ObjectReference>,
}

impl Effect {
    /// Builds one effect from an event payload.
    ///
    /// # Errors
    ///
    /// Returns [`EffectPayloadError`] when one required field is empty.
    pub fn from_payload(payload: &EffectPayload) -> Result<Self, EffectPayloadError> {
        payload.validate()?;
        Ok(Self {
            recipient: payload.recipient.clone(),
            subject: payload.subject.clone(),
            body: payload.body.clone(),
            enrichment: payload.enrichment.clone(),
        })
    }
}

impl fmt::Debug for Effect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Effect")
            .field("recipient", &"<redacted>")
            .field("subject", &"<redacted>")
            .field("body", &"<redacted>")
            .field("enrichment", &self.enrichment)
            .finish()
    }
}

/// A missing required effect field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectPayloadError {
    /// The recipient is empty.
    #[error("The payload has no recipient.")]
    Recipient,
    /// The subject is empty.
    #[error("The payload has no subject.")]
    Subject,
    /// The body is empty.
    #[error("The payload has no body.")]
    Body,
}

impl EffectPayloadError {
    /// Returns the missing field name.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::Recipient => "recipient",
            Self::Subject => "subject",
            Self::Body => "body",
        }
    }
}

/// One business event that a consumer can deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEnvelope {
    /// The envelope version.
    pub envelope_version: EventVersion,
    /// The deterministic event identity.
    pub event_id: EventId,
    /// The shard that committed the event.
    pub shard_id: ShardId,
    /// The user table of the committed key.
    pub table: TableName,
    /// The committed key.
    pub key: Vec<u8>,
    /// The per-key event sequence.
    pub key_sequence: EventSequence,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The injected commit time in milliseconds.
    pub committed_at: u64,
    /// The rule identity that emitted the event.
    pub rule_id: String,
    /// The pinned policy version.
    pub policy_version: PolicyVersion,
    /// The emission index inside one commit.
    pub emission_index: u32,
    /// The effect payload.
    pub payload: EffectPayload,
    /// The effect payload schema.
    pub payload_schema: String,
    /// The origin request identity. It is not the raw idempotency key.
    pub origin_request_id: RequestId,
}

impl EventEnvelope {
    /// Creates an envelope with the supported version and default metadata.
    ///
    /// The caller sets the commit metadata and the payload schema on the
    /// returned value. The function keeps the constructor small.
    #[must_use]
    pub fn new(
        event_id: EventId,
        shard_id: ShardId,
        table: TableName,
        key: Vec<u8>,
        key_sequence: EventSequence,
        payload: EffectPayload,
    ) -> Self {
        Self {
            envelope_version: EFFECT_ENVELOPE_VERSION,
            event_id,
            shard_id,
            table,
            key,
            key_sequence,
            commit_version: CommitVersion::new(0),
            committed_at: 0,
            rule_id: String::new(),
            policy_version: PolicyVersion::from_bytes([0; 32]),
            emission_index: 0,
            payload,
            payload_schema: EFFECT_PAYLOAD_SCHEMA.to_owned(),
            origin_request_id: RequestId::from_bytes([0; 32]),
        }
    }

    /// Returns the event identity that the envelope fields derive.
    #[must_use]
    pub fn derived_event_id(&self) -> EventId {
        EventId::derive(
            &self.shard_id,
            &self.key,
            self.commit_version,
            &self.rule_id,
            self.emission_index,
        )
    }

    /// Returns true when the stored identity matches the derived identity.
    #[must_use]
    pub fn has_valid_event_id(&self) -> bool {
        self.event_id == self.derived_event_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> TableName {
        TableName::parse_user("invoice").expect("the table name is valid")
    }

    fn payload() -> EffectPayload {
        EffectPayload::new(
            "ada@example.test".to_owned(),
            "Invoice 1042 approved".to_owned(),
            "The invoice is approved.".to_owned(),
        )
    }

    #[test]
    fn the_event_id_is_deterministic() {
        let shard = ShardId::new(b"invoice/".to_vec());
        let first = EventId::derive(
            &shard,
            b"invoice/1042",
            CommitVersion::new(91),
            "invoice.approved",
            0,
        );
        let second = EventId::derive(
            &shard,
            b"invoice/1042",
            CommitVersion::new(91),
            "invoice.approved",
            0,
        );
        assert_eq!(first, second);
        assert_ne!(
            first,
            EventId::derive(
                &shard,
                b"invoice/1042",
                CommitVersion::new(92),
                "invoice.approved",
                0
            )
        );
        assert_ne!(
            first,
            EventId::derive(
                &shard,
                b"invoice/1042",
                CommitVersion::new(91),
                "invoice.approved",
                1
            )
        );
        assert_ne!(
            first,
            EventId::derive(
                &shard,
                b"invoice/1043",
                CommitVersion::new(91),
                "invoice.approved",
                0
            )
        );
    }

    #[test]
    fn a_payload_requires_every_required_field() {
        let valid = payload();
        assert_eq!(valid.validate(), Ok(()));
        assert_eq!(
            Effect::from_payload(&valid),
            Ok(Effect {
                recipient: valid.recipient.clone(),
                subject: valid.subject.clone(),
                body: valid.body.clone(),
                enrichment: None,
            })
        );

        let mut missing = valid.clone();
        missing.recipient.clear();
        assert_eq!(missing.validate(), Err(EffectPayloadError::Recipient));
        assert_eq!(EffectPayloadError::Recipient.field(), "recipient");
        assert_eq!(
            EffectPayloadError::Subject.to_string(),
            "The payload has no subject."
        );
        assert_eq!(
            EffectPayloadError::Body.to_string(),
            "The payload has no body."
        );
    }

    #[test]
    fn an_optional_reference_is_carried_but_not_read() {
        let reference = ObjectReference::new(ContentDigest::from_bytes([7; 32]), 4);
        let payload = payload().with_enrichment(reference.clone());
        let effect = Effect::from_payload(&payload).expect("the payload is valid");
        assert_eq!(effect.enrichment, Some(reference));
    }

    #[test]
    fn a_payload_debug_form_redacts_text() {
        let payload = payload();
        let text = format!("{payload:?}");
        assert!(!text.contains("ada@example.test"));
        assert!(!text.contains("Invoice 1042 approved"));
        assert!(!text.contains("The invoice is approved."));
        assert!(text.contains("<redacted>"));
    }

    #[test]
    fn the_envelope_identity_matches_its_fields() {
        let shard = ShardId::new(b"invoice/".to_vec());
        let event_id = EventId::derive(
            &shard,
            b"invoice/1042",
            CommitVersion::new(91),
            "invoice.approved",
            0,
        );
        let mut envelope = EventEnvelope {
            commit_version: CommitVersion::new(91),
            rule_id: "invoice.approved".to_owned(),
            ..EventEnvelope::new(
                event_id,
                shard,
                table(),
                b"invoice/1042".to_vec(),
                EventSequence::new(1),
                payload(),
            )
        };
        assert_eq!(envelope.derived_event_id(), event_id);
        assert!(envelope.has_valid_event_id());
        envelope.rule_id = "invoice.rejected".to_owned();
        assert!(!envelope.has_valid_event_id());
    }

    #[test]
    fn the_envelope_carries_the_request_identity() {
        let envelope = EventEnvelope::new(
            EventId::from_bytes([1; 32]),
            ShardId::new(b"invoice/".to_vec()),
            table(),
            b"invoice/1042".to_vec(),
            EventSequence::new(1),
            payload(),
        );
        assert_eq!(envelope.envelope_version, EFFECT_ENVELOPE_VERSION);
        assert_eq!(envelope.origin_request_id, RequestId::from_bytes([0; 32]));
        assert_eq!(envelope.payload_schema, EFFECT_PAYLOAD_SCHEMA);
    }
}
