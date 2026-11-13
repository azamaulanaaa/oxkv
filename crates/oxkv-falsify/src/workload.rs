//! The deterministic workload model.
//!
//! [`WorkloadSpec`] declares the dataset size, selectivity, rule count, fact
//! count, client count, shard map, policy version, concurrency schedule,
//! trace, axes, and metrics.
//!
//! [`Workload::replay`] derives every operation from a fixed [`Seed`] and a
//! [`LogicalClock`]. The model reads no wall clock. It draws no operating
//! system entropy. One spec therefore produces one operation hash on every
//! target.

use core::fmt;

use oxkv_contracts::version::{Hash32, PolicyVersion, ShardId};
use thiserror::Error;

use crate::falsifier::Falsifier;

/// A fixed workload seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Seed(u64);

impl Seed {
    /// Creates a seed.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the seed value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Seed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// A logical clock for one replay.
///
/// The clock counts logical ticks. It has no relation to a wall clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogicalClock {
    tick: u64,
}

impl LogicalClock {
    /// Creates a clock at tick zero.
    #[must_use]
    pub const fn new() -> Self {
        Self { tick: 0 }
    }

    /// Creates a clock at the given tick.
    #[must_use]
    pub const fn from_tick(tick: u64) -> Self {
        Self { tick }
    }

    /// Returns the current tick.
    #[must_use]
    pub const fn now(self) -> u64 {
        self.tick
    }

    /// Advances the clock and returns the new tick.
    ///
    /// The clock saturates at the maximum tick. It never goes backward.
    pub fn advance(&mut self, ticks: u64) -> u64 {
        self.tick = self.tick.saturating_add(ticks);
        self.tick
    }
}

/// A deterministic `SplitMix64` generator.
///
/// The generator seeds from a [`Seed`] value. It never reads operating-system
/// entropy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Creates a generator from a seed.
    #[must_use]
    pub const fn new(seed: Seed) -> Self {
        Self { state: seed.get() }
    }

    /// Returns the next value in the sequence.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

/// One axis that a workload sweeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Axis {
    /// The dataset row count.
    DatasetRows,
    /// The selectivity of the scope.
    Selectivity,
    /// The rule count.
    Rules,
    /// The fact count.
    Facts,
    /// The client count.
    Clients,
    /// The scope byte count.
    ScopeBytes,
}

impl Axis {
    /// Every axis.
    pub const ALL: [Self; 6] = [
        Self::DatasetRows,
        Self::Selectivity,
        Self::Rules,
        Self::Facts,
        Self::Clients,
        Self::ScopeBytes,
    ];

    /// Returns the stable axis name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DatasetRows => "dataset_rows",
            Self::Selectivity => "selectivity",
            Self::Rules => "rules",
            Self::Facts => "facts",
            Self::Clients => "clients",
            Self::ScopeBytes => "scope_bytes",
        }
    }

    /// Returns the stable axis tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::DatasetRows => 1,
            Self::Selectivity => 2,
            Self::Rules => 3,
            Self::Facts => 4,
            Self::Clients => 5,
            Self::ScopeBytes => 6,
        }
    }
}

impl fmt::Display for Axis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The source of one workload trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TraceKind {
    /// A synthetic generator.
    Synthetic,
    /// A scrubbed replay trace.
    ScrubbedReplay,
}

impl TraceKind {
    /// Returns the stable trace name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::ScrubbedReplay => "scrubbed_replay",
        }
    }

    /// Returns the stable trace tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Synthetic => 1,
            Self::ScrubbedReplay => 2,
        }
    }
}

impl fmt::Display for TraceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A concurrency schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConcurrencySchedule {
    /// The number of write bursts.
    pub bursts: u32,
    /// The number of writes in one burst for one client.
    pub writes_per_burst: u32,
}

impl ConcurrencySchedule {
    /// Creates a concurrency schedule.
    #[must_use]
    pub const fn new(bursts: u32, writes_per_burst: u32) -> Self {
        Self {
            bursts,
            writes_per_burst,
        }
    }

