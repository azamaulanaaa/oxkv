//! Per-key consumer cursors.
//!
//! The consumer cursor is contiguous per key. One unacknowledged event blocks
//! its key only. A per-shard cursor is not the 1.0 consumer contract.
//!
//! The contract token type is [`EventCursor`]. [`ConsumerCursor`] adds the
//! per-key acknowledgement operations that an effect consumer needs.

use std::collections::BTreeMap;

use oxkv_contracts::cursor::{EventCursor, EventPosition, EventSequence};

use crate::error::EventError;
use crate::retention::{RetentionFloor, position_minimum};

/// A per-key consumer cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerCursor {
    position: EventPosition,
}

impl ConsumerCursor {
    /// Creates an empty per-key cursor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            position: EventPosition::PerKey(BTreeMap::new()),
        }
    }

    /// Creates a compact watermark cursor.
    #[must_use]
    pub const fn compact(sequence: EventSequence) -> Self {
        Self {
            position: EventPosition::Compact(sequence),
        }
    }

    /// Returns the contract position.
    #[must_use]
    pub const fn position(&self) -> &EventPosition {
        &self.position
    }

    /// Returns the contract cursor token value.
    #[must_use]
    pub fn as_contract_cursor(&self) -> EventCursor {
        EventCursor::new(self.position.clone())
    }

    /// Returns true when the cursor uses one compact watermark.
    #[must_use]
    pub const fn is_compact(&self) -> bool {
        matches!(self.position, EventPosition::Compact(_))
    }

    /// Returns the next expected sequence for one key.
    #[must_use]
    pub fn next_sequence(&self, key: &[u8]) -> EventSequence {
        EventSequence::new(self.sequence_for(key).get().saturating_add(1))
    }

    /// Returns the last acknowledged sequence for one key.
    #[must_use]
    pub fn sequence_for(&self, key: &[u8]) -> EventSequence {
        match &self.position {
            EventPosition::Compact(sequence) => *sequence,
            EventPosition::PerKey(entries) => entries
                .get(key)
                .copied()
                .unwrap_or_else(|| EventSequence::new(0)),
        }
    }

    /// Acknowledges the next contiguous event for one key.
    ///
    /// The method returns true when the cursor advanced. A duplicate or
    /// non-contiguous acknowledgement does not advance the cursor.
    #[must_use]
    pub fn acknowledge(&mut self, key: &[u8], sequence: EventSequence) -> bool {
        match &mut self.position {
            EventPosition::Compact(current) => advance(current, sequence),
            EventPosition::PerKey(entries) => {
                let current = entries
                    .entry(key.to_vec())
                    .or_insert_with(|| EventSequence::new(0));
                advance(current, sequence)
            }
        }
    }

    /// Sets one key to a resume position after a snapshot backfill.
    pub fn seek(&mut self, key: &[u8], sequence: EventSequence) {
        if let EventPosition::PerKey(entries) = &mut self.position {
            entries.insert(key.to_vec(), sequence);
        }
    }

    /// Returns true when every cursor position is at or above the oldest
    /// retained position.
    #[must_use]
    pub fn is_readable_at(&self, oldest: &EventPosition) -> bool {
        position_minimum(&self.position) >= position_minimum(oldest)
    }

    /// Checks the cursor against the event retention floor.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::RetentionGap`] when the cursor is behind
    /// retention. The error carries the oldest retained position.
    pub fn ensure_readable(&self, floor: &RetentionFloor) -> Result<(), EventError> {
        if self.is_readable_at(floor.position()) {
            Ok(())
        } else {
            Err(EventError::RetentionGap {
                oldest: floor.position().clone(),
            })
        }
    }

    /// Checks the completion cursor against completion retention.
    ///
    /// # Errors
    ///
    /// Returns [`EventError::CursorExpired`] when the completion cursor is
    /// behind retention.
    pub fn ensure_completion(&self, oldest: &EventPosition) -> Result<(), EventError> {
        if self.is_readable_at(oldest) {
            Ok(())
        } else {
            Err(EventError::CursorExpired {
                oldest: oldest.clone(),
            })
        }
    }

    /// Returns the lag for one key against a high sequence.
    #[must_use]
    pub fn lag(&self, key: &[u8], high: EventSequence) -> u64 {
        high.get().saturating_sub(self.sequence_for(key).get())
    }

    /// Returns the per-key entries. The compact form returns `None`.
    #[must_use]
    pub const fn per_key(&self) -> Option<&BTreeMap<Vec<u8>, EventSequence>> {
        match &self.position {
            EventPosition::Compact(_) => None,
            EventPosition::PerKey(entries) => Some(entries),
        }
    }
}

