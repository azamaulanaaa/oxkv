//! The platform adapter boundary.
//!
//! An adapter supplies one capability. It does not decide a verdict and does
//! not shape a byte. The semantic core sees these traits only.
//!
//! The crate ships portable adapters for tests and for a single process. A
//! production native or browser adapter implements the same traits.

use core::cell::Cell;
use std::collections::BTreeMap;

/// The declared durability of one storage commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Durability {
    /// The data survives only while the process lives.
    Memory,
    /// The data survives a process restart.
    Process,
    /// The data survives a device restart.
    Device,
}

impl Durability {
    /// Returns the stable wire tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Memory => 0,
            Self::Process => 1,
            Self::Device => 2,
        }
    }
}

/// An error from a storage adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
    /// The storage is unavailable.
    #[error("The storage is unavailable.")]
    Unavailable,
    /// The stored bytes are corrupt.
    #[error("The stored bytes are corrupt.")]
    Corrupt,
    /// The adapter cannot commit the batch as one unit.
    #[error("The storage cannot commit the batch as one unit.")]
    BatchNotAtomic,
}

impl StorageError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "replication.tier_unavailable",
            Self::Corrupt => "format.corrupt_page",
            Self::BatchNotAtomic => "internal.invariant_violation",
        }
    }
}

/// One range scan request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanRequest<'a> {
    /// The inclusive lower key bound.
    pub lower: &'a [u8],
    /// The exclusive upper key bound.
    pub upper: &'a [u8],
    /// Return keys strictly after this key.
    pub after: Option<&'a [u8]>,
    /// The maximum number of records.
    pub limit: usize,
}

/// One range scan result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanPage {
    /// The records in ascending key order.
    pub records: Vec<(Vec<u8>, Vec<u8>)>,
    /// The next key, when more records remain.
    pub next: Option<Vec<u8>>,
}

/// One atomic storage batch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Batch {
    entries: Vec<BatchEntry>,
}

/// One entry in an atomic storage batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchEntry {
    /// Write a key and a value.
    Put {
        /// The key bytes.
        key: Vec<u8>,
        /// The value bytes.
        value: Vec<u8>,
    },
    /// Delete a key.
    Delete {
        /// The key bytes.
        key: Vec<u8>,
    },
}

impl Batch {
    /// Creates an empty batch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Adds a write.
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.entries.push(BatchEntry::Put { key, value });
    }

    /// Adds a delete.
    pub fn delete(&mut self, key: Vec<u8>) {
        self.entries.push(BatchEntry::Delete { key });
    }

    /// Returns the entries in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[BatchEntry] {
        &self.entries
    }

    /// Returns true when the batch has no entry.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The result of one atomic commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitOutcome {
    /// The number of applied entries.
    pub applied: u64,
    /// The declared durability of the commit.
    pub durability: Durability,
}

/// An ordered key-value storage adapter.
pub trait Storage {
    /// Returns one value.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the adapter cannot read the value.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    /// Returns one page of an ordered range scan.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the adapter cannot scan the range.
    fn range(&self, request: &ScanRequest<'_>) -> Result<ScanPage, StorageError>;

    /// Commits one batch as one atomic unit.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the adapter cannot commit the batch.
    fn commit(&mut self, batch: &Batch) -> Result<CommitOutcome, StorageError>;

    /// Returns the declared durability of this adapter.
    #[must_use]
    fn durability(&self) -> Durability;
}

/// A portable in-memory storage adapter.
///
/// The adapter applies a batch to a copy and swaps the copy on success. A
/// failed batch changes no key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryStorage {
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
    durability: Durability,
}

impl MemoryStorage {
    /// Creates an empty storage adapter.
    #[must_use]
    pub const fn new(durability: Durability) -> Self {
        Self {
            entries: BTreeMap::new(),
            durability,
        }
    }

    /// Creates a storage adapter from key-value pairs.
    #[must_use]
    pub fn from_entries(
        durability: Durability,
        entries: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
    ) -> Self {
        Self {
            entries: entries.into_iter().collect(),
            durability,
        }
    }

    /// Inserts one value without a batch.
    pub fn insert(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.entries.insert(key, value);
    }

    /// Returns the number of stored keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the adapter stores no key.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new(Durability::Memory)
    }
}

impl Storage for MemoryStorage {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self.entries.get(key).cloned())
    }

    fn range(&self, request: &ScanRequest<'_>) -> Result<ScanPage, StorageError> {
        let mut matching: Vec<(&Vec<u8>, &Vec<u8>)> = self
            .entries
            .iter()
            .filter(|(key, _)| {
                let key = key.as_slice();
                key >= request.lower
                    && key < request.upper
                    && request.after.is_none_or(|after| key > after)
            })
            .collect();
        let limit = request.limit;
        let next = if matching.len() > limit {
            matching.get(limit).map(|(key, _)| (*key).clone())
        } else {
            None
        };
        matching.truncate(limit);
        let records = matching
            .into_iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Ok(ScanPage { records, next })
    }

    fn commit(&mut self, batch: &Batch) -> Result<CommitOutcome, StorageError> {
        let mut next = self.entries.clone();
        for entry in batch.entries() {
            match entry {
                BatchEntry::Put { key, value } => {
                    next.insert(key.clone(), value.clone());
                }
                BatchEntry::Delete { key } => {
                    next.remove(key);
                }
            }
        }
        self.entries = next;
        Ok(CommitOutcome {
            applied: u64::try_from(batch.entries().len()).unwrap_or(u64::MAX),
            durability: self.durability,
        })
    }

    fn durability(&self) -> Durability {
        self.durability
    }
}

