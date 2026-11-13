//! The replicated log.
//!
//! The log is durable. A driver appends a record only after the record bytes
//! are durable. A crash keeps every appended record. A crash discards every
//! record that the driver did not append.
//!
//! Each record has a plain header with an explicit index, term, and length.
//! An operator can read the header without a decoder.

use core::fmt;
use std::collections::BTreeSet;

use oxkv_contracts::receipt::IdempotencyKey;
use oxkv_contracts::version::LeaseTerm;

use crate::error::ReplError;

/// The position of one record in the log.
///
/// Index zero means "no record". The first record has index one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogIndex(u64);

impl LogIndex {
    /// The empty position.
    pub const ZERO: Self = Self(0);

    /// Creates a log index.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns true for the empty position.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Returns the next index.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the index would overflow.
    pub fn next(self) -> Result<Self, ReplError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(ReplError::InvariantViolation {
                reason: "the log index overflowed",
            })
    }

    fn as_offset(self) -> Result<usize, ReplError> {
        usize::try_from(self.0).map_err(|_| ReplError::InvariantViolation {
            reason: "the log index does not fit the platform length",
        })
    }
}

impl fmt::Display for LogIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// The durable owner fence at the log boundary.
///
/// A write fence below the owner fence is stale. A higher write fence wins and
/// becomes the new owner fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnerFence {
    term: LeaseTerm,
}

impl Default for OwnerFence {
    fn default() -> Self {
        Self::new(LeaseTerm::new(0))
    }
}

impl OwnerFence {
    /// Creates an owner fence from a lease term.
    #[must_use]
    pub const fn new(term: LeaseTerm) -> Self {
        Self { term }
    }

    /// Returns the fence term.
    #[must_use]
    pub const fn term(self) -> LeaseTerm {
        self.term
    }

    /// Returns the integer fence value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.term.get()
    }
}

/// One entry in the replicated log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryPayload {
    /// A client data write.
    Data {
        /// The client idempotency key.
        idempotency: IdempotencyKey,
        /// The user key.
        key: Vec<u8>,
        /// The user value.
        value: Vec<u8>,
    },
    /// A membership change.
    Config {
        /// The complete member set after the change.
        members: Vec<u64>,
    },
    /// A no-op that carries a new term.
    ///
    /// A new leader appends one no-op. It lets the leader commit the entries
    /// of earlier terms without a client write.
    Noop,
}

impl EntryPayload {
    /// Returns the idempotency key for a data entry.
    #[must_use]
    pub const fn idempotency(&self) -> Option<&IdempotencyKey> {
        match self {
            Self::Data { idempotency, .. } => Some(idempotency),
            Self::Config { .. } | Self::Noop => None,
        }
    }

    /// Returns the encoded length of the payload in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> u64 {
        let length = match self {
            Self::Data {
                idempotency,
                key,
                value,
            } => idempotency
                .as_bytes()
                .len()
                .saturating_add(key.len())
                .saturating_add(value.len()),
            Self::Config { members } => members.len().saturating_mul(8),
            Self::Noop => 0,
        };
        u64::try_from(length).unwrap_or(u64::MAX)
    }
}

/// One log record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    index: LogIndex,
    term: LeaseTerm,
    payload: EntryPayload,
}

impl LogRecord {
    /// Creates a log record.
    #[must_use]
    pub const fn new(index: LogIndex, term: LeaseTerm, payload: EntryPayload) -> Self {
        Self {
            index,
            term,
            payload,
        }
    }

    /// Returns the log index.
    #[must_use]
    pub const fn index(&self) -> LogIndex {
        self.index
    }

    /// Returns the lease term.
    #[must_use]
    pub const fn term(&self) -> LeaseTerm {
        self.term
    }

    /// Returns the payload.
    #[must_use]
    pub const fn payload(&self) -> &EntryPayload {
        &self.payload
    }

    /// Returns the readable record header.
    ///
    /// The header has an explicit index, term, and payload length.
    #[must_use]
    pub fn header_line(&self) -> String {
        format!(
            "index={} term={} len={}",
            self.index,
            self.term,
            self.payload.encoded_len()
        )
    }
}

/// A contiguous durable log.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Log {
    records: Vec<LogRecord>,
    owner_fence: OwnerFence,
}

