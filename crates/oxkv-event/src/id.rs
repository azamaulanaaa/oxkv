//! Deterministic business event identity.
//!
//! The event id is a `BLAKE3` hash of the shard, the key, the commit version,
//! the rule identity, and the emission index. The hash input is canonical.
//! The same commit produces the same id on native and `wasm32`.

use core::fmt;

use oxkv_contracts::version::{CommitVersion, DigestDomain, Hash32, ShardId};

use crate::codec::Encoder;

/// A deterministic business event identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventId(Hash32);

impl EventId {
    /// Creates an event id from raw hash bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Hash32::from_bytes(bytes))
    }

    /// Returns the raw event id bytes.
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
            .field(&self.0.to_hex())
            .finish()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.to_hex())
    }
}

/// A manifest rule identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleId(String);

impl RuleId {
    /// Creates a rule identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the rule identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An event emission index inside one commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EmissionIndex(u64);

impl EmissionIndex {
    /// Creates an emission index.
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

impl fmt::Display for EmissionIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// An injected commit timestamp.
///
/// The core never reads a clock. The authority supplies this value at commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommittedAt(u64);

impl CommittedAt {
    /// Creates an injected commit timestamp.
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

impl fmt::Display for CommittedAt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// An opaque origin request identity.
///
/// This value is not the raw idempotency key. The envelope carries this value
/// so a consumer can correlate an effect without client identity leakage.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OriginRequestId(Vec<u8>);

impl OriginRequestId {
    /// Creates an origin request identity.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Returns the raw identity bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for OriginRequestId {
    fn from(bytes: Vec<u8>) -> Self {
        Self::new(bytes)
    }
}

/// A payload schema identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PayloadSchema(String);

impl PayloadSchema {
    /// Creates a payload schema identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the schema identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A required effect field name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EffectField(String);

impl EffectField {
    /// Creates a required effect field name.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the field name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Computes the deterministic event identity.
///
/// The function hashes the shard, the key, the commit version, the rule
/// identity, and the emission index. It uses the `oxkv/event-id/v1` domain.
#[must_use]
pub fn compute_event_id(
    shard_id: &ShardId,
    key: &[u8],
    commit_version: CommitVersion,
    rule_id: &RuleId,
    emission_index: EmissionIndex,
) -> EventId {
    let mut encoder = Encoder::new();
    encoder.bytes(shard_id.as_bytes());
    encoder.bytes(key);
    encoder.u64(commit_version.get());
    encoder.text(rule_id.as_str());
    encoder.u64(emission_index.get());
    EventId(Hash32::of_domain(DigestDomain::EventId, &encoder.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_id() -> EventId {
        compute_event_id(
            &ShardId::new(vec![1, 2]),
            b"accounts/42",
            CommitVersion::new(18),
            &RuleId::new("welcome_mail"),
            EmissionIndex::new(0),
        )
    }

    #[test]
    fn the_event_id_is_stable() {
        assert_eq!(sample_id(), sample_id());
        assert_eq!(sample_id().to_hex(), sample_id().to_hex());
    }

    #[test]
    fn every_identity_field_changes_the_id() {
        let base = sample_id();
        assert_ne!(
            base,
            compute_event_id(
                &ShardId::new(vec![1, 3]),
                b"accounts/42",
                CommitVersion::new(18),
                &RuleId::new("welcome_mail"),
                EmissionIndex::new(0),
            )
        );
        assert_ne!(
            base,
            compute_event_id(
                &ShardId::new(vec![1, 2]),
                b"accounts/43",
                CommitVersion::new(18),
                &RuleId::new("welcome_mail"),
                EmissionIndex::new(0),
            )
        );
        assert_ne!(
            base,
            compute_event_id(
                &ShardId::new(vec![1, 2]),
                b"accounts/42",
                CommitVersion::new(19),
                &RuleId::new("welcome_mail"),
                EmissionIndex::new(0),
            )
        );
        assert_ne!(
            base,
            compute_event_id(
                &ShardId::new(vec![1, 2]),
                b"accounts/42",
                CommitVersion::new(18),
                &RuleId::new("welcome_mail_v2"),
                EmissionIndex::new(0),
            )
        );
        assert_ne!(
            base,
            compute_event_id(
                &ShardId::new(vec![1, 2]),
                b"accounts/42",
                CommitVersion::new(18),
                &RuleId::new("welcome_mail"),
                EmissionIndex::new(1),
            )
        );
    }

    #[test]
    fn the_id_has_32_bytes_and_hex_text() {
        let id = sample_id();
        assert_eq!(id.as_bytes().len(), 32);
        assert_eq!(id.to_hex().len(), 64);
        assert_eq!(id.to_string(), id.to_hex());
    }
}
