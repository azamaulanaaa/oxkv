//! Diff semantics and the reference applier.

use std::collections::BTreeMap;

use oxkv_contracts::cursor::ScopeGeneration;
use oxkv_contracts::version::{CommitVersion, ShardId};

use crate::codec::{CodecError, Decoder, Encoder};
use crate::error::ErrorCode;
use crate::types::{MessageSequence, SubscriptionId};

/// One change to one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOperation {
    /// The key holds a new value.
    Put(Vec<u8>),
    /// The key is a tombstone inside the scope.
    Delete,
    /// The key left the scope. The replica prunes it.
    LeaveScope,
}

impl DiffOperation {
    /// Returns the value for a put.
    #[must_use]
    pub fn value(&self) -> Option<&[u8]> {
        match self {
            Self::Put(value) => Some(value),
            Self::Delete | Self::LeaveScope => None,
        }
    }

    /// Returns the stable operation tag.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Put(_) => 1,
            Self::Delete => 2,
            Self::LeaveScope => 3,
        }
    }
}

/// One ordered diff entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    /// The subscription that owns the entry.
    pub subscription_id: SubscriptionId,
    /// The scope generation.
    pub scope_generation: ScopeGeneration,
    /// The gap-free stream sequence.
    pub sequence: MessageSequence,
    /// The shard that owns the key.
    pub shard_id: ShardId,
    /// The canonical key bytes.
    pub key: Vec<u8>,
    /// The change.
    pub operation: DiffOperation,
    /// The commit version of the entry.
    pub commit_version: CommitVersion,
}

impl DiffEntry {
    /// Returns true when the entry removes the key from the scope.
    #[must_use]
    pub const fn leaves_scope(&self) -> bool {
        matches!(self.operation, DiffOperation::LeaveScope)
    }

    /// Returns the key's canonical `(shard, key)` order value.
    #[must_use]
    pub fn order_key(&self) -> (&ShardId, &[u8]) {
        (&self.shard_id, &self.key)
    }

    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.u64(self.scope_generation.get());
        encoder.u64(self.sequence.get());
        encoder.bytes(self.shard_id.as_bytes());
        encoder.bytes(&self.key);
        encoder.u8(self.operation.tag());
        if let DiffOperation::Put(value) = &self.operation {
            encoder.bytes(value);
        }
        encoder.u64(self.commit_version.get());
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        let subscription_id = SubscriptionId::new(decoder.u64()?);
        let scope_generation = ScopeGeneration::new(decoder.u64()?);
        let sequence = MessageSequence::new(decoder.u64()?);
        let shard_id = ShardId::new(decoder.bytes()?.to_vec());
        let key = decoder.bytes()?.to_vec();
        let operation = match decoder.u8()? {
            1 => DiffOperation::Put(decoder.bytes()?.to_vec()),
            2 => DiffOperation::Delete,
            3 => DiffOperation::LeaveScope,
            _ => return Err(CodecError::InvalidTag),
        };
        let commit_version = CommitVersion::new(decoder.u64()?);
        Ok(Self {
            subscription_id,
            scope_generation,
            sequence,
            shard_id,
            key,
            operation,
            commit_version,
        })
    }
}

/// A bounded group of diff entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffBatch {
    /// The subscription that owns the batch.
    pub subscription_id: SubscriptionId,
    /// The entries in sync order.
    pub entries: Vec<DiffEntry>,
}

impl DiffBatch {
    /// Creates a diff batch.
    #[must_use]
    pub const fn new(subscription_id: SubscriptionId, entries: Vec<DiffEntry>) -> Self {
        Self {
            subscription_id,
            entries,
        }
    }

    /// Returns true when every entry has the batch subscription and the
    /// canonical `(shard, key)` order with no sequence gap.
    #[must_use]
    pub fn is_canonical(&self) -> bool {
        let mut previous: Option<(&ShardId, &[u8], u64)> = None;
        for entry in &self.entries {
            if entry.subscription_id != self.subscription_id {
                return false;
            }
            if let Some((shard_id, key, sequence)) = previous {
                if (&entry.shard_id, entry.key.as_slice()) < (shard_id, key) {
                    return false;
                }
                if entry.sequence.get() != sequence.saturating_add(1) {
                    return false;
                }
            }
            previous = Some((&entry.shard_id, entry.key.as_slice(), entry.sequence.get()));
        }
        true
    }
}