    /// Returns the operation count for a client count.
    ///
    /// Returns `None` when the count does not fit in `u64`.
    #[must_use]
    pub fn operation_count(self, clients: u32) -> Option<u64> {
        u64::from(clients)
            .checked_mul(u64::from(self.bursts))?
            .checked_mul(u64::from(self.writes_per_burst))
    }
}

/// The declared shard map of one workload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShardMap {
    shards: Vec<ShardId>,
}

impl ShardMap {
    /// Creates a shard map.
    ///
    /// # Errors
    ///
    /// Returns [`WorkloadError::EmptyShardMap`] when the list is empty.
    /// Returns [`WorkloadError::TooManyShards`] when the list has more than
    /// `u32::MAX` entries. Returns [`WorkloadError::DuplicateShard`] when one
    /// shard identifier appears more than one time.
    pub fn new(shards: Vec<ShardId>) -> Result<Self, WorkloadError> {
        if shards.is_empty() {
            return Err(WorkloadError::EmptyShardMap);
        }
        if u32::try_from(shards.len()).is_err() {
            return Err(WorkloadError::TooManyShards);
        }
        for (index, shard) in shards.iter().enumerate() {
            if shards[..index].contains(shard) {
                return Err(WorkloadError::DuplicateShard);
            }
        }
        Ok(Self { shards })
    }

    fn from_parts(shards: Vec<ShardId>) -> Self {
        Self { shards }
    }

    /// Returns the shard identifiers.
    #[must_use]
    pub fn shards(&self) -> &[ShardId] {
        &self.shards
    }

    /// Returns the shard count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shards.len()
    }

    /// Returns true when the map has no shard.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shards.is_empty()
    }
}

/// One deterministic workload declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadSpec {
    /// The fixed seed.
    pub seed: Seed,
    /// The dataset row count.
    pub dataset_rows: u64,
    /// The scope selectivity in parts per million.
    pub selectivity_ppm: u32,
    /// The rule count.
    pub rules: u32,
    /// The fact count.
    pub facts: u32,
    /// The client count.
    pub clients: u32,
    /// The byte size of one matching row.
    pub row_bytes: u64,
    /// The declared shard map.
    pub shard_map: ShardMap,
    /// The pinned policy version.
    pub policy_version: PolicyVersion,
    /// The concurrency schedule.
    pub concurrency: ConcurrencySchedule,
    /// The trace source.
    pub trace: TraceKind,
    /// The swept axes.
    pub axes: Vec<Axis>,
    /// The measured falsifiers.
    pub metrics: Vec<Falsifier>,
}

impl WorkloadSpec {
    /// Returns the candidate spec for the falsifier suite.
    #[must_use]
    pub fn candidate() -> Self {
        Self {
            seed: Seed::new(0x0F1A_2B3C_4D5E_6F70),
            dataset_rows: 1_000_000,
            selectivity_ppm: 10_000,
            rules: 50,
            facts: 10_000,
            clients: 64,
            row_bytes: 128,
            shard_map: ShardMap::from_parts(vec![
                ShardId::new(vec![1]),
                ShardId::new(vec![2]),
                ShardId::new(vec![3]),
                ShardId::new(vec![4]),
            ]),
            policy_version: PolicyVersion::from_bytes([0x11; 32]),
            concurrency: ConcurrencySchedule::new(4, 8),
            trace: TraceKind::Synthetic,
            axes: vec![Axis::DatasetRows, Axis::Selectivity, Axis::Clients],
            metrics: Falsifier::ALL.to_vec(),
        }
    }

