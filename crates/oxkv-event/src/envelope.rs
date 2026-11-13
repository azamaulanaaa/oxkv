//! The canonical `oxkv/event/v1` business event envelope.
//!
//! The envelope carries the event identity, the shard, the key, the commit
//! version, the rule identity, the emission index, the origin request id, the
//! pinned policy version, the payload schema, the required effect fields, the
//! retention class, and an injected commit timestamp.
//!
//! The encoding is canonical. It does not depend on the platform, the pointer
//! width, or the Rust enum layout.

use std::collections::BTreeMap;

use oxkv_contracts::cursor::EventSequence;
use oxkv_contracts::keyspace::{KeyspaceError, TableName};
use oxkv_contracts::version::{
    CommitVersion, ContentDigest, EventVersion, Hash32, PolicyVersion, ShardId,
};

use crate::codec::{DecodeError, Decoder, Encoder};
use crate::error::EventError;
use crate::id::{
    CommittedAt, EffectField, EmissionIndex, EventId, OriginRequestId, PayloadSchema, RuleId,
    compute_event_id,
};
use crate::retention::RetentionClass;

/// The current event envelope version text.
pub const EVENT_VERSION_TEXT: &str = "oxkv/event/v1";

/// The current event envelope version.
pub const EVENT_VERSION: EventVersion = EventVersion::new(1);

/// The default payload byte bound.
pub const DEFAULT_PAYLOAD_BOUND: PayloadBound = PayloadBound::new(64 * 1024);

/// A bounded byte budget for one event payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PayloadBound {
    max_bytes: usize,
}

impl PayloadBound {
    /// Creates a payload bound.
    #[must_use]
    pub const fn new(max_bytes: usize) -> Self {
        Self { max_bytes }
    }

    /// Returns the maximum byte length.
    #[must_use]
    pub const fn max_bytes(self) -> usize {
        self.max_bytes
    }
}

impl Default for PayloadBound {
    fn default() -> Self {
        DEFAULT_PAYLOAD_BOUND
    }
}

/// The version of one immutable object that a reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectVersion(u64);

impl ObjectVersion {
    /// Creates an object version.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A reference to one immutable object.
///
/// A reference can travel in an optional enrichment field. A required effect
/// field must not contain a reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectReference {
    object_id: String,
    digest: ContentDigest,
    version: ObjectVersion,
}

impl ObjectReference {
    /// Creates an object reference.
    ///
    /// The caller must name one immutable object. The digest and the version
    /// identify the object content.
    #[must_use]
    pub fn new(
        object_id: impl Into<String>,
        digest: ContentDigest,
        version: ObjectVersion,
    ) -> Self {
        Self {
            object_id: object_id.into(),
            digest,
            version,
        }
    }

    /// Returns the object id.
    #[must_use]
    pub fn object_id(&self) -> &str {
        &self.object_id
    }

    /// Returns the immutable content digest.
    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }

    /// Returns the immutable object version.
    #[must_use]
    pub const fn version(&self) -> ObjectVersion {
        self.version
    }
}

/// One value in an event payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadValue {
    /// A UTF-8 text value.
    Text(String),
    /// An opaque byte value.
    Bytes(Vec<u8>),
    /// A signed integer value.
    Integer(i64),
    /// A boolean value.
    Boolean(bool),
    /// An optional object reference.
    Reference(ObjectReference),
}

impl PayloadValue {
    /// Returns true when the value is an object reference.
    #[must_use]
    pub const fn is_reference(&self) -> bool {
        matches!(self, Self::Reference(_))
    }

    /// Returns the object reference when the value is one.
    #[must_use]
    pub const fn as_reference(&self) -> Option<&ObjectReference> {
        match self {
            Self::Reference(reference) => Some(reference),
            Self::Text(_) | Self::Bytes(_) | Self::Integer(_) | Self::Boolean(_) => None,
        }
    }

    const fn tag(&self) -> u8 {
        match self {
            Self::Text(_) => 1,
            Self::Bytes(_) => 2,
            Self::Integer(_) => 3,
            Self::Boolean(_) => 4,
            Self::Reference(_) => 5,
        }
    }

