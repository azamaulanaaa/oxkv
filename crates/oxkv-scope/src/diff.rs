//! Add and remove diff records.
//!
//! A diff interval applies at one scope version vector. Each add names the key,
//! the key version, and the byte count. Each remove names the key, the version,
//! and the reason. A policy change opens a new interval. An old interval keeps
//! its own policy version and bytes.

use std::collections::BTreeSet;

use oxkv_contracts::version::{
    CommitVersion, KeyVersion, PolicyVersion, ScopeIdentity, ScopeVersionVector, ShardId,
    VersionVector,
};

use crate::error::ScopeError;
use crate::version::ScopeVersion;

const DIFF_MAGIC: [u8; 4] = *b"OXD1";
const DIFF_FORMAT_VERSION: u8 = 1;

/// The reason that a record leaves a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RemoveReason {
    /// The record changed and no longer matches the policy.
    RecordChanged,
    /// The policy changed and the record no longer matches.
    PolicyChanged,
    /// The scope narrowed and the record is outside the new query.
    ScopeNarrowed,
}

impl RemoveReason {
    /// Returns the stable wire tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::RecordChanged => 1,
            Self::PolicyChanged => 2,
            Self::ScopeNarrowed => 3,
        }
    }

    /// Reads a reason from its wire tag.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::RecordChanged),
            2 => Some(Self::PolicyChanged),
            3 => Some(Self::ScopeNarrowed),
            _ => None,
        }
    }

    /// Returns the canonical reason name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RecordChanged => "record_changed",
            Self::PolicyChanged => "policy_changed",
            Self::ScopeNarrowed => "scope_narrowed",
        }
    }
}

/// One add record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AddRecord {
    /// The record key.
    pub key: Vec<u8>,
    /// The version of the record.
    pub key_version: KeyVersion,
    /// The logical byte count of the record.
    pub byte_len: u64,
}

/// One remove record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RemoveRecord {
    /// The record key.
    pub key: Vec<u8>,
    /// The version of the record that leaves.
    pub key_version: KeyVersion,
    /// The reason that the record leaves.
    pub reason: RemoveReason,
}

/// One diff record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DiffRecord {
    /// The record enters the scope.
    Add(AddRecord),
    /// The record leaves the scope.
    Remove(RemoveRecord),
}

impl DiffRecord {
    /// Creates an add record.
    #[must_use]
    pub fn add(key: Vec<u8>, key_version: KeyVersion, byte_len: u64) -> Self {
        Self::Add(AddRecord {
            key,
            key_version,
            byte_len,
        })
    }

    /// Creates a remove record.
    #[must_use]
    pub fn remove(key: Vec<u8>, key_version: KeyVersion, reason: RemoveReason) -> Self {
        Self::Remove(RemoveRecord {
            key,
            key_version,
            reason,
        })
    }

    /// Returns the record key.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        match self {
            Self::Add(record) => &record.key,
            Self::Remove(record) => &record.key,
        }
    }

    /// Returns the record version.
    #[must_use]
    pub fn key_version(&self) -> KeyVersion {
        match self {
            Self::Add(record) => record.key_version,
            Self::Remove(record) => record.key_version,
        }
    }

    /// Returns true for an add record.
    #[must_use]
    pub const fn is_add(&self) -> bool {
        matches!(self, Self::Add(_))
    }

    /// Returns true for a remove record.
    #[must_use]
    pub const fn is_remove(&self) -> bool {
        matches!(self, Self::Remove(_))
    }
}

/// The churn of one diff interval.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Churn {
    /// The interval sequence.
    pub interval: u64,
    /// The policy version of the interval.
    pub policy_version: PolicyVersion,
    /// The target scope version.
    pub version: ScopeVersionVector,
    /// The add count.
    pub adds: u64,
    /// The remove count.
    pub removes: u64,
}

impl Churn {
    /// Returns the total change count.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.adds.saturating_add(self.removes)
    }
}

/// One diff interval.
///
/// The interval carries the scope identity, the policy version, a base vector,
/// a target vector, and the records. The base vector is the version before the
/// interval. The target vector is the version after the interval.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiffInterval {
    scope_identity: ScopeIdentity,
    interval: u64,
    policy_version: PolicyVersion,
    base: ScopeVersionVector,
    target: ScopeVersionVector,
    records: Vec<DiffRecord>,
}

