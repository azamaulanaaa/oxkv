//! Scheduler state and the three request states.
//!
//! A request has exactly one state. The state is `queued`, `in_flight`, or
//! `terminal`. A key has at most one request in flight. A key may have many
//! queued requests.
//!
//! The scheduler never writes a receipt state. It writes only its own request
//! state. The commit path owns every terminal receipt transition.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use oxkv_contracts::InternalDiagnostic;
use oxkv_contracts::receipt::RequestId;
use oxkv_contracts::version::{Hash32, ShardId};

/// The state of one request in the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RequestState {
    /// The request waits for its key and its dependencies.
    Queued,
    /// The request is selected for commit. The commit path owns it.
    InFlight,
    /// The request is resolved. The commit path owns the receipt verdict.
    Terminal,
}

impl RequestState {
    /// Every request state.
    pub const ALL: [Self; 3] = [Self::Queued, Self::InFlight, Self::Terminal];

    /// Returns the canonical state name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InFlight => "in_flight",
            Self::Terminal => "terminal",
        }
    }

    /// Returns true when the request is terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal)
    }
}

impl fmt::Display for RequestState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The canonical bytes of one key.
///
/// The caller supplies the bytes from key encoding v1. The scheduler does not
/// decode or normalize a key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId(Vec<u8>);

impl KeyId {
    /// Creates a key identity.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Returns the canonical key bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for KeyId {
    fn from(bytes: Vec<u8>) -> Self {
        Self::new(bytes)
    }
}

/// An arrival sequence number within one shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Arrival(u64);

impl Arrival {
    /// Creates an arrival sequence number.
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

impl fmt::Display for Arrival {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// The admission limits of one scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerConfig {
    /// The maximum number of dependency edges on one request.
    pub max_fan_in: usize,
}

/// The default maximum fan-in of one request.
pub const DEFAULT_MAX_FAN_IN: usize = 8;

impl SchedulerConfig {
    /// Creates a configuration.
    #[must_use]
    pub const fn new(max_fan_in: usize) -> Self {
        Self { max_fan_in }
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FAN_IN)
    }
}

/// The pure scheduler state.
///
/// The state uses ordered maps and ordered sets. Iteration order is stable.
/// Equal state produces equal decisions for an equal budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerState {
    pub(crate) shards: BTreeMap<ShardId, ShardState>,
    pub(crate) epoch: u64,
    pub(crate) config: SchedulerConfig,
}

impl SchedulerState {
    /// Creates an empty scheduler with default limits.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(SchedulerConfig::default())
    }

    /// Creates an empty scheduler with explicit limits.
    #[must_use]
    pub fn with_config(config: SchedulerConfig) -> Self {
        Self {
            shards: BTreeMap::new(),
            epoch: 0,
            config,
        }
    }

    /// Returns the admission limits.
    #[must_use]
    pub const fn config(&self) -> SchedulerConfig {
        self.config
    }

    /// Returns true when no request is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending_count() == 0
    }

    /// Returns the number of shards in the state.
    #[must_use]
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// Returns the number of non-terminal requests in all shards.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.shards
            .values()
            .map(|shard| {
                shard
                    .requests
                    .values()
                    .filter(|record| !record.state.is_terminal())
                    .count()
            })
            .sum()
    }

    /// Returns the number of ready requests in one shard.
    #[must_use]
    pub fn ready_count(&self, shard_id: &ShardId) -> usize {
        self.shards
            .get(shard_id)
            .map_or(0, |shard| shard.ready.len())
    }

    /// Returns the number of in-flight requests on one key.
    #[must_use]
    pub fn in_flight_count(&self, shard_id: &ShardId, key: &KeyId) -> usize {
        self.shards.get(shard_id).map_or(0, |shard| {
            shard
                .requests
                .values()
                .filter(|record| record.key == *key && record.state == RequestState::InFlight)
                .count()
        })
    }

    /// Returns the state of one request.
    #[must_use]
    pub fn request_state(&self, shard_id: &ShardId, request_id: RequestId) -> Option<RequestState> {
        let shard = self.shards.get(shard_id)?;
        let arrival = shard.by_id.get(&request_id)?;
        shard.requests.get(arrival).map(|record| record.state)
    }

    /// Returns the arrival of one request.
    #[must_use]
    pub fn request_arrival(&self, shard_id: &ShardId, request_id: RequestId) -> Option<Arrival> {
        let shard = self.shards.get(shard_id)?;
        shard.by_id.get(&request_id).copied()
    }

    /// Returns true when the shard is blocked.
    #[must_use]
    pub fn shard_blocked(&self, shard_id: &ShardId) -> bool {
        self.shards.get(shard_id).is_some_and(|shard| shard.blocked)
    }

    /// Blocks or unblocks one shard.
    ///
    /// A blocked shard keeps its queues. The scheduler selects no request from
    /// a blocked shard.
    pub fn set_shard_blocked(&mut self, shard_id: &ShardId, blocked: bool) {
        self.shards.entry(shard_id.clone()).or_default().blocked = blocked;
    }

    /// Blocks one shard.
    pub fn block_shard(&mut self, shard_id: &ShardId) {
        self.set_shard_blocked(shard_id, true);
    }

    /// Unblocks one shard.
    pub fn unblock_shard(&mut self, shard_id: &ShardId) {
        self.set_shard_blocked(shard_id, false);
    }

    /// Returns a stable digest of the scheduler state.
    ///
    /// The digest covers the configuration, the epoch, the blocked flags, the
    /// arrival counters, and every request record. The digest does not cover
    /// the derived maps. Those maps follow from the records.
    #[must_use]
    pub fn digest(&self) -> Hash32 {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"oxkv/scheduler-state/v1\0");
        push_usize(&mut bytes, self.config.max_fan_in);
        push_u64(&mut bytes, self.epoch);
        push_usize(&mut bytes, self.shards.len());
        for (shard_id, shard) in &self.shards {
            push_bytes(&mut bytes, shard_id.as_bytes());
            push_bool(&mut bytes, shard.blocked);
            push_u64(&mut bytes, shard.next_arrival);
            push_usize(&mut bytes, shard.requests.len());
            for (arrival, record) in &shard.requests {
                push_u64(&mut bytes, arrival.get());
                push_bytes(&mut bytes, record.request_id.as_bytes());
                push_bytes(&mut bytes, record.key.as_bytes());
                bytes.push(state_tag(record.state));
                push_usize(&mut bytes, record.dependencies.len());
                for dependency in &record.dependencies {
                    push_bytes(&mut bytes, dependency.as_bytes());
                }
            }
        }
        Hash32::of(&bytes)
    }

    pub(crate) fn find_request_shard(&self, request_id: RequestId) -> Option<&ShardId> {
        self.shards
            .iter()
            .find(|(_, shard)| shard.by_id.contains_key(&request_id))
            .map(|(shard_id, _)| shard_id)
    }
}