    fn encode(&self, encoder: &mut Encoder) {
        encoder.u8(self.tag());
        match self {
            Self::Text(value) => encoder.text(value),
            Self::Bytes(value) => encoder.bytes(value),
            Self::Integer(value) => encoder.i64(*value),
            Self::Boolean(value) => encoder.bool(*value),
            Self::Reference(reference) => {
                encoder.text(reference.object_id());
                encoder.raw32(reference.digest().as_bytes());
                encoder.u64(reference.version().get());
            }
        }
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        match decoder.u8()? {
            1 => Ok(Self::Text(decoder.text()?)),
            2 => Ok(Self::Bytes(decoder.bytes()?.to_vec())),
            3 => Ok(Self::Integer(decoder.i64()?)),
            4 => Ok(Self::Boolean(decoder.bool()?)),
            5 => {
                let object_id = decoder.text()?;
                let digest = ContentDigest::from_bytes(decoder.raw32()?);
                let version = ObjectVersion::new(decoder.u64()?);
                Ok(Self::Reference(ObjectReference::new(
                    object_id, digest, version,
                )))
            }
            _ => Err(DecodeError),
        }
    }
}

/// A bounded, canonical map of payload fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventPayload {
    fields: BTreeMap<String, PayloadValue>,
}

impl EventPayload {
    /// Creates an empty payload.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts one field and returns the previous value.
    pub fn insert(&mut self, name: impl Into<String>, value: PayloadValue) -> Option<PayloadValue> {
        self.fields.insert(name.into(), value)
    }

    /// Inserts one field and returns the payload.
    #[must_use]
    pub fn with_field(mut self, name: impl Into<String>, value: PayloadValue) -> Self {
        self.insert(name, value);
        self
    }

    /// Returns one field value.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PayloadValue> {
        self.fields.get(name)
    }

    /// Iterates over the fields in canonical name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &PayloadValue)> {
        self.fields
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Returns the field count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns true when the payload has no fields.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(u64::try_from(self.fields.len()).unwrap_or(u64::MAX));
        for (name, value) in &self.fields {
            encoder.text(name);
            value.encode(encoder);
        }
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let count = decoder.count()?;
        let mut fields = BTreeMap::new();
        for _ in 0..count {
            let name = decoder.text()?;
            let value = PayloadValue::decode(decoder)?;
            if fields.insert(name, value).is_some() {
                return Err(DecodeError);
            }
        }
        Ok(Self { fields })
    }

    fn encoded_len(&self) -> usize {
        let mut encoder = Encoder::new();
        self.encode(&mut encoder);
        encoder.finish().len()
    }
}

/// The fields that build one business event envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEnvelopeParts {
    /// The shard that owns the commit.
    pub shard_id: ShardId,
    /// The user table name.
    pub table: TableName,
    /// The canonical key bytes.
    pub key: Vec<u8>,
    /// The per-key event sequence.
    pub key_sequence: EventSequence,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The injected commit timestamp.
    pub committed_at: CommittedAt,
    /// The manifest rule identity.
    pub rule_id: RuleId,
    /// The pinned policy version.
    pub policy_version: PolicyVersion,
    /// The emission index inside one commit.
    pub emission_index: EmissionIndex,
    /// The bounded payload.
    pub payload: EventPayload,
    /// The payload schema identity.
    pub payload_schema: PayloadSchema,
    /// The opaque origin request identity.
    pub origin_request_id: OriginRequestId,
    /// The required effect fields.
    pub required_effects: Vec<EffectField>,
    /// The retention class.
    pub retention: RetentionClass,
    /// The payload byte bound.
    pub payload_bound: PayloadBound,
}

/// A canonical business event envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEnvelope {
    envelope_version: EventVersion,
    event_id: EventId,
    shard_id: ShardId,
    table: TableName,
    key: Vec<u8>,
    key_sequence: EventSequence,
    commit_version: CommitVersion,
    committed_at: CommittedAt,
    rule_id: RuleId,
    policy_version: PolicyVersion,
    emission_index: EmissionIndex,
    payload: EventPayload,
    payload_schema: PayloadSchema,
    origin_request_id: OriginRequestId,
    required_effects: Vec<EffectField>,
    retention: RetentionClass,
    payload_bound: PayloadBound,
}