impl Default for ConsumerCursor {
    fn default() -> Self {
        Self::new()
    }
}

fn advance(current: &mut EventSequence, sequence: EventSequence) -> bool {
    if sequence.get() <= current.get() {
        return false;
    }
    if sequence.get() != current.get().saturating_add(1) {
        return false;
    }
    *current = sequence;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgement_is_contiguous_per_key() {
        let mut cursor = ConsumerCursor::new();
        assert_eq!(cursor.next_sequence(b"a"), EventSequence::new(1));
        assert!(cursor.acknowledge(b"a", EventSequence::new(1)));
        assert_eq!(cursor.next_sequence(b"a"), EventSequence::new(2));
        assert!(!cursor.acknowledge(b"a", EventSequence::new(3)));
        assert_eq!(cursor.next_sequence(b"a"), EventSequence::new(2));
        assert!(!cursor.acknowledge(b"a", EventSequence::new(1)));
        assert_eq!(cursor.next_sequence(b"a"), EventSequence::new(2));
    }

    #[test]
    fn one_key_does_not_block_another() {
        let mut cursor = ConsumerCursor::new();
        assert!(cursor.acknowledge(b"a", EventSequence::new(1)));
        assert!(cursor.acknowledge(b"b", EventSequence::new(1)));
        assert!(cursor.acknowledge(b"b", EventSequence::new(2)));
        assert_eq!(cursor.sequence_for(b"a"), EventSequence::new(1));
        assert_eq!(cursor.sequence_for(b"b"), EventSequence::new(2));
    }

    #[test]
    fn a_compact_watermark_advances() {
        let mut cursor = ConsumerCursor::compact(EventSequence::new(4));
        assert_eq!(cursor.next_sequence(b"a"), EventSequence::new(5));
        assert!(cursor.acknowledge(b"a", EventSequence::new(5)));
        assert_eq!(cursor.next_sequence(b"b"), EventSequence::new(6));
    }

    #[test]
    fn a_cursor_behind_retention_returns_a_gap() {
        let mut entries = BTreeMap::new();
        entries.insert(b"a".to_vec(), EventSequence::new(2));
        entries.insert(b"b".to_vec(), EventSequence::new(4));
        let mut floor_entries = BTreeMap::new();
        floor_entries.insert(b"a".to_vec(), EventSequence::new(3));
        floor_entries.insert(b"b".to_vec(), EventSequence::new(3));
        let cursor = ConsumerCursor {
            position: EventPosition::PerKey(entries),
        };
        let floor = RetentionFloor::new(EventPosition::PerKey(floor_entries));
        assert_eq!(
            cursor
                .ensure_readable(&floor)
                .expect_err("the cursor is behind"),
            EventError::RetentionGap {
                oldest: floor.position().clone()
            }
        );
    }

    #[test]
    fn a_cursor_at_the_floor_is_readable() {
        let cursor = ConsumerCursor::compact(EventSequence::new(3));
        let floor = RetentionFloor::new(EventPosition::Compact(EventSequence::new(3)));
        assert!(cursor.ensure_readable(&floor).is_ok());
    }

    #[test]
    fn lag_is_the_distance_to_the_high_sequence() {
        let mut cursor = ConsumerCursor::new();
        cursor.seek(b"a", EventSequence::new(4));
        assert_eq!(cursor.lag(b"a", EventSequence::new(9)), 5);
        assert_eq!(cursor.lag(b"b", EventSequence::new(9)), 9);
    }
}
