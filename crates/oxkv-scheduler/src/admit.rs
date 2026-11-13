//! Admission of explicit requests and dependency edges.
//!
//! Admission appends one request to the end of a shard arrival sequence. The
//! scheduler validates each edge against earlier requests. It never infers an
//! edge from values, versions, or reads.
//!
//! Admission appends a request after every existing request. A self-edge is
//! therefore the only cycle that a caller can declare. The cycle check also
//! walks the existing edges.
//!
//! A forward reference uses `outbox.dependency_cycle`. The error registry has
//! no forward-reference code. A forward reference and a cycle are both a
//! broken edge order. The code documents that shared cause.

use std::collections::BTreeSet;

use oxkv_contracts::receipt::RequestId;
use oxkv_contracts::version::ShardId;

use crate::state::{Arrival, KeyId, RequestRecord, RequestState, SchedulerState, ShardState};

/// One request to admit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestAdmission {
    /// The shard that owns the key.
    pub shard_id: ShardId,
    /// The canonical key bytes.
    pub key: KeyId,
    /// The request identity.
    pub request_id: RequestId,
    /// The explicit dependency edges. Every edge must name an earlier request
    /// in the same shard.
    pub dependencies: Vec<RequestId>,
}

impl RequestAdmission {
    /// Creates a request with no dependencies.
    #[must_use]
    pub fn new(shard_id: ShardId, key: KeyId, request_id: RequestId) -> Self {
        Self {
            shard_id,
            key,
            request_id,
            dependencies: Vec::new(),
        }
    }

    /// Adds explicit dependency edges.
    #[must_use]
    pub fn with_dependencies(mut self, dependencies: Vec<RequestId>) -> Self {
        self.dependencies = dependencies;
        self
    }
}

/// The result of one admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admitted {
    /// The assigned arrival sequence number.
    pub arrival: Arrival,
}

/// An error from admission.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    /// The edge would close a cycle.
    #[error(
        "The dependency edge would create a cycle. Request {request_id:?} depends on {dependency:?}."
    )]
    Cycle {
        /// The request that declares the edge.
        request_id: RequestId,
        /// The dependency that closes the cycle.
        dependency: RequestId,
    },
    /// The edge names a request that is not an earlier request in the shard.
    ///
    /// The error code is `outbox.dependency_cycle`. The registry has no
    /// forward-reference code.
    #[error(
        "The dependency is not an earlier request in the shard. Request {request_id:?} depends on {dependency:?}."
    )]
    ForwardReference {
        /// The request that declares the edge.
        request_id: RequestId,
        /// The missing dependency.
        dependency: RequestId,
    },
    /// The edge names a request in another shard.
    #[error(
        "The dependency edge crosses a shard boundary. Request {request_id:?} depends on {dependency:?}."
    )]
    CrossShardEdge {
        /// The request that declares the edge.
        request_id: RequestId,
        /// The dependency in the other shard.
        dependency: RequestId,
        /// The shard that holds the dependency.
        target_shard: ShardId,
    },
    /// The request has too many dependency edges.
    ///
    /// The error code is `write.batch_too_large`. The registry has no
    /// scheduler fan-in code.
    #[error("The request has too many dependency edges. The limit is {limit}.")]
    FanInExceeded {
        /// The request that declares the edges.
        request_id: RequestId,
        /// The configured limit.
        limit: usize,
        /// The number of unique edges.
        found: usize,
    },
    /// The request identity is already admitted.
    #[error("The request is already admitted.")]
    DuplicateRequest {
        /// The duplicate request.
        request_id: RequestId,
    },
    /// The shard arrival sequence is exhausted.
    #[error("The shard arrival sequence is exhausted.")]
    ArrivalExhausted {
        /// The shard with the exhausted sequence.
        shard_id: ShardId,
    },
}

impl AdmissionError {
    /// Returns the canonical error code.
    ///
    /// A forward reference returns `outbox.dependency_cycle`. The registry has
    /// no forward-reference code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Cycle { .. } | Self::ForwardReference { .. } => "outbox.dependency_cycle",
            Self::CrossShardEdge { .. } => "outbox.cross_shard_edge",
            Self::FanInExceeded { .. } | Self::DuplicateRequest { .. } => "write.batch_too_large",
            Self::ArrivalExhausted { .. } => "internal.resource_exhausted",
        }
    }
}

