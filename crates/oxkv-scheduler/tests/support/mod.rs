#![allow(missing_docs)]
#![allow(dead_code)]

//! Shared helpers for the scheduler tests.

use oxkv_contracts::receipt::RequestId;
use oxkv_contracts::version::ShardId;
use oxkv_scheduler::{AdmissionError, Admitted, Arrival, KeyId, RequestAdmission, SchedulerState};

/// Returns a shard identity from one byte.
pub(crate) fn shard(value: u8) -> ShardId {
    ShardId::new(vec![value])
}

/// Returns a shard identity from text bytes.
pub(crate) fn shard_text(value: &str) -> ShardId {
    ShardId::new(value.as_bytes().to_vec())
}

/// Returns a key identity from one byte.
pub(crate) fn key(value: u8) -> KeyId {
    KeyId::new(vec![value])
}

/// Returns a key identity from text bytes.
pub(crate) fn key_text(value: &str) -> KeyId {
    KeyId::new(value.as_bytes().to_vec())
}

/// Returns a request identity from one byte.
pub(crate) fn request(value: u8) -> RequestId {
    RequestId::from_bytes([value; 32])
}

/// Admits one request with explicit edges.
pub(crate) fn admit(
    state: &mut SchedulerState,
    shard_id: &ShardId,
    key_id: &KeyId,
    request_id: RequestId,
    dependencies: &[RequestId],
) -> Arrival {
    admit_result(state, shard_id, key_id, request_id, dependencies)
        .expect("the test admission is valid")
        .arrival
}

/// Admits one request and returns the raw result.
pub(crate) fn admit_result(
    state: &mut SchedulerState,
    shard_id: &ShardId,
    key_id: &KeyId,
    request_id: RequestId,
    dependencies: &[RequestId],
) -> Result<Admitted, AdmissionError> {
    state.admit(
        RequestAdmission::new(shard_id.clone(), key_id.clone(), request_id)
            .with_dependencies(dependencies.to_vec()),
    )
}

/// A deterministic integer sequence for test interleavings.
///
/// The sequence is not an entropy source. It is a fixed recurrence.
pub(crate) struct Sequence(u64);

impl Sequence {
    /// Creates a sequence from a fixed seed.
    pub(crate) const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Returns the next value.
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    /// Returns a value below one bound.
    pub(crate) fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        let bound = u64::try_from(bound).unwrap_or(u64::MAX);
        usize::try_from(self.next() % bound).unwrap_or(0)
    }

    /// Shuffles a slice in place.
    pub(crate) fn shuffle<T>(&mut self, values: &mut [T]) {
        if values.len() < 2 {
            return;
        }
        let mut index = values.len();
        while index > 1 {
            let other = self.below(index);
            index -= 1;
            values.swap(index, other);
        }
    }
}