    /// Validates the spec.
    ///
    /// # Errors
    ///
    /// Returns [`WorkloadError`] when a declared value is out of range or a
    /// required declaration is absent.
    pub fn validate(&self) -> Result<(), WorkloadError> {
        if self.dataset_rows == 0 {
            return Err(WorkloadError::EmptyDataset);
        }
        if self.selectivity_ppm > 1_000_000 {
            return Err(WorkloadError::SelectivityOutOfRange);
        }
        if self.clients == 0
            || self.concurrency.bursts == 0
            || self.concurrency.writes_per_burst == 0
        {
            return Err(WorkloadError::EmptySchedule);
        }
        if self.shard_map.is_empty() {
            return Err(WorkloadError::EmptyShardMap);
        }
        if self.axes.is_empty() {
            return Err(WorkloadError::EmptyAxes);
        }
        for falsifier in Falsifier::ALL {
            if !self.metrics.contains(&falsifier) {
                return Err(WorkloadError::MissingMetric(falsifier));
            }
        }
        if self.concurrency.operation_count(self.clients).is_none() {
            return Err(WorkloadError::SizeOverflow);
        }
        Ok(())
    }

    /// Returns the canonical byte form of the spec.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_bytes(&mut bytes, b"oxkv-falsify/workload/v1");
        push_u64(&mut bytes, self.seed.get());
        push_u64(&mut bytes, self.dataset_rows);
        push_u32(&mut bytes, self.selectivity_ppm);
        push_u32(&mut bytes, self.rules);
        push_u32(&mut bytes, self.facts);
        push_u32(&mut bytes, self.clients);
        push_u64(&mut bytes, self.row_bytes);
        push_u32(
            &mut bytes,
            u32::try_from(self.shard_map.len()).unwrap_or(u32::MAX),
        );
        for shard in self.shard_map.shards() {
            push_bytes(&mut bytes, shard.as_bytes());
        }
        bytes.extend_from_slice(self.policy_version.as_bytes());
        push_u32(&mut bytes, self.concurrency.bursts);
        push_u32(&mut bytes, self.concurrency.writes_per_burst);
        bytes.push(self.trace.tag());
        push_u32(
            &mut bytes,
            u32::try_from(self.axes.len()).unwrap_or(u32::MAX),
        );
        for axis in &self.axes {
            bytes.push(axis.tag());
        }
        push_u32(
            &mut bytes,
            u32::try_from(self.metrics.len()).unwrap_or(u32::MAX),
        );
        for metric in &self.metrics {
            bytes.push(u8::try_from(metric.index()).unwrap_or(u8::MAX));
        }
        bytes
    }

    /// Returns the content hash of the canonical spec bytes.
    #[must_use]
    pub fn hash(&self) -> Hash32 {
        Hash32::of(&self.canonical_bytes())
    }
}

/// One generated operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Operation {
    /// The logical tick at which the operation starts.
    pub tick: u64,
    /// The client that submits the operation.
    pub client: u32,
    /// The deterministic key.
    pub key: u64,
    /// The target shard index.
    pub shard_index: u32,
    /// The operation kind.
    pub kind: OperationKind,
}

/// The kind of one generated operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OperationKind {
    /// A write.
    Write,
    /// A point read.
    Read,
    /// A search read.
    Search,
}

impl OperationKind {
    /// Returns the stable operation name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Read => "read",
            Self::Search => "search",
        }
    }

    /// Returns the stable operation tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Write => 1,
            Self::Read => 2,
            Self::Search => 3,
        }
    }
}

impl fmt::Display for OperationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One generated workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workload {
    spec_hash: Hash32,
    rows: u64,
    matching_rows: u64,
    matching_bytes: u64,
    logical_ticks: u64,
    operations: Vec<Operation>,
    operation_hash: Hash32,
}

