//! The operational feed.
//!
//! The operational feed is a separate stream with its own envelope and its own
//! retention. It carries audit export, metrics, traces, budget signals,
//! lifecycle notices, and alerts.
//!
//! The operational feed is not the business event stream. It is not the only
//! path to a business side effect. An operational signal cannot become a
//! [`crate::BusinessEvent`].

use core::fmt;
use std::collections::BTreeMap;

use oxkv_contracts::version::{Hash32, ShardId};

use crate::codec::{DecodeError, Decoder, Encoder};
use crate::error::EventError;
use crate::id::CommittedAt;
use crate::retention::RetentionClass;

/// The current operational feed version text.
pub const OPERATIONAL_FEED_VERSION_TEXT: &str = "oxkv/ops-feed/v1";

/// The operational feed version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OperationalFeedVersion(u16);

impl OperationalFeedVersion {
    /// The current version.
    pub const V1: Self = Self(1);

    /// Creates a feed version.
    #[must_use]
    pub const fn new(revision: u16) -> Self {
        Self(revision)
    }

    /// Returns the revision number.
    #[must_use]
    pub const fn revision(self) -> u16 {
        self.0
    }
}

impl fmt::Display for OperationalFeedVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "oxkv/ops-feed/v{}", self.0)
    }
}

/// One kind of operational signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OperationalSignalKind {
    /// An audit export signal.
    AuditExport,
    /// A metrics signal.
    Metrics,
    /// A trace signal.
    Trace,
    /// A budget signal.
    BudgetSignal,
    /// A lifecycle notice.
    LifecycleNotice,
    /// An alert.
    Alert,
}

impl OperationalSignalKind {
    /// Every operational signal kind.
    pub const ALL: &'static [Self] = &[
        Self::AuditExport,
        Self::Metrics,
        Self::Trace,
        Self::BudgetSignal,
        Self::LifecycleNotice,
        Self::Alert,
    ];

    /// Returns the canonical kind name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::AuditExport => "audit_export",
            Self::Metrics => "metrics",
            Self::Trace => "trace",
            Self::BudgetSignal => "budget_signal",
            Self::LifecycleNotice => "lifecycle_notice",
            Self::Alert => "alert",
        }
    }

    /// Returns the stable wire tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::AuditExport => 1,
            Self::Metrics => 2,
            Self::Trace => 3,
            Self::BudgetSignal => 4,
            Self::LifecycleNotice => 5,
            Self::Alert => 6,
        }
    }

    /// Reads a kind from its stable wire tag.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::AuditExport),
            2 => Some(Self::Metrics),
            3 => Some(Self::Trace),
            4 => Some(Self::BudgetSignal),
            5 => Some(Self::LifecycleNotice),
            6 => Some(Self::Alert),
            _ => None,
        }
    }

    /// Returns false. An operational signal is not a business side effect.
    #[must_use]
    pub const fn is_business_side_effect(self) -> bool {
        let _ = self;
        false
    }
}

impl fmt::Display for OperationalSignalKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A signal schema identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignalSchema(String);

impl SignalSchema {
    /// Creates a signal schema identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the schema text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A deterministic operational signal identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignalId(Hash32);

impl SignalId {
    /// Creates a signal id from raw hash bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Hash32::from_bytes(bytes))
    }

    /// Returns the raw signal id bytes.
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

/// A text payload for one operational signal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperationalPayload {
    fields: BTreeMap<String, String>,
}

impl OperationalPayload {
    /// Creates an empty payload.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts one text field and returns the previous value.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) -> Option<String> {
        self.fields.insert(name.into(), value.into())
    }

    /// Inserts one text field and returns the payload.
    #[must_use]
    pub fn with_field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.insert(name, value);
        self
    }

    /// Returns one field value.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// Iterates over the fields in canonical name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.fields
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
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
            encoder.text(value);
        }
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let count = decoder.count()?;
        let mut fields = BTreeMap::new();
        for _ in 0..count {
            let name = decoder.text()?;
            let value = decoder.text()?;
            if fields.insert(name, value).is_some() {
                return Err(DecodeError);
            }
        }
        Ok(Self { fields })
    }
}

/// One operational feed envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalEnvelope {
    feed_version: OperationalFeedVersion,
    signal_id: SignalId,
    kind: OperationalSignalKind,
    shard_id: ShardId,
    sequence: u64,
    occurred_at: CommittedAt,
    schema: SignalSchema,
    payload: OperationalPayload,
    retention: RetentionClass,
}