impl SchedulerState {
    /// Admits one request at the end of a shard arrival sequence.
    ///
    /// The scheduler collapses duplicate dependency edges. It stores the
    /// remaining edges in arrival order.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError::FanInExceeded`] when the unique edge count is
    /// above the configured limit. Returns [`AdmissionError::Cycle`] for a
    /// self-edge or a detected cycle. Returns
    /// [`AdmissionError::ForwardReference`] for an edge that names no earlier
    /// request. Returns [`AdmissionError::CrossShardEdge`] for an edge that
    /// names a request in another shard. Returns
    /// [`AdmissionError::DuplicateRequest`] for a request identity that is
    /// already admitted. Returns [`AdmissionError::ArrivalExhausted`] when the
    /// shard sequence cannot advance.
    pub fn admit(&mut self, admission: RequestAdmission) -> Result<Admitted, AdmissionError> {
        let RequestAdmission {
            shard_id,
            key,
            request_id,
            dependencies,
        } = admission;

        if self.find_request_shard(request_id).is_some() {
            return Err(AdmissionError::DuplicateRequest { request_id });
        }

        let unique: BTreeSet<RequestId> = dependencies.into_iter().collect();
        if unique.len() > self.config.max_fan_in {
            return Err(AdmissionError::FanInExceeded {
                request_id,
                limit: self.config.max_fan_in,
                found: unique.len(),
            });
        }

        let next_arrival = self
            .shards
            .get(&shard_id)
            .map_or(0, |shard| shard.next_arrival);
        let arrival = Arrival::new(next_arrival);
        let next_arrival =
            next_arrival
                .checked_add(1)
                .ok_or_else(|| AdmissionError::ArrivalExhausted {
                    shard_id: shard_id.clone(),
                })?;

        let mut resolved = Vec::with_capacity(unique.len());
        for dependency in unique {
            if dependency == request_id {
                return Err(AdmissionError::Cycle {
                    request_id,
                    dependency,
                });
            }
            if let Some(shard) = self.shards.get(&shard_id)
                && let Some(target_arrival) = shard.by_id.get(&dependency)
            {
                if *target_arrival >= arrival {
                    return Err(AdmissionError::ForwardReference {
                        request_id,
                        dependency,
                    });
                }
                resolved.push((*target_arrival, dependency));
                continue;
            }
            if let Some(target_shard) = self.find_request_shard(dependency) {
                return Err(AdmissionError::CrossShardEdge {
                    request_id,
                    dependency,
                    target_shard: target_shard.clone(),
                });
            }
            return Err(AdmissionError::ForwardReference {
                request_id,
                dependency,
            });
        }

        if let Some(shard) = self.shards.get(&shard_id) {
            for (_, dependency) in &resolved {
                if reaches(shard, *dependency, request_id) {
                    return Err(AdmissionError::Cycle {
                        request_id,
                        dependency: *dependency,
                    });
                }
            }
        }

        resolved.sort_unstable_by_key(|(target_arrival, _)| *target_arrival);
        let dependencies: Vec<RequestId> = resolved
            .into_iter()
            .map(|(_, dependency)| dependency)
            .collect();

        let shard = self.shards.entry(shard_id).or_default();
        let record = RequestRecord {
            request_id,
            key: key.clone(),
            arrival,
            state: RequestState::Queued,
            dependencies: dependencies.clone(),
        };
        for dependency in &dependencies {
            shard
                .dependents
                .entry(*dependency)
                .or_default()
                .insert(request_id);
        }
        shard.by_id.insert(request_id, arrival);
        shard.requests.insert(arrival, record);
        shard.keys.entry(key).or_default().insert(arrival);
        shard.next_arrival = next_arrival;
        shard.refresh_ready(arrival);

        Ok(Admitted { arrival })
    }
}

fn reaches(shard: &ShardState, from: RequestId, target: RequestId) -> bool {
    let mut stack = vec![from];
    let mut seen = BTreeSet::new();
    while let Some(current) = stack.pop() {
        if !seen.insert(current) {
            continue;
        }
        if current == target {
            return true;
        }
        let Some(arrival) = shard.by_id.get(&current) else {
            continue;
        };
        let Some(record) = shard.requests.get(arrival) else {
            continue;
        };
        for dependency in &record.dependencies {
            stack.push(*dependency);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_id(value: u8) -> RequestId {
        RequestId::from_bytes([value; 32])
    }

    #[test]
    fn the_cycle_check_walks_existing_edges() {
        let mut shard = ShardState::default();
        let first = request_id(1);
        let second = request_id(2);
        shard.by_id.insert(first, Arrival::new(0));
        shard.by_id.insert(second, Arrival::new(1));
        shard.requests.insert(
            Arrival::new(0),
            RequestRecord {
                request_id: first,
                key: KeyId::new(vec![1]),
                arrival: Arrival::new(0),
                state: RequestState::Queued,
                dependencies: vec![second],
            },
        );
        shard.requests.insert(
            Arrival::new(1),
            RequestRecord {
                request_id: second,
                key: KeyId::new(vec![2]),
                arrival: Arrival::new(1),
                state: RequestState::Queued,
                dependencies: vec![first],
            },
        );

        assert!(reaches(&shard, first, second));
        assert!(reaches(&shard, second, first));
        assert!(!reaches(&shard, first, request_id(3)));
    }
}