impl DiffInterval {
    /// Creates one diff interval.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::DuplicateDiffRecord`] when a key has two records.
    /// Returns [`ScopeError::VersionRegression`] when the target is below the
    /// base.
    pub fn new(
        scope_identity: ScopeIdentity,
        interval: u64,
        policy_version: PolicyVersion,
        base: ScopeVersionVector,
        target: ScopeVersionVector,
        records: Vec<DiffRecord>,
    ) -> Result<Self, ScopeError> {
        let mut keys = BTreeSet::new();
        for record in &records {
            if !keys.insert(record.key().to_vec()) {
                return Err(ScopeError::DuplicateDiffRecord {
                    key: record.key().to_vec(),
                });
            }
        }
        let mut version = ScopeVersion::from_vector(base.clone());
        version.advance_to(&target)?;
        Ok(Self {
            scope_identity,
            interval,
            policy_version,
            base,
            target,
            records,
        })
    }

    /// Returns the scope identity.
    #[must_use]
    pub const fn scope_identity(&self) -> ScopeIdentity {
        self.scope_identity
    }

    /// Returns the interval sequence.
    #[must_use]
    pub const fn interval(&self) -> u64 {
        self.interval
    }

    /// Returns the policy version of this interval.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the base scope version.
    #[must_use]
    pub const fn base(&self) -> &ScopeVersionVector {
        &self.base
    }

    /// Returns the target scope version.
    #[must_use]
    pub const fn target(&self) -> &ScopeVersionVector {
        &self.target
    }

    /// Returns the records.
    #[must_use]
    pub fn records(&self) -> &[DiffRecord] {
        &self.records
    }

    /// Returns the churn of this interval.
    #[must_use]
    pub fn churn(&self) -> Churn {
        let adds = self.records.iter().filter(|record| record.is_add()).count();
        let removes = self.records.len().saturating_sub(adds);
        Churn {
            interval: self.interval,
            policy_version: self.policy_version,
            version: self.target.clone(),
            adds: u64::try_from(adds).unwrap_or(u64::MAX),
            removes: u64::try_from(removes).unwrap_or(u64::MAX),
        }
    }

    /// Encodes the interval into canonical bytes.
    ///
    /// The bytes are stable across hosts and targets. They carry the interval
    /// policy version, so an old interval keeps its old meaning.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&DIFF_MAGIC);
        bytes.push(DIFF_FORMAT_VERSION);
        bytes.extend_from_slice(self.scope_identity.as_bytes());
        bytes.extend_from_slice(self.policy_version.as_bytes());
        push_u64(&mut bytes, self.interval);
        push_vector(&mut bytes, &self.base);
        push_vector(&mut bytes, &self.target);
        push_u64(
            &mut bytes,
            u64::try_from(self.records.len()).unwrap_or(u64::MAX),
        );
        for record in &self.records {
            match record {
                DiffRecord::Add(record) => {
                    bytes.push(1);
                    push_bytes(&mut bytes, &record.key);
                    push_u64(&mut bytes, record.key_version.get());
                    push_u64(&mut bytes, record.byte_len);
                }
                DiffRecord::Remove(record) => {
                    bytes.push(2);
                    push_bytes(&mut bytes, &record.key);
                    push_u64(&mut bytes, record.key_version.get());
                    bytes.push(record.reason.tag());
                }
            }
        }
        bytes
    }

    /// Decodes one interval from canonical bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::MalformedDiff`] for malformed bytes. Returns
    /// [`ScopeError::UnsupportedDiffVersion`] for an unknown byte format
    /// version.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ScopeError> {
        let mut reader = Reader::new(bytes);
        if reader.take(4)? != DIFF_MAGIC {
            return Err(ScopeError::MalformedDiff);
        }
        let format = reader.u8()?;
        if format != DIFF_FORMAT_VERSION {
            return Err(ScopeError::UnsupportedDiffVersion { found: format });
        }
        let scope_identity = ScopeIdentity::from_bytes(reader.hash()?);
        let policy_version = PolicyVersion::from_bytes(reader.hash()?);
        let interval = reader.u64()?;
        let base = reader.vector()?;
        let target = reader.vector()?;
        let count = reader.count()?;
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let tag = reader.u8()?;
            let key = reader.bytes()?.to_vec();
            let key_version = KeyVersion::new(reader.u64()?);
            let record = match tag {
                1 => DiffRecord::add(key, key_version, reader.u64()?),
                2 => {
                    let tag = reader.u8()?;
                    let reason = RemoveReason::from_tag(tag).ok_or(ScopeError::MalformedDiff)?;
                    DiffRecord::remove(key, key_version, reason)
                }
                _ => return Err(ScopeError::MalformedDiff),
            };
            records.push(record);
        }
        reader.finish()?;
        Self::new(
            scope_identity,
            interval,
            policy_version,
            base,
            target,
            records,
        )
    }
}

/// A log of diff intervals.
///
/// The log keeps every interval. A policy change opens a new interval. The log
/// does not reinterpret an old interval.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiffLog {
    scope_identity: ScopeIdentity,
    intervals: Vec<DiffInterval>,
}

