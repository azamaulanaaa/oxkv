//! Retention bounds and the retention floor.

use oxkv_contracts::cursor::{EventPosition, EventSequence};
use oxkv_contracts::version::CommitVersion;

use crate::codec::{DecodeError, Decoder, Encoder};

/// The retention bound of one business event or operational feed.
///
/// A retention rule has a time bound, a byte bound, or both. An unbounded
/// retention class does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetentionClass {
    /// A time bound in seconds.
    Time {
        /// The time bound in seconds.
        seconds: u64,
    },
    /// A byte bound.
    Bytes {
        /// The byte bound.
        max_bytes: u64,
    },
    /// A time bound and a byte bound.
    TimeAndBytes {
        /// The time bound in seconds.
        seconds: u64,
        /// The byte bound.
        max_bytes: u64,
    },
}

impl RetentionClass {
    /// Creates a time-bounded retention class.
    #[must_use]
    pub const fn time(seconds: u64) -> Self {
        Self::Time { seconds }
    }

    /// Creates a byte-bounded retention class.
    #[must_use]
    pub const fn bytes(max_bytes: u64) -> Self {
        Self::Bytes { max_bytes }
    }

    /// Creates a retention class with a time bound and a byte bound.
    #[must_use]
    pub const fn time_and_bytes(seconds: u64, max_bytes: u64) -> Self {
        Self::TimeAndBytes { seconds, max_bytes }
    }

    /// Returns the time bound in seconds.
    #[must_use]
    pub const fn time_bound(self) -> Option<u64> {
        match self {
            Self::Time { seconds } | Self::TimeAndBytes { seconds, .. } => Some(seconds),
            Self::Bytes { .. } => None,
        }
    }

    /// Returns the byte bound.
    #[must_use]
    pub const fn byte_bound(self) -> Option<u64> {
        match self {
            Self::Bytes { max_bytes } | Self::TimeAndBytes { max_bytes, .. } => Some(max_bytes),
            Self::Time { .. } => None,
        }
    }

    /// Returns true when the class has a finite bound.
    #[must_use]
    pub const fn is_bounded(self) -> bool {
        self.time_bound().is_some() || self.byte_bound().is_some()
    }

    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Time { .. } => 1,
            Self::Bytes { .. } => 2,
            Self::TimeAndBytes { .. } => 3,
        }
    }

    pub(crate) fn encode(self, encoder: &mut Encoder) {
        encoder.u8(self.tag());
        if let Some(seconds) = self.time_bound() {
            encoder.u64(seconds);
        }
        if let Some(max_bytes) = self.byte_bound() {
            encoder.u64(max_bytes);
        }
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        match decoder.u8()? {
            1 => Ok(Self::time(decoder.u64()?)),
            2 => Ok(Self::bytes(decoder.u64()?)),
            3 => Ok(Self::time_and_bytes(decoder.u64()?, decoder.u64()?)),
            _ => Err(DecodeError),
        }
    }
}

/// The oldest position that retention still holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionFloor {
    position: EventPosition,
    oldest_commit_version: Option<CommitVersion>,
}

impl RetentionFloor {
    /// Creates a retention floor from the oldest position.
    #[must_use]
    pub const fn new(position: EventPosition) -> Self {
        Self {
            position,
            oldest_commit_version: None,
        }
    }

    /// Adds the oldest commit version that retention still holds.
    #[must_use]
    pub const fn with_commit_version(mut self, commit_version: CommitVersion) -> Self {
        self.oldest_commit_version = Some(commit_version);
        self
    }

    /// Returns the oldest position.
    #[must_use]
    pub const fn position(&self) -> &EventPosition {
        &self.position
    }

    /// Returns the oldest commit version.
    #[must_use]
    pub const fn oldest_commit_version(&self) -> Option<CommitVersion> {
        self.oldest_commit_version
    }

    /// Returns the smallest sequence in the position.
    #[must_use]
    pub fn sequence(&self) -> EventSequence {
        position_minimum(&self.position)
    }
}

/// Returns the smallest sequence in an event position.
#[must_use]
pub fn position_minimum(position: &EventPosition) -> EventSequence {
    match position {
        EventPosition::Compact(sequence) => *sequence,
        EventPosition::PerKey(entries) => entries
            .values()
            .copied()
            .min()
            .unwrap_or_else(|| EventSequence::new(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn every_retention_class_is_bounded() {
        let classes = [
            RetentionClass::time(60),
            RetentionClass::bytes(1024),
            RetentionClass::time_and_bytes(60, 1024),
        ];
        for class in classes {
            assert!(class.is_bounded());
        }
        assert_eq!(RetentionClass::time(60).time_bound(), Some(60));
        assert_eq!(RetentionClass::time(60).byte_bound(), None);
        assert_eq!(RetentionClass::bytes(1024).time_bound(), None);
        assert_eq!(RetentionClass::bytes(1024).byte_bound(), Some(1024));
        assert_eq!(
            RetentionClass::time_and_bytes(60, 1024).time_bound(),
            Some(60)
        );
        assert_eq!(
            RetentionClass::time_and_bytes(60, 1024).byte_bound(),
            Some(1024)
        );
    }

    #[test]
    fn the_retention_floor_uses_the_smallest_sequence() {
        let mut entries = BTreeMap::new();
        entries.insert(vec![b'a'], EventSequence::new(4));
        entries.insert(vec![b'b'], EventSequence::new(7));
        let floor = RetentionFloor::new(EventPosition::PerKey(entries));
        assert_eq!(floor.sequence(), EventSequence::new(4));
        assert_eq!(floor.oldest_commit_version(), None);
        assert_eq!(
            floor
                .with_commit_version(CommitVersion::new(3))
                .oldest_commit_version(),
            Some(CommitVersion::new(3))
        );
    }
}