impl Workload {
    /// Replays one spec into a deterministic workload.
    ///
    /// # Errors
    ///
    /// Returns [`WorkloadError`] when the spec is invalid or a size does not
    /// fit.
    pub fn replay(spec: &WorkloadSpec) -> Result<Self, WorkloadError> {
        spec.validate()?;
        let matching = u128::from(spec.dataset_rows) * u128::from(spec.selectivity_ppm) / 1_000_000;
        let matching_rows = u64::try_from(matching).map_err(|_| WorkloadError::SizeOverflow)?;
        let matching_bytes = matching_rows
            .checked_mul(spec.row_bytes)
            .ok_or(WorkloadError::SizeOverflow)?;
        let operation_count = spec
            .concurrency
            .operation_count(spec.clients)
            .ok_or(WorkloadError::SizeOverflow)?;
        let capacity = usize::try_from(operation_count).map_err(|_| WorkloadError::SizeOverflow)?;

        let mut generator = SplitMix64::new(spec.seed);
        let mut clock = LogicalClock::new();
        let mut operations = Vec::with_capacity(capacity);
        let shard_count = u32::try_from(spec.shard_map.len()).unwrap_or(u32::MAX);
        for burst in 0..spec.concurrency.bursts {
            for client in 0..spec.clients {
                for slot in 0..spec.concurrency.writes_per_burst {
                    let tick = clock.advance(1);
                    let key = zipf_like(&mut generator, spec.dataset_rows);
                    let shard_index = u32::try_from(key % u64::from(shard_count)).unwrap_or(0);
                    let kind = match (burst ^ slot ^ client) % 5 {
                        0 => OperationKind::Read,
                        1 => OperationKind::Search,
                        _ => OperationKind::Write,
                    };
                    operations.push(Operation {
                        tick,
                        client,
                        key,
                        shard_index,
                        kind,
                    });
                }
            }
        }

        let spec_hash = spec.hash();
        let operation_hash = hash_operations(spec_hash, &operations);
        Ok(Self {
            spec_hash,
            rows: spec.dataset_rows,
            matching_rows,
            matching_bytes,
            logical_ticks: clock.now(),
            operations,
            operation_hash,
        })
    }

    /// Returns the spec hash.
    #[must_use]
    pub const fn spec_hash(&self) -> Hash32 {
        self.spec_hash
    }

    /// Returns the dataset row count.
    #[must_use]
    pub const fn rows(&self) -> u64 {
        self.rows
    }

    /// Returns the matching row count.
    #[must_use]
    pub const fn matching_rows(&self) -> u64 {
        self.matching_rows
    }

    /// Returns the encoded byte count of the matching rows.
    #[must_use]
    pub const fn matching_bytes(&self) -> u64 {
        self.matching_bytes
    }

    /// Returns the final logical tick.
    #[must_use]
    pub const fn logical_ticks(&self) -> u64 {
        self.logical_ticks
    }

    /// Returns the generated operations.
    #[must_use]
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// Returns the operation hash.
    #[must_use]
    pub const fn operation_hash(&self) -> Hash32 {
        self.operation_hash
    }
}

fn zipf_like(generator: &mut SplitMix64, rows: u64) -> u64 {
    let first = generator.next_u64() % rows;
    let second = generator.next_u64() % rows;
    first.min(second)
}

fn hash_operations(spec_hash: Hash32, operations: &[Operation]) -> Hash32 {
    let mut bytes = Vec::with_capacity(40 + operations.len() * 21);
    push_bytes(&mut bytes, spec_hash.as_bytes());
    push_u64(
        &mut bytes,
        u64::try_from(operations.len()).unwrap_or(u64::MAX),
    );
    for operation in operations {
        push_u64(&mut bytes, operation.tick);
        push_u32(&mut bytes, operation.client);
        push_u64(&mut bytes, operation.key);
        push_u32(&mut bytes, operation.shard_index);
        bytes.push(operation.kind.tag());
    }
    Hash32::of(&bytes)
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    push_u32(bytes, u32::try_from(value.len()).unwrap_or(u32::MAX));
    bytes.extend_from_slice(value);
}