impl DiffLog {
    /// Creates an empty diff log.
    #[must_use]
    pub const fn new(scope_identity: ScopeIdentity) -> Self {
        Self {
            scope_identity,
            intervals: Vec::new(),
        }
    }

    /// Appends one diff interval.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::IdentityMismatch`] for another scope identity.
    /// Returns [`ScopeError::DiffSequence`] for a gap in the interval sequence.
    /// Returns [`ScopeError::BaseVersionMismatch`] for a wrong base vector.
    /// Returns [`ScopeError::VersionRegression`] for a decreasing target.
    pub fn push(&mut self, interval: DiffInterval) -> Result<Churn, ScopeError> {
        if interval.scope_identity != self.scope_identity {
            return Err(ScopeError::IdentityMismatch {
                expected: self.scope_identity,
                found: interval.scope_identity,
            });
        }
        let expected = match self.intervals.last() {
            Some(last) => last
                .interval
                .checked_add(1)
                .ok_or(ScopeError::ByteCountOverflow)?,
            None => 1,
        };
        if interval.interval != expected {
            return Err(ScopeError::DiffSequence {
                expected,
                found: interval.interval,
            });
        }
        match self.intervals.last() {
            Some(last) => {
                if interval.base != last.target {
                    return Err(ScopeError::BaseVersionMismatch);
                }
            }
            None => {
                if !interval.base.is_empty() {
                    return Err(ScopeError::BaseVersionMismatch);
                }
            }
        }
        let mut version = ScopeVersion::from_vector(self.current_version());
        version.advance_to(&interval.target)?;
        let churn = interval.churn();
        self.intervals.push(interval);
        Ok(churn)
    }

    /// Returns the scope identity.
    #[must_use]
    pub const fn scope_identity(&self) -> ScopeIdentity {
        self.scope_identity
    }

    /// Returns every interval in apply order.
    #[must_use]
    pub fn intervals(&self) -> &[DiffInterval] {
        &self.intervals
    }

    /// Returns the last interval.
    #[must_use]
    pub fn last(&self) -> Option<&DiffInterval> {
        self.intervals.last()
    }

    /// Returns the current scope version.
    #[must_use]
    pub fn current_version(&self) -> ScopeVersionVector {
        self.intervals
            .last()
            .map_or_else(ScopeVersionVector::new, |interval| interval.target.clone())
    }

    /// Returns the current policy version.
    #[must_use]
    pub fn current_policy_version(&self) -> Option<PolicyVersion> {
        self.intervals
            .last()
            .map(|interval| interval.policy_version)
    }

    /// Returns the churn for every interval.
    #[must_use]
    pub fn churn_per_version(&self) -> Vec<Churn> {
        self.intervals.iter().map(DiffInterval::churn).collect()
    }
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    push_u64(bytes, u64::try_from(value.len()).unwrap_or(u64::MAX));
    bytes.extend_from_slice(value);
}

