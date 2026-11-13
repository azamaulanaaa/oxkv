//! Shared wire value types.

use core::fmt;
use std::collections::BTreeSet;

use crate::codec::{CodecError, Decoder, Encoder};

/// The capability tag for ordered diff streams.
pub const CAPABILITY_DIFF_V1: &str = "sync.diff.v1";
/// The capability tag for snapshot page transfer.
pub const CAPABILITY_SNAPSHOT_V1: &str = "sync.snapshot.v1";
/// The capability tag for resumable cursors.
pub const CAPABILITY_RESUME_V1: &str = "sync.resume.v1";
/// The capability tag for opaque policy payloads.
pub const CAPABILITY_POLICY_OPAQUE_V1: &str = "sync.policy-opaque.v1";
/// The capability tag for receipt replay.
pub const CAPABILITY_RECEIPT_V1: &str = "sync.receipt.v1";
/// The capability tag for credit-based flow control.
pub const CAPABILITY_FLOW_CONTROL_V1: &str = "sync.flow-control.v1";
/// The capability tag for keepalive frames.
pub const CAPABILITY_KEEPALIVE_V1: &str = "sync.keepalive.v1";

/// The identity of one subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriptionId(u64);

impl SubscriptionId {
    /// Creates a subscription identity.
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

impl fmt::Display for SubscriptionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// A stream sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageSequence(u64);

impl MessageSequence {
    /// Creates a stream sequence number.
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

impl fmt::Display for MessageSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// A canonical set of capability names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    entries: BTreeSet<String>,
}

impl Capabilities {
    /// Creates an empty capability set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a capability set from names.
    #[must_use]
    pub fn from_names(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut capabilities = Self::new();
        for name in names {
            capabilities.insert(name);
        }
        capabilities
    }

    /// Adds one capability.
    pub fn insert(&mut self, name: impl Into<String>) {
        self.entries.insert(name.into());
    }

    /// Returns true when the set contains the capability.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains(name)
    }

    /// Returns the number of capabilities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates over the names in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }

    /// Returns the names that this set has and another set lacks.
    #[must_use]
    pub fn missing_from(&self, supported: &Self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|name| !supported.entries.contains(*name))
            .cloned()
            .collect()
    }

    /// Returns the intersection with another set.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        Self {
            entries: self.entries.intersection(&other.entries).cloned().collect(),
        }
    }

    /// Returns the union with another set.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        Self {
            entries: self.entries.union(&other.entries).cloned().collect(),
        }
    }

    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u32(u32::try_from(self.entries.len()).unwrap_or(u32::MAX));
        for name in &self.entries {
            encoder.text(name);
        }
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        let count = decoder.count()?;
        let mut capabilities = Self::new();
        for _ in 0..count {
            capabilities.insert(decoder.text()?);
        }
        Ok(capabilities)
    }
}

/// The size and count limits that one session negotiates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest frame in bytes.
    pub max_frame_bytes: u32,
    /// The largest message payload in bytes.
    pub max_message_bytes: u32,
    /// The largest diff batch in entries.
    pub max_batch_entries: u32,
    /// The largest scope in prefixes.
    pub max_scope_prefixes: u32,
    /// The largest snapshot in pages.
    pub max_snapshot_pages: u32,
    /// The largest number of unacknowledged messages.
    pub max_in_flight_messages: u32,
    /// The keepalive interval in milliseconds.
    pub keepalive_interval_millis: u32,
}

impl Limits {
    /// Returns the reference 1.0 limits.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            max_frame_bytes: 1_048_576,
            max_message_bytes: 1_048_576,
            max_batch_entries: 1_024,
            max_scope_prefixes: 64,
            max_snapshot_pages: 4_096,
            max_in_flight_messages: 1_024,
            keepalive_interval_millis: 30_000,
        }
    }

    /// Returns the smaller value of each field.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self {
            max_frame_bytes: min_u32(self.max_frame_bytes, other.max_frame_bytes),
            max_message_bytes: min_u32(self.max_message_bytes, other.max_message_bytes),
            max_batch_entries: min_u32(self.max_batch_entries, other.max_batch_entries),
            max_scope_prefixes: min_u32(self.max_scope_prefixes, other.max_scope_prefixes),
            max_snapshot_pages: min_u32(self.max_snapshot_pages, other.max_snapshot_pages),
            max_in_flight_messages: min_u32(
                self.max_in_flight_messages,
                other.max_in_flight_messages,
            ),
            keepalive_interval_millis: min_u32(
                self.keepalive_interval_millis,
                other.keepalive_interval_millis,
            ),
        }
    }

    pub(crate) fn encode(self, encoder: &mut Encoder) {
        encoder.u32(self.max_frame_bytes);
        encoder.u32(self.max_message_bytes);
        encoder.u32(self.max_batch_entries);
        encoder.u32(self.max_scope_prefixes);
        encoder.u32(self.max_snapshot_pages);
        encoder.u32(self.max_in_flight_messages);
        encoder.u32(self.keepalive_interval_millis);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            max_frame_bytes: decoder.u32()?,
            max_message_bytes: decoder.u32()?,
            max_batch_entries: decoder.u32()?,
            max_scope_prefixes: decoder.u32()?,
            max_snapshot_pages: decoder.u32()?,
            max_in_flight_messages: decoder.u32()?,
            keepalive_interval_millis: decoder.u32()?,
        })
    }
}

const fn min_u32(left: u32, right: u32) -> u32 {
    if left < right { left } else { right }
}