impl OperationalEnvelope {
    /// Builds one operational envelope.
    #[must_use]
    pub fn new(
        kind: OperationalSignalKind,
        shard_id: ShardId,
        sequence: u64,
        occurred_at: CommittedAt,
        schema: SignalSchema,
        payload: OperationalPayload,
        retention: RetentionClass,
    ) -> Self {
        let mut encoder = Encoder::new();
        encoder.text(OPERATIONAL_FEED_VERSION_TEXT);
        encoder.u8(kind.tag());
        encoder.bytes(shard_id.as_bytes());
        encoder.u64(sequence);
        encoder.u64(occurred_at.get());
        encoder.text(schema.as_str());
        payload.encode(&mut encoder);
        let signal_id = SignalId::from_bytes(*Hash32::of(&encoder.finish()).as_bytes());
        Self {
            feed_version: OperationalFeedVersion::V1,
            signal_id,
            kind,
            shard_id,
            sequence,
            occurred_at,
            schema,
            payload,
            retention,
        }
    }

    /// Returns the feed version.
    #[must_use]
    pub const fn feed_version(&self) -> OperationalFeedVersion {
        self.feed_version
    }

    /// Returns the deterministic signal id.
    #[must_use]
    pub const fn signal_id(&self) -> SignalId {
        self.signal_id
    }

    /// Returns the signal kind.
    #[must_use]
    pub const fn kind(&self) -> OperationalSignalKind {
        self.kind
    }

    /// Returns the shard id.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the feed sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the injected occurrence timestamp.
    #[must_use]
    pub const fn occurred_at(&self) -> CommittedAt {
        self.occurred_at
    }

    /// Returns the signal schema.
    #[must_use]
    pub const fn schema(&self) -> &SignalSchema {
        &self.schema
    }

    /// Returns the signal payload.
    #[must_use]
    pub const fn payload(&self) -> &OperationalPayload {
        &self.payload
    }

    /// Returns the operational retention class.
    #[must_use]
    pub const fn retention(&self) -> RetentionClass {
        self.retention
    }

    /// Encodes the envelope in the canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.text(OPERATIONAL_FEED_VERSION_TEXT);
        encoder.raw32(self.signal_id.as_bytes());
        encoder.u8(self.kind.tag());
        encoder.bytes(self.shard_id.as_bytes());
        encoder.u64(self.sequence);
        encoder.u64(self.occurred_at.get());
        encoder.text(self.schema.as_str());
        self.payload.encode(&mut encoder);
        self.retention.encode(&mut encoder);
        encoder.finish()
    }

    /// Decodes one operational envelope.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::UnknownVersion`] when the feed version is not
    /// `oxkv/ops-feed/v1`. Returns [`EventError::Poison`] when the bytes are
    /// malformed or the signal id does not match the fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, EventError> {
        let mut decoder = Decoder::new(bytes);
        let found = decoder.text().map_err(|_| EventError::UnknownVersion {
            found: "<malformed>".to_owned(),
        })?;
        if found != OPERATIONAL_FEED_VERSION_TEXT {
            return Err(EventError::UnknownVersion { found });
        }
        let signal_id = SignalId::from_bytes(decoder.raw32().map_err(|_| EventError::Poison {
            event_id: crate::id::EventId::from_bytes([0; 32]),
            reason: "the operational envelope bytes are malformed",
        })?);
        let malformed = || EventError::Poison {
            event_id: crate::id::EventId::from_bytes([0; 32]),
            reason: "the operational envelope bytes are malformed",
        };
        let kind = OperationalSignalKind::from_tag(decoder.u8().map_err(|_| malformed())?)
            .ok_or_else(malformed)?;
        let shard_id = ShardId::new(decoder.bytes().map_err(|_| malformed())?.to_vec());
        let sequence = decoder.u64().map_err(|_| malformed())?;
        let occurred_at = CommittedAt::new(decoder.u64().map_err(|_| malformed())?);
        let schema = SignalSchema::new(decoder.text().map_err(|_| malformed())?);
        let payload = OperationalPayload::decode(&mut decoder).map_err(|_| malformed())?;
        let retention = RetentionClass::decode(&mut decoder).map_err(|_| malformed())?;
        decoder.finish().map_err(|_| malformed())?;
        let expected = Self::new(
            kind,
            shard_id.clone(),
            sequence,
            occurred_at,
            schema.clone(),
            payload.clone(),
            retention,
        );
        if expected.signal_id != signal_id {
            return Err(EventError::Poison {
                event_id: crate::id::EventId::from_bytes([0; 32]),
                reason: "the signal id does not match the feed fields",
            });
        }
        Ok(Self {
            feed_version: OperationalFeedVersion::V1,
            signal_id,
            kind,
            shard_id,
            sequence,
            occurred_at,
            schema,
            payload,
            retention,
        })
    }
}