fn push_vector(bytes: &mut Vec<u8>, vector: &ScopeVersionVector) {
    push_u64(
        bytes,
        u64::try_from(vector.as_vector().len()).unwrap_or(u64::MAX),
    );
    for (shard_id, commit_version) in vector.as_vector().iter() {
        push_bytes(bytes, shard_id.as_bytes());
        push_u64(bytes, commit_version.get());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn finish(&self) -> Result<(), ScopeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(ScopeError::MalformedDiff)
        }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ScopeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(ScopeError::MalformedDiff)?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(ScopeError::MalformedDiff)?;
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, ScopeError> {
        let slice = self.take(1)?;
        Ok(slice[0])
    }

    fn u64(&mut self) -> Result<u64, ScopeError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(u64::from_be_bytes(bytes))
    }

    fn hash(&mut self) -> Result<[u8; 32], ScopeError> {
        let slice = self.take(32)?;
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(slice);
        Ok(bytes)
    }

    fn bytes(&mut self) -> Result<&'a [u8], ScopeError> {
        let length = self.u64()?;
        let length = usize::try_from(length).map_err(|_| ScopeError::MalformedDiff)?;
        self.take(length)
    }

    fn count(&mut self) -> Result<usize, ScopeError> {
        let count = self.u64()?;
        let count = usize::try_from(count).map_err(|_| ScopeError::MalformedDiff)?;
        if count > self.bytes.len().saturating_sub(self.position) {
            return Err(ScopeError::MalformedDiff);
        }
        Ok(count)
    }

    fn vector(&mut self) -> Result<ScopeVersionVector, ScopeError> {
        let count = self.count()?;
        let mut vector = VersionVector::new();
        for _ in 0..count {
            let shard_id = ShardId::new(self.bytes()?.to_vec());
            let commit_version = CommitVersion::new(self.u64()?);
            vector.set(shard_id, commit_version);
        }
        Ok(ScopeVersionVector::from_vector(vector))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ScopeIdentity {
        ScopeIdentity::from_bytes([1; 32])
    }

    fn vector(entries: &[(&[u8], u64)]) -> ScopeVersionVector {
        let mut vector = VersionVector::new();
        for (shard, version) in entries {
            vector.set(
                ShardId::new((*shard).to_vec()),
                CommitVersion::new(*version),
            );
        }
        ScopeVersionVector::from_vector(vector)
    }

    fn interval(policy_byte: u8, records: Vec<DiffRecord>) -> DiffInterval {
        DiffInterval::new(
            identity(),
            1,
            PolicyVersion::from_bytes([policy_byte; 32]),
            ScopeVersionVector::new(),
            vector(&[(&[1], 1)]),
            records,
        )
        .expect("the interval is valid")
    }

    #[test]
    fn a_remove_names_the_key_version_and_reason() {
        let interval = interval(
            1,
            vec![DiffRecord::remove(
                b"accounts/1".to_vec(),
                KeyVersion::new(7),
                RemoveReason::RecordChanged,
            )],
        );
        let DiffRecord::Remove(record) = &interval.records()[0] else {
            panic!("the record is a remove");
        };
        assert_eq!(record.key, b"accounts/1");
        assert_eq!(record.key_version, KeyVersion::new(7));
        assert_eq!(record.reason, RemoveReason::RecordChanged);
        assert_eq!(record.reason.name(), "record_changed");
    }

    #[test]
    fn a_diff_interval_round_trips_through_bytes() {
        let interval = interval(
            2,
            vec![
                DiffRecord::add(b"a".to_vec(), KeyVersion::new(1), 10),
                DiffRecord::remove(
                    b"b".to_vec(),
                    KeyVersion::new(2),
                    RemoveReason::ScopeNarrowed,
                ),
            ],
        );
        let bytes = interval.to_bytes();
        assert_eq!(DiffInterval::from_bytes(&bytes), Ok(interval));
    }

    #[test]
    fn a_duplicate_key_fails() {
        let result = DiffInterval::new(
            identity(),
            1,
            PolicyVersion::from_bytes([1; 32]),
            ScopeVersionVector::new(),
            vector(&[(&[1], 1)]),
            vec![
                DiffRecord::add(b"a".to_vec(), KeyVersion::new(1), 1),
                DiffRecord::remove(
                    b"a".to_vec(),
                    KeyVersion::new(2),
                    RemoveReason::RecordChanged,
                ),
            ],
        );
        assert!(matches!(
            result,
            Err(ScopeError::DuplicateDiffRecord { .. })
        ));
    }

    #[test]
    fn an_unknown_diff_version_fails() {
        let mut bytes = interval(1, Vec::new()).to_bytes();
        bytes[4] = 9;
        assert_eq!(
            DiffInterval::from_bytes(&bytes),
            Err(ScopeError::UnsupportedDiffVersion { found: 9 })
        );
    }

    #[test]
    fn a_policy_change_opens_a_new_interval_and_old_bytes_still_parse() {
        let mut log = DiffLog::new(identity());
        let first = interval(
            1,
            vec![DiffRecord::add(b"a".to_vec(), KeyVersion::new(1), 1)],
        );
        let first_bytes = first.to_bytes();
        log.push(first.clone()).expect("the first interval applies");
        let second = DiffInterval::new(
            identity(),
            2,
            PolicyVersion::from_bytes([2; 32]),
            log.current_version(),
            vector(&[(&[1], 2)]),
            vec![DiffRecord::remove(
                b"a".to_vec(),
                KeyVersion::new(2),
                RemoveReason::PolicyChanged,
            )],
        )
        .expect("the second interval is valid");
        log.push(second).expect("the second interval applies");
        assert_eq!(
            log.current_policy_version(),
            Some(PolicyVersion::from_bytes([2; 32]))
        );
        assert_eq!(
            DiffInterval::from_bytes(&first_bytes).expect("the old interval parses"),
            first
        );
        let churn = log.churn_per_version();
        assert_eq!(churn.len(), 2);
        assert_eq!(churn[0].adds, 1);
        assert_eq!(churn[1].removes, 1);
    }

    #[test]
    fn a_sequence_gap_fails() {
        let mut log = DiffLog::new(identity());
        let mut second = interval(1, Vec::new());
        second.interval = 2;
        assert_eq!(
            log.push(second),
            Err(ScopeError::DiffSequence {
                expected: 1,
                found: 2
            })
        );
    }
}