impl Default for SchedulerState {
    fn default() -> Self {
        Self::new()
    }
}

/// The internal state of one shard.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ShardState {
    pub(crate) blocked: bool,
    pub(crate) next_arrival: u64,
    pub(crate) requests: BTreeMap<Arrival, RequestRecord>,
    pub(crate) by_id: BTreeMap<RequestId, Arrival>,
    pub(crate) keys: BTreeMap<KeyId, BTreeSet<Arrival>>,
    pub(crate) ready: BTreeSet<Arrival>,
    pub(crate) dependents: BTreeMap<RequestId, BTreeSet<RequestId>>,
}

impl ShardState {
    pub(crate) fn head_arrival(&self, key: &KeyId) -> Option<Arrival> {
        self.keys
            .get(key)
            .and_then(|arrivals| arrivals.first().copied())
    }

    pub(crate) fn is_terminal(&self, request_id: &RequestId) -> bool {
        self.by_id
            .get(request_id)
            .and_then(|arrival| self.requests.get(arrival))
            .is_some_and(|record| record.state.is_terminal())
    }

    pub(crate) fn dependencies_terminal(&self, record: &RequestRecord) -> bool {
        record
            .dependencies
            .iter()
            .all(|dependency| self.is_terminal(dependency))
    }

    pub(crate) fn is_ready(&self, arrival: Arrival) -> bool {
        let Some(record) = self.requests.get(&arrival) else {
            return false;
        };
        if record.state != RequestState::Queued {
            return false;
        }
        if self.head_arrival(&record.key) != Some(arrival) {
            return false;
        }
        self.dependencies_terminal(record)
    }

    pub(crate) fn refresh_ready(&mut self, arrival: Arrival) {
        if self.is_ready(arrival) {
            self.ready.insert(arrival);
        } else {
            self.ready.remove(&arrival);
        }
    }
}

/// One request record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestRecord {
    pub(crate) request_id: RequestId,
    pub(crate) key: KeyId,
    pub(crate) arrival: Arrival,
    pub(crate) state: RequestState,
    pub(crate) dependencies: Vec<RequestId>,
}

/// An error from a scheduler state transition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    /// The shard is not in the state.
    #[error("The shard is not in the scheduler state.")]
    UnknownShard {
        /// The unknown shard.
        shard_id: ShardId,
    },
    /// The request is not in the shard.
    #[error("The request is not in the shard.")]
    UnknownRequest {
        /// The unknown request.
        request_id: RequestId,
    },
    /// The request is not in the state that the transition needs.
    #[error("The request is in the {found} state. The transition needs the {expected} state.")]
    InvalidTransition {
        /// The request.
        request_id: RequestId,
        /// The required state.
        expected: RequestState,
        /// The current state.
        found: RequestState,
    },
    /// The batch selects a key that is not free.
    #[error("The batch selects a key with an earlier request or an in-flight request.")]
    KeyNotFree {
        /// The request.
        request_id: RequestId,
    },
}

impl TransitionError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownShard { .. }
            | Self::UnknownRequest { .. }
            | Self::InvalidTransition { .. }
            | Self::KeyNotFree { .. } => "internal.invariant_violation",
        }
    }

    /// Returns the operator diagnostic for the failed transition check.
    ///
    /// The diagnostic is for operators and logs. It is not a wire value. The
    /// public code stays `internal.invariant_violation`.
    #[must_use]
    pub const fn diagnostic(&self) -> InternalDiagnostic {
        match self {
            Self::UnknownShard { .. }
            | Self::UnknownRequest { .. }
            | Self::InvalidTransition { .. }
            | Self::KeyNotFree { .. } => InternalDiagnostic::SchedulerTransition,
        }
    }
}

fn state_tag(state: RequestState) -> u8 {
    match state {
        RequestState::Queued => 1,
        RequestState::InFlight => 2,
        RequestState::Terminal => 3,
    }
}

fn push_bool(bytes: &mut Vec<u8>, value: bool) {
    bytes.push(u8::from(value));
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_usize(bytes: &mut Vec<u8>, value: usize) {
    push_u64(bytes, u64::try_from(value).unwrap_or(u64::MAX));
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    push_usize(bytes, value.len());
    bytes.extend_from_slice(value);
}