/// An error from the reference diff applier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffApplyError {
    /// The entry belongs to another subscription.
    WrongSubscription,
    /// The entry belongs to another scope generation.
    WrongGeneration,
    /// The stream skipped one or more sequences.
    Gap {
        /// The expected sequence.
        expected: u64,
        /// The received sequence.
        found: u64,
    },
}

impl DiffApplyError {
    /// Returns the frozen wire error code.
    #[must_use]
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::WrongSubscription | Self::WrongGeneration => ErrorCode::ScopeRejected,
            Self::Gap { .. } => ErrorCode::GapTooLarge,
        }
    }
}

/// The result of one diff apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The entry changed replica state.
    Applied,
    /// The entry was already applied. The call is idempotent.
    Duplicate,
}

/// The reference replica state for one subscription.
///
/// The applier rejects a sequence gap before it applies a later entry. Two
/// applies of one entry equal one apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffApplier {
    subscription_id: SubscriptionId,
    scope_generation: ScopeGeneration,
    next_sequence: u64,
    entries: BTreeMap<(ShardId, Vec<u8>), DiffOperation>,
}

impl DiffApplier {
    /// Creates an applier for one subscription.
    #[must_use]
    pub fn new(subscription_id: SubscriptionId, scope_generation: ScopeGeneration) -> Self {
        Self {
            subscription_id,
            scope_generation,
            next_sequence: 0,
            entries: BTreeMap::new(),
        }
    }

    /// Returns the next expected sequence.
    #[must_use]
    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Returns the number of retained keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the replica holds no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns one value. A tombstone or a leave-scope entry is absent.
    #[must_use]
    pub fn get(&self, shard_id: &ShardId, key: &[u8]) -> Option<&[u8]> {
        match self.entries.get(&(shard_id.clone(), key.to_vec())) {
            Some(DiffOperation::Put(value)) => Some(value),
            Some(DiffOperation::Delete | DiffOperation::LeaveScope) | None => None,
        }
    }

    /// Applies one entry.
    ///
    /// # Errors
    ///
    /// Returns [`DiffApplyError::Gap`] when the entry skips a sequence.
    /// Returns [`DiffApplyError::WrongSubscription`] or
    /// [`DiffApplyError::WrongGeneration`] when the entry belongs to another
    /// stream.
    pub fn apply(&mut self, entry: &DiffEntry) -> Result<ApplyOutcome, DiffApplyError> {
        if entry.subscription_id != self.subscription_id {
            return Err(DiffApplyError::WrongSubscription);
        }
        if entry.scope_generation != self.scope_generation {
            return Err(DiffApplyError::WrongGeneration);
        }
        if entry.sequence.get() < self.next_sequence {
            return Ok(ApplyOutcome::Duplicate);
        }
        if entry.sequence.get() > self.next_sequence {
            return Err(DiffApplyError::Gap {
                expected: self.next_sequence,
                found: entry.sequence.get(),
            });
        }
        let key = (entry.shard_id.clone(), entry.key.clone());
        match &entry.operation {
            DiffOperation::Put(value) => {
                self.entries.insert(key, DiffOperation::Put(value.clone()));
            }
            DiffOperation::Delete | DiffOperation::LeaveScope => {
                self.entries.remove(&key);
            }
        }
        self.next_sequence = self.next_sequence.saturating_add(1);
        Ok(ApplyOutcome::Applied)
    }

    /// Applies a batch in order.
    ///
    /// # Errors
    ///
    /// Returns the first apply error.
    pub fn apply_batch(&mut self, batch: &DiffBatch) -> Result<(), DiffApplyError> {
        for entry in &batch.entries {
            self.apply(entry)?;
        }
        Ok(())
    }
}

/// Returns true when the key starts with one declared scope prefix.
#[must_use]
pub fn key_in_scope(key: &[u8], prefixes: &[Vec<u8>]) -> bool {
    prefixes.iter().any(|prefix| key.starts_with(prefix))
}