/// A clock adapter. The core never reads a system clock directly.
pub trait Clock {
    /// Returns the current time in milliseconds.
    fn now_millis(&self) -> u64;
}

/// A scripted clock for deterministic tests.
#[derive(Debug)]
pub struct ScriptedClock {
    values: Vec<u64>,
    cursor: Cell<usize>,
}

impl ScriptedClock {
    /// Creates a scripted clock. The last value repeats after the script.
    #[must_use]
    pub const fn new(values: Vec<u64>) -> Self {
        Self {
            values,
            cursor: Cell::new(0),
        }
    }

    /// Creates a clock with one fixed value.
    #[must_use]
    pub fn fixed(value: u64) -> Self {
        Self::new(vec![value])
    }
}

impl Clock for ScriptedClock {
    fn now_millis(&self) -> u64 {
        let index = self.cursor.get();
        let value = self
            .values
            .get(index)
            .copied()
            .or_else(|| self.values.last().copied())
            .unwrap_or(0);
        self.cursor.set(index.saturating_add(1));
        value
    }
}

/// An entropy adapter. The core never draws system randomness directly.
pub trait Entropy {
    /// Fills the output with entropy bytes.
    fn fill(&self, output: &mut [u8]);
}

/// A deterministic entropy adapter for tests.
///
/// The adapter uses a `u64` linear congruential generator. One seed produces
/// one byte sequence.
#[derive(Debug)]
pub struct DeterministicEntropy {
    state: Cell<u64>,
}

impl DeterministicEntropy {
    /// Creates an entropy adapter with one seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            state: Cell::new(seed),
        }
    }
}

impl Entropy for DeterministicEntropy {
    fn fill(&self, output: &mut [u8]) {
        let mut state = self.state.get();
        for chunk in output.chunks_mut(8) {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let bytes = state.to_be_bytes();
            let length = chunk.len();
            chunk.copy_from_slice(&bytes[..length]);
        }
        self.state.set(state);
    }
}

/// A scheduling adapter. It reports the number of independent lanes.
pub trait Scheduler {
    /// Returns the number of lanes. The value is at least one.
    fn lane_count(&self) -> usize;
}

/// A fixed lane-count scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedScheduler {
    lanes: usize,
}

impl FixedScheduler {
    /// Creates a scheduler with the given lane count.
    #[must_use]
    pub const fn new(lanes: usize) -> Self {
        Self {
            lanes: if lanes == 0 { 1 } else { lanes },
        }
    }
}

impl Scheduler for FixedScheduler {
    fn lane_count(&self) -> usize {
        self.lanes
    }
}

/// An error from a transport adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The transport is disconnected.
    #[error("The transport is disconnected.")]
    Disconnected,
    /// The transport rejected the frame size.
    #[error("The transport rejected the frame size.")]
    FrameTooLarge,
}

impl TransportError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Disconnected => "sync.shard_unavailable",
            Self::FrameTooLarge => "rpc.codec_unsupported",
        }
    }
}

/// A transport adapter. It moves bytes and does not decode them.
pub trait Transport {
    /// Sends one opaque frame and returns the response bytes.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] when the adapter cannot send the frame.
    fn send(&self, frame: &[u8]) -> Result<Vec<u8>, TransportError>;
}

/// A portable in-process transport. It returns the frame bytes unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InProcessTransport;

impl Transport for InProcessTransport {
    fn send(&self, frame: &[u8]) -> Result<Vec<u8>, TransportError> {
        Ok(frame.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_storage_scan_uses_key_order() {
        let storage = MemoryStorage::from_entries(
            Durability::Memory,
            [
                (vec![3], vec![30]),
                (vec![1], vec![10]),
                (vec![2], vec![20]),
            ],
        );
        let page = storage
            .range(&ScanRequest {
                lower: &[1],
                upper: &[4],
                after: None,
                limit: 2,
            })
            .expect("the scan succeeds");
        assert_eq!(page.records, vec![(vec![1], vec![10]), (vec![2], vec![20])]);
        assert_eq!(page.next, Some(vec![3]));
    }

    #[test]
    fn a_memory_storage_commit_is_atomic_on_success() {
        let mut storage = MemoryStorage::new(Durability::Device);
        let mut batch = Batch::new();
        batch.put(vec![1], vec![10]);
        batch.put(vec![2], vec![20]);
        let outcome = storage.commit(&batch).expect("the commit succeeds");
        assert_eq!(outcome.applied, 2);
        assert_eq!(outcome.durability, Durability::Device);
        assert_eq!(storage.get(&[1]), Ok(Some(vec![10])));
    }

    #[test]
    fn a_scripted_clock_repeats_the_last_value() {
        let clock = ScriptedClock::new(vec![5, 7]);
        assert_eq!(clock.now_millis(), 5);
        assert_eq!(clock.now_millis(), 7);
        assert_eq!(clock.now_millis(), 7);
    }

    #[test]
    fn deterministic_entropy_repeats_for_one_seed() {
        let first = DeterministicEntropy::new(42);
        let second = DeterministicEntropy::new(42);
        let mut left = [0_u8; 16];
        let mut right = [0_u8; 16];
        first.fill(&mut left);
        second.fill(&mut right);
        assert_eq!(left, right);
        assert_ne!(left, [0_u8; 16]);
    }

    #[test]
    fn the_in_process_transport_does_not_shape_the_frame() {
        let transport = InProcessTransport;
        assert_eq!(transport.send(b"frame"), Ok(b"frame".to_vec()));
    }
}