/// A separate operational feed with its own retention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalFeed {
    retention: RetentionClass,
    signals: Vec<OperationalEnvelope>,
    next_sequence: u64,
}

impl OperationalFeed {
    /// Creates an empty operational feed.
    #[must_use]
    pub fn new(retention: RetentionClass) -> Self {
        Self {
            retention,
            signals: Vec::new(),
            next_sequence: 1,
        }
    }

    /// Appends one signal and returns the new envelope.
    pub fn append(
        &mut self,
        kind: OperationalSignalKind,
        shard_id: ShardId,
        occurred_at: CommittedAt,
        schema: SignalSchema,
        payload: OperationalPayload,
    ) -> OperationalEnvelope {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let envelope = OperationalEnvelope::new(
            kind,
            shard_id,
            sequence,
            occurred_at,
            schema,
            payload,
            self.retention,
        );
        self.signals.push(envelope.clone());
        envelope
    }

    /// Returns the feed retention class.
    #[must_use]
    pub const fn retention(&self) -> RetentionClass {
        self.retention
    }

    /// Returns the signals in append order.
    #[must_use]
    pub fn signals(&self) -> &[OperationalEnvelope] {
        &self.signals
    }

    /// Returns the signal count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.signals.len()
    }

    /// Returns true when the feed has no signals.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.signals.is_empty()
    }

    /// Encodes the feed in the canonical form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u64(u64::try_from(self.signals.len()).unwrap_or(u64::MAX));
        for signal in &self.signals {
            encoder.bytes(&signal.encode());
        }
        encoder.finish()
    }

    /// Decodes one operational feed.
    ///
    /// # Errors
    ///
    /// Returns [`EventError`] when the bytes are malformed or one signal is
    /// invalid.
    pub fn decode(bytes: &[u8]) -> Result<Self, EventError> {
        let mut decoder = Decoder::new(bytes);
        let count = decoder.count().map_err(|_| EventError::UnknownVersion {
            found: "<malformed>".to_owned(),
        })?;
        let mut feed = Self::new(RetentionClass::time(1));
        for _ in 0..count {
            let signal_bytes = decoder.bytes().map_err(|_| EventError::UnknownVersion {
                found: "<malformed>".to_owned(),
            })?;
            let signal = OperationalEnvelope::decode(signal_bytes)?;
            feed.retention = signal.retention;
            feed.next_sequence = feed.next_sequence.max(signal.sequence.saturating_add(1));
            feed.signals.push(signal);
        }
        Ok(feed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> OperationalPayload {
        OperationalPayload::new().with_field("export_id", "exp-1")
    }

    #[test]
    fn every_signal_kind_is_not_a_business_side_effect() {
        for kind in OperationalSignalKind::ALL {
            assert!(!kind.is_business_side_effect());
            assert_eq!(OperationalSignalKind::from_tag(kind.tag()), Some(*kind));
        }
        assert!(OperationalSignalKind::from_tag(0).is_none());
        assert!(OperationalSignalKind::from_tag(7).is_none());
    }

    #[test]
    fn the_operational_envelope_round_trips() {
        let envelope = OperationalEnvelope::new(
            OperationalSignalKind::AuditExport,
            ShardId::new(vec![1]),
            1,
            CommittedAt::new(1_760_000_000),
            SignalSchema::new("audit-export/v1"),
            payload(),
            RetentionClass::time_and_bytes(3600, 4096),
        );
        assert_eq!(
            OperationalEnvelope::decode(&envelope.encode()),
            Ok(envelope)
        );
    }

    #[test]
    fn the_feed_uses_its_own_retention() {
        let mut feed = OperationalFeed::new(RetentionClass::time(60));
        let signal = feed.append(
            OperationalSignalKind::Metrics,
            ShardId::new(vec![1]),
            CommittedAt::new(1_760_000_000),
            SignalSchema::new("metrics/v1"),
            OperationalPayload::new(),
        );
        assert_eq!(feed.retention(), RetentionClass::time(60));
        assert_eq!(signal.retention(), RetentionClass::time(60));
        assert_eq!(feed.signals().len(), 1);
        assert_eq!(OperationalFeed::decode(&feed.encode()), Ok(feed));
    }

    #[test]
    fn a_budget_signal_is_an_operational_signal() {
        assert_eq!(OperationalSignalKind::BudgetSignal.name(), "budget_signal");
        assert!(!OperationalSignalKind::BudgetSignal.is_business_side_effect());
    }
}