impl Log {
    /// Creates an empty log.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
            owner_fence: OwnerFence::new(LeaseTerm::new(0)),
        }
    }

    /// Returns the durable owner fence.
    #[must_use]
    pub const fn owner_fence(&self) -> OwnerFence {
        self.owner_fence
    }

    /// Returns the record count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns true for an empty log.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Returns every record.
    #[must_use]
    pub fn records(&self) -> &[LogRecord] {
        &self.records
    }

    /// Returns the last index, or zero for an empty log.
    #[must_use]
    pub fn last_index(&self) -> LogIndex {
        self.records
            .last()
            .map_or(LogIndex::ZERO, |record| record.index)
    }

    /// Returns the last term, or term zero for an empty log.
    #[must_use]
    pub fn last_term(&self) -> LeaseTerm {
        self.records
            .last()
            .map_or(LeaseTerm::new(0), |record| record.term)
    }

    /// Returns the term at one index.
    ///
    /// Index zero returns term zero. A missing index returns `None`.
    #[must_use]
    pub fn term_at(&self, index: LogIndex) -> Option<LeaseTerm> {
        if index.is_zero() {
            return Some(LeaseTerm::new(0));
        }
        self.record(index).map(LogRecord::term)
    }

    /// Returns the record at one index.
    #[must_use]
    pub fn record(&self, index: LogIndex) -> Option<&LogRecord> {
        if index.is_zero() {
            return None;
        }
        let offset = usize::try_from(index.get().checked_sub(1)?).ok()?;
        self.records.get(offset)
    }

    /// Appends one payload under one write fence and owner fence.
    ///
    /// The log is the durable boundary. A successful return means that the
    /// record bytes are durable. A write fence below the owner fence returns
    /// [`ReplError::StaleEpoch`]. A higher write fence becomes the owner fence.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] when the write fence is below the
    /// owner fence. Returns [`ReplError::InvariantViolation`] when the next
    /// index overflows.
    pub fn append(
        &mut self,
        term: LeaseTerm,
        owner_fence: OwnerFence,
        payload: EntryPayload,
    ) -> Result<LogRecord, ReplError> {
        let owner_fence = self.owner_fence.term().max(owner_fence.term());
        if term < owner_fence {
            return Err(ReplError::StaleEpoch {
                presented: term.get(),
                current: owner_fence.get(),
            });
        }
        if term > self.owner_fence.term() {
            self.owner_fence = OwnerFence::new(term);
        }
        let index = self.last_index().next()?;
        let record = LogRecord::new(index, term, payload);
        self.records.push(record.clone());
        Ok(record)
    }

    /// Appends one prepared record at the next index.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the record index is not
    /// the next index.
    pub fn append_record(&mut self, record: LogRecord) -> Result<(), ReplError> {
        if record.index != self.last_index().next()? {
            return Err(ReplError::InvariantViolation {
                reason: "the replicated record index is not contiguous",
            });
        }
        self.records.push(record);
        Ok(())
    }

    /// Removes every record at or after one index.
    ///
    /// Index zero clears the log.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the index does not fit
    /// the platform length.
    pub fn truncate_from(&mut self, index: LogIndex) -> Result<(), ReplError> {
        if index.is_zero() {
            self.records.clear();
            return Ok(());
        }
        let offset = index.as_offset()?.saturating_sub(1);
        self.records.truncate(offset);
        Ok(())
    }

    /// Returns the first data record with one idempotency key.
    #[must_use]
    pub fn find_idempotency(&self, key: &IdempotencyKey) -> Option<&LogRecord> {
        self.records
            .iter()
            .find(|record| record.payload.idempotency() == Some(key))
    }

    /// Returns every record after one index.
    #[must_use]
    pub fn entries_after(&self, index: LogIndex) -> Vec<LogRecord> {
        let offset = usize::try_from(index.get()).unwrap_or(usize::MAX);
        self.records
            .iter()
            .skip(offset)
            .cloned()
            .collect::<Vec<_>>()
    }

    /// Returns the commit version that a data record would receive.
    ///
    /// The version is the count of distinct idempotency keys at or before the
    /// given index. A record that is committed but not applied still has a
    /// defined version.
    #[must_use]
    pub fn commit_version_at(&self, index: LogIndex) -> u64 {
        let mut keys = BTreeSet::new();
        let mut version = 0_u64;
        for record in &self.records {
            if record.index > index {
                break;
            }
            if let Some(key) = record.payload.idempotency()
                && keys.insert(key.as_bytes().to_vec())
            {
                version = version.saturating_add(1);
            }
        }
        version
    }
}