/// An error from a workload declaration or replay.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkloadError {
    /// The dataset has no row.
    #[error("The dataset size is zero.")]
    EmptyDataset,
    /// The selectivity is above one.
    #[error("The selectivity is above one million parts per million.")]
    SelectivityOutOfRange,
    /// The shard map has no shard.
    #[error("The shard map is empty.")]
    EmptyShardMap,
    /// The shard map has a duplicate identifier.
    #[error("The shard map has a duplicate shard identifier.")]
    DuplicateShard,
    /// The shard map has too many entries.
    #[error("The shard map has too many entries.")]
    TooManyShards,
    /// The client count or the concurrency schedule is zero.
    #[error("The client count or the concurrency schedule is zero.")]
    EmptySchedule,
    /// The axis list is empty.
    #[error("The axis list is empty.")]
    EmptyAxes,
    /// One required falsifier is absent from the metric list.
    #[error("The workload spec does not declare the {0} falsifier.")]
    MissingMetric(Falsifier),
    /// A size does not fit in the target integer type.
    #[error("A declared size does not fit.")]
    SizeOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_candidate_spec_is_valid_and_hashes_stably() {
        let spec = WorkloadSpec::candidate();
        spec.validate().expect("the candidate spec is valid");
        assert_eq!(spec.hash(), spec.hash());
        assert_eq!(spec.canonical_bytes(), spec.canonical_bytes());
    }

    #[test]
    fn one_spec_replays_to_one_operation_hash() {
        let spec = WorkloadSpec::candidate();
        let first = Workload::replay(&spec).expect("the first replay succeeds");
        let second = Workload::replay(&spec).expect("the second replay succeeds");
        assert_eq!(first.operations(), second.operations());
        assert_eq!(first.operation_hash(), second.operation_hash());
        assert_eq!(first.spec_hash(), second.spec_hash());
        assert_eq!(first.logical_ticks(), first.operations().len() as u64);
    }

    #[test]
    fn the_logical_clock_is_monotonic() {
        let mut clock = LogicalClock::new();
        assert_eq!(clock.now(), 0);
        assert_eq!(clock.advance(2), 2);
        assert_eq!(clock.advance(0), 2);
        assert_eq!(clock.advance(3), 5);
        assert_eq!(clock.now(), 5);
        let mut high = LogicalClock::from_tick(u64::MAX);
        assert_eq!(high.advance(1), u64::MAX);
    }

    #[test]
    fn the_generator_is_stable_and_seeded() {
        let mut first = SplitMix64::new(Seed::new(7));
        let mut second = SplitMix64::new(Seed::new(7));
        let mut third = SplitMix64::new(Seed::new(8));
        let left: Vec<u64> = (0..8).map(|_| first.next_u64()).collect();
        let right: Vec<u64> = (0..8).map(|_| second.next_u64()).collect();
        assert_eq!(left, right);
        assert_ne!(left[0], third.next_u64());
    }

    #[test]
    fn the_matching_rows_follow_the_selectivity() {
        let spec = WorkloadSpec::candidate();
        let workload = Workload::replay(&spec).expect("the replay succeeds");
        assert_eq!(workload.rows(), 1_000_000);
        assert_eq!(workload.matching_rows(), 10_000);
        assert_eq!(workload.matching_bytes(), 1_280_000);
    }

    #[test]
    fn an_invalid_spec_is_rejected() {
        let mut spec = WorkloadSpec::candidate();
        spec.selectivity_ppm = 1_000_001;
        assert_eq!(spec.validate(), Err(WorkloadError::SelectivityOutOfRange));
        spec.selectivity_ppm = 10_000;
        spec.clients = 0;
        assert_eq!(spec.validate(), Err(WorkloadError::EmptySchedule));
        spec.clients = 64;
        spec.metrics.pop();
        assert_eq!(
            spec.validate(),
            Err(WorkloadError::MissingMetric(Falsifier::ReadP99))
        );
    }

    #[test]
    fn a_duplicate_shard_is_rejected() {
        let result = ShardMap::new(vec![ShardId::new(vec![1]), ShardId::new(vec![1])]);
        assert_eq!(result, Err(WorkloadError::DuplicateShard));
        assert_eq!(ShardMap::new(Vec::new()), Err(WorkloadError::EmptyShardMap));
    }
}