impl EventEnvelope {
    /// Builds one canonical envelope.
    ///
    /// The function computes the deterministic event id. It then checks the
    /// payload bound and the required effect fields.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::PayloadTooLarge`] when the payload is above the
    /// declared bound. Returns [`EventError::Poison`] when a required effect
    /// field is absent or is a reference.
    pub fn new(parts: EventEnvelopeParts) -> Result<Self, EventError> {
        let event_id = compute_event_id(
            &parts.shard_id,
            &parts.key,
            parts.commit_version,
            &parts.rule_id,
            parts.emission_index,
        );
        let envelope = Self {
            envelope_version: EVENT_VERSION,
            event_id,
            shard_id: parts.shard_id,
            table: parts.table,
            key: parts.key,
            key_sequence: parts.key_sequence,
            commit_version: parts.commit_version,
            committed_at: parts.committed_at,
            rule_id: parts.rule_id,
            policy_version: parts.policy_version,
            emission_index: parts.emission_index,
            payload: parts.payload,
            payload_schema: parts.payload_schema,
            origin_request_id: parts.origin_request_id,
            required_effects: parts.required_effects,
            retention: parts.retention,
            payload_bound: parts.payload_bound,
        };
        envelope.validate_payload_bound()?;
        envelope.validate_effects()?;
        Ok(envelope)
    }

    /// Returns the envelope version.
    #[must_use]
    pub const fn envelope_version(&self) -> EventVersion {
        self.envelope_version
    }

    /// Returns the deterministic event id.
    #[must_use]
    pub const fn event_id(&self) -> EventId {
        self.event_id
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

    /// Returns the per-key event sequence.
    #[must_use]
    pub const fn key_sequence(&self) -> EventSequence {
        self.key_sequence
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

    /// Returns the manifest rule identity.
    #[must_use]
    pub const fn rule_id(&self) -> &RuleId {
        &self.rule_id
    }

    /// Returns the pinned policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the emission index inside one commit.
    #[must_use]
    pub const fn emission_index(&self) -> EmissionIndex {
        self.emission_index
    }

    /// Returns the bounded payload.
    #[must_use]
    pub const fn payload(&self) -> &EventPayload {
        &self.payload
    }

    /// Returns the payload schema identity.
    #[must_use]
    pub const fn payload_schema(&self) -> &PayloadSchema {
        &self.payload_schema
    }

    /// Returns the opaque origin request identity.
    #[must_use]
    pub const fn origin_request_id(&self) -> &OriginRequestId {
        &self.origin_request_id
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

    /// Returns the declared payload byte bound.
    #[must_use]
    pub const fn payload_bound(&self) -> PayloadBound {
        self.payload_bound
    }

    /// Checks the payload against its declared bound.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::PayloadTooLarge`] when the payload is above the
    /// declared bound.
    pub fn validate_payload_bound(&self) -> Result<(), EventError> {
        let actual = self.payload.encoded_len();
        if actual > self.payload_bound.max_bytes() {
            Err(EventError::PayloadTooLarge {
                limit: self.payload_bound.max_bytes(),
                actual,
            })
        } else {
            Ok(())
        }
    }

    /// Returns the canonical encoded payload length.
    #[must_use]
    pub fn payload_byte_len(&self) -> usize {
        self.payload.encoded_len()
    }

    /// Returns true when the field is a declared required effect.
    #[must_use]
    pub fn is_required_effect(&self, field: &EffectField) -> bool {
        self.required_effects
            .iter()
            .any(|required| required == field)
    }

    /// Builds one required effect value from the payload.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::Poison`] when the field is not a declared
    /// required effect, is absent, or is a reference.
    pub fn effect_value(&self, field: &EffectField) -> Result<&PayloadValue, EventError> {
        if !self.is_required_effect(field) {
            return Err(EventError::Poison {
                event_id: self.event_id,
                reason: "the field is not a declared required effect",
            });
        }
        match self.payload.get(field.as_str()) {
            Some(PayloadValue::Reference(_)) => Err(EventError::Poison {
                event_id: self.event_id,
                reason: "a required effect field is a reference",
            }),
            Some(value) => Ok(value),
            None => Err(EventError::Poison {
                event_id: self.event_id,
                reason: "a required effect field is absent",
            }),
        }
    }

    /// Checks every required effect field.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::Poison`] when a required field is absent or is a
    /// reference.
    pub fn validate_effects(&self) -> Result<(), EventError> {
        for field in &self.required_effects {
            match self.payload.get(field.as_str()) {
                Some(PayloadValue::Reference(_)) => {
                    return Err(EventError::Poison {
                        event_id: self.event_id,
                        reason: "a required effect field is a reference",
                    });
                }
                Some(_) => {}
                None => {
                    return Err(EventError::Poison {
                        event_id: self.event_id,
                        reason: "a required effect field is absent",
                    });
                }
            }
        }
        Ok(())
    }

    /// Encodes the envelope in the canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.text(EVENT_VERSION_TEXT);
        encoder.raw32(self.event_id.as_bytes());
        encoder.bytes(self.shard_id.as_bytes());
        encoder.text(self.table.as_str());
        encoder.bytes(&self.key);
        encoder.u64(self.key_sequence.get());
        encoder.u64(self.commit_version.get());
        encoder.u64(self.committed_at.get());
        encoder.text(self.rule_id.as_str());
        encoder.raw32(self.policy_version.as_bytes());
        encoder.u64(self.emission_index.get());
        self.payload.encode(&mut encoder);
        encoder.text(self.payload_schema.as_str());
        encoder.bytes(self.origin_request_id.as_bytes());
        encoder.u64(u64::try_from(self.required_effects.len()).unwrap_or(u64::MAX));
        for field in &self.required_effects {
            encoder.text(field.as_str());
        }
        self.retention.encode(&mut encoder);
        encoder.u64(u64::try_from(self.payload_bound.max_bytes()).unwrap_or(u64::MAX));
        encoder.finish()
    }

    /// Decodes one canonical envelope.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::UnknownVersion`] when the version text is not
    /// `oxkv/event/v1`. Returns [`EventError::Poison`] when the bytes are
    /// malformed, the event id does not match the commit fields, or a required
    /// effect field is invalid.
    pub fn decode(bytes: &[u8]) -> Result<Self, EventError> {
        let mut decoder = Decoder::new(bytes);
        let found = decoder.text().map_err(|_| EventError::UnknownVersion {
            found: "<malformed>".to_owned(),
        })?;
        if found != EVENT_VERSION_TEXT {
            return Err(EventError::UnknownVersion { found });
        }
        let event_id =
            EventId::from_bytes(decoder.raw32().map_err(|_| EventError::UnknownVersion {
                found: "<malformed>".to_owned(),
            })?);
        let malformed = || EventError::Poison {
            event_id,
            reason: "the envelope bytes are malformed",
        };
        let shard_id = ShardId::new(decoder.bytes().map_err(|_| malformed())?.to_vec());
        let table_text = decoder.text().map_err(|_| malformed())?;
        let table = TableName::parse(&table_text).map_err(|_: KeyspaceError| malformed())?;
        let key = decoder.bytes().map_err(|_| malformed())?.to_vec();
        let key_sequence = EventSequence::new(decoder.u64().map_err(|_| malformed())?);
        let commit_version = CommitVersion::new(decoder.u64().map_err(|_| malformed())?);
        let committed_at = CommittedAt::new(decoder.u64().map_err(|_| malformed())?);
        let rule_id = RuleId::new(decoder.text().map_err(|_| malformed())?);
        let policy_version = PolicyVersion::from_bytes(decoder.raw32().map_err(|_| malformed())?);
        let emission_index = EmissionIndex::new(decoder.u64().map_err(|_| malformed())?);
        let payload = EventPayload::decode(&mut decoder).map_err(|_| malformed())?;
        let payload_schema = PayloadSchema::new(decoder.text().map_err(|_| malformed())?);
        let origin_request_id =
            OriginRequestId::new(decoder.bytes().map_err(|_| malformed())?.to_vec());
        let required_count = decoder.count().map_err(|_| malformed())?;
        let mut required_effects = Vec::with_capacity(required_count);
        for _ in 0..required_count {
            required_effects.push(EffectField::new(decoder.text().map_err(|_| malformed())?));
        }
        let retention = RetentionClass::decode(&mut decoder).map_err(|_| malformed())?;
        let payload_bound = PayloadBound::new(
            usize::try_from(decoder.u64().map_err(|_| malformed())?).map_err(|_| malformed())?,
        );
        decoder.finish().map_err(|_| malformed())?;
        let expected = compute_event_id(&shard_id, &key, commit_version, &rule_id, emission_index);
        if expected != event_id {
            return Err(EventError::Poison {
                event_id,
                reason: "the event id does not match the commit fields",
            });
        }
        let envelope = Self {
            envelope_version: EVENT_VERSION,
            event_id,
            shard_id,
            table,
            key,
            key_sequence,
            commit_version,
            committed_at,
            rule_id,
            policy_version,
            emission_index,
            payload,
            payload_schema,
            origin_request_id,
            required_effects,
            retention,
            payload_bound,
        };
        envelope.validate_payload_bound()?;
        envelope.validate_effects()?;
        Ok(envelope)
    }
}

impl EventPayload {
    /// Returns a stable fingerprint of the payload fields.
    #[must_use]
    pub fn fingerprint(&self) -> Hash32 {
        let mut encoder = Encoder::new();
        self.encode(&mut encoder);
        Hash32::of(&encoder.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_parts() -> EventEnvelopeParts {
        EventEnvelopeParts {
            shard_id: ShardId::new(vec![1, 2]),
            table: TableName::parse_user("accounts").expect("the table name is valid"),
            key: b"accounts/42".to_vec(),
            key_sequence: EventSequence::new(1),
            commit_version: CommitVersion::new(18),
            committed_at: CommittedAt::new(1_760_000_000),
            rule_id: RuleId::new("welcome_mail"),
            policy_version: PolicyVersion::from_bytes([4; 32]),
            emission_index: EmissionIndex::new(0),
            payload: EventPayload::new()
                .with_field("recipient", PayloadValue::Text("a@example.com".to_owned())),
            payload_schema: PayloadSchema::new("welcome_mail/v1"),
            origin_request_id: OriginRequestId::new(vec![9, 9]),
            required_effects: vec![EffectField::new("recipient")],
            retention: RetentionClass::time_and_bytes(3600, 1024),
            payload_bound: DEFAULT_PAYLOAD_BOUND,
        }
    }

    #[test]
    fn a_valid_envelope_round_trips() {
        let envelope = EventEnvelope::new(sample_parts()).expect("the envelope is valid");
        let bytes = envelope.encode();
        assert_eq!(EventEnvelope::decode(&bytes), Ok(envelope));
    }

    #[test]
    fn an_unknown_version_is_named() {
        let envelope = EventEnvelope::new(sample_parts()).expect("the envelope is valid");
        let mut bytes = envelope.encode();
        let old = EVENT_VERSION_TEXT.as_bytes();
        let new = b"oxkv/event/v2";
        assert_eq!(old.len(), new.len());
        let text_offset = 8;
        bytes[text_offset..text_offset + old.len()].copy_from_slice(new);
        assert_eq!(
            EventEnvelope::decode(&bytes),
            Err(EventError::UnknownVersion {
                found: "oxkv/event/v2".to_owned()
            })
        );
    }

    #[test]
    fn a_changed_event_id_is_poison() {
        let envelope = EventEnvelope::new(sample_parts()).expect("the envelope is valid");
        let mut bytes = envelope.encode();
        let id_offset = EVENT_VERSION_TEXT.len() + 8;
        bytes[id_offset] ^= 0x01;
        assert_eq!(
            EventEnvelope::decode(&bytes)
                .expect_err("the id is invalid")
                .code(),
            "event.poison"
        );
    }

    #[test]
    fn a_required_field_must_be_self_contained() {
        let mut parts = sample_parts();
        parts.payload = EventPayload::new().with_field(
            "recipient",
            PayloadValue::Reference(ObjectReference::new(
                "object-1",
                ContentDigest::from_bytes([7; 32]),
                ObjectVersion::new(2),
            )),
        );
        let error = EventEnvelope::new(parts).expect_err("a reference cannot be required");
        assert_eq!(error.code(), "event.poison");
    }

    #[test]
    fn an_optional_reference_does_not_block_the_effect() {
        let mut parts = sample_parts();
        parts.payload = parts.payload.with_field(
            "invoice",
            PayloadValue::Reference(ObjectReference::new(
                "object-1",
                ContentDigest::from_bytes([7; 32]),
                ObjectVersion::new(2),
            )),
        );
        let envelope = EventEnvelope::new(parts).expect("the envelope is valid");
        assert!(
            envelope
                .effect_value(&EffectField::new("recipient"))
                .is_ok()
        );
        assert!(matches!(
            envelope.payload().get("invoice"),
            Some(PayloadValue::Reference(_))
        ));
    }

    #[test]
    fn a_payload_above_the_bound_is_rejected() {
        let mut parts = sample_parts();
        parts.payload_bound = PayloadBound::new(4);
        let error = EventEnvelope::new(parts).expect_err("the payload is too large");
        assert_eq!(error.code(), "event.payload_too_large");
    }

    #[test]
    fn the_payload_fingerprint_is_stable() {
        let envelope = EventEnvelope::new(sample_parts()).expect("the envelope is valid");
        assert_eq!(
            envelope.payload().fingerprint(),
            envelope.payload().fingerprint()
        );
    }
}
