//! Bounded deterministic selection.
//!
//! One decision walks the ready sets in a stable shard rotation. It selects
//! at most one request per key. It selects at most one request per shard per
//! round. A blocked key or a blocked shard never consumes a position.
//!
//! The decision is pure. It does not change the state. The commit path calls
//! [`SchedulerState::apply_selection`] to mark the selected requests in
//! flight.

use std::collections::BTreeSet;

use oxkv_contracts::receipt::RequestId;
use oxkv_contracts::version::ShardId;

use crate::budget::{BudgetUsage, SchedulerBudget};
use crate::state::{Arrival, KeyId, RequestState, SchedulerState, TransitionError};

/// One selected request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The shard that owns the request.
    pub shard_id: ShardId,
    /// The request key.
    pub key: KeyId,
    /// The request identity.
    pub request_id: RequestId,
    /// The arrival sequence number in the shard.
    pub arrival: Arrival,
}

/// The reason that one non-terminal request was not selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnselectedReason {
    /// The shard is blocked.
    ShardBlocked,
    /// An earlier non-terminal request holds the same key.
    KeyBlocked,
    /// An explicit dependency is not terminal.
    DependencyBlocked,
    /// The request is already in flight.
    InFlight,
    /// The position or edge budget stopped the scan.
    OutsideBudget,
}

impl UnselectedReason {
    /// Returns the canonical reason name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ShardBlocked => "shard_blocked",
            Self::KeyBlocked => "key_blocked",
            Self::DependencyBlocked => "dependency_blocked",
            Self::InFlight => "in_flight",
            Self::OutsideBudget => "outside_budget",
        }
    }
}

/// One unselected request and its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unselected {
    /// The shard that owns the request.
    pub shard_id: ShardId,
    /// The request identity.
    pub request_id: RequestId,
    /// The single reason the request was not selected.
    pub reason: UnselectedReason,
}

/// The result of one scheduler decision.
///
/// Every non-terminal request appears in exactly one place. A selected request
/// appears in `selections`. Every other non-terminal request appears in
/// `reasons`. A terminal request appears in neither list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// The selected requests in commit order.
    pub selections: Vec<Selection>,
    /// One reason for every unselected non-terminal request.
    pub reasons: Vec<Unselected>,
    /// True when a ready request remains for a later call.
    pub more_work: bool,
    /// The work that this decision used.
    pub usage: BudgetUsage,
    /// The rotation epoch for the next decision.
    pub next_epoch: u64,
}

impl Decision {
    /// Returns true when the decision selected no request.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.selections.is_empty()
    }
}

impl SchedulerState {
    /// Selects the next batch of ready requests.
    ///
    /// The function inspects at most `budget.max_positions` ready positions
    /// and at most `budget.max_edges` dependency edges. When a limit stops the
    /// scan, the function returns a partial decision and sets `more_work`.
    ///
    /// The function is pure. Call [`Self::apply_selection`] to mark the chosen
    /// requests in flight.
    #[must_use]
    pub fn decide(&self, budget: SchedulerBudget) -> Decision {
        let available: Vec<ShardId> = self
            .shards
            .iter()
            .filter(|(_, shard)| !shard.blocked && !shard.ready.is_empty())
            .map(|(shard_id, _)| shard_id.clone())
            .collect();
        let mut taken = vec![0_usize; available.len()];
        let mut selections = Vec::new();
        let mut usage = BudgetUsage::default();

        if !available.is_empty() && budget.max_positions > 0 {
            let count = available.len();
            let offset = rotation_offset(self.epoch, count);
            'rounds: loop {
                let mut selected_in_round = false;
                for step in 0..count {
                    if usage.positions >= budget.max_positions {
                        break 'rounds;
                    }
                    let index = (offset + step) % count;
                    let shard = &self.shards[&available[index]];
                    let Some(arrival) = shard.ready.iter().nth(taken[index]).copied() else {
                        continue;
                    };
                    let Some(record) = shard.requests.get(&arrival) else {
                        break 'rounds;
                    };

                    usage.positions += 1;
                    let remaining_edges = budget.max_edges.saturating_sub(usage.edges);
                    if record.dependencies.len() > remaining_edges {
                        break 'rounds;
                    }
                    usage.edges += record.dependencies.len();

                    if !shard.dependencies_terminal(record) {
                        break 'rounds;
                    }

                    selections.push(Selection {
                        shard_id: available[index].clone(),
                        key: record.key.clone(),
                        request_id: record.request_id,
                        arrival,
                    });
                    taken[index] += 1;
                    selected_in_round = true;
                }
                if !selected_in_round {
                    break;
                }
            }
        }

        let more_work = available
            .iter()
            .enumerate()
            .any(|(index, shard_id)| self.shards[shard_id].ready.len() > taken[index]);
        let reasons = self.reasons_for(&selections);
        let selected_count = u64::try_from(selections.len()).unwrap_or(u64::MAX);
        let next_epoch = self.epoch.saturating_add(selected_count);

        Decision {
            selections,
            reasons,
            more_work,
            usage,
            next_epoch,
        }
    }

    /// Marks every selected request in flight.
    ///
    /// The method checks every selection before it changes the state. A stale
    /// decision changes nothing and returns an error.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError::UnknownShard`] when a selection names an
    /// unknown shard. Returns [`TransitionError::UnknownRequest`] when a
    /// selection names an unknown arrival or request. Returns
    /// [`TransitionError::InvalidTransition`] when a selected request is not
    /// in the queued state. Returns [`TransitionError::KeyNotFree`] when a
    /// batch selects a key twice, selects a non-head request, or selects a key
    /// that already has an in-flight request.
    pub fn apply_selection(&mut self, decision: &Decision) -> Result<(), TransitionError> {
        let mut selected_keys = BTreeSet::new();
        for selection in &decision.selections {
            let shard = self.shards.get(&selection.shard_id).ok_or_else(|| {
                TransitionError::UnknownShard {
                    shard_id: selection.shard_id.clone(),
                }
            })?;
            let record =
                shard
                    .requests
                    .get(&selection.arrival)
                    .ok_or(TransitionError::UnknownRequest {
                        request_id: selection.request_id,
                    })?;
            if record.request_id != selection.request_id {
                return Err(TransitionError::UnknownRequest {
                    request_id: selection.request_id,
                });
            }
            if record.state != RequestState::Queued {
                return Err(TransitionError::InvalidTransition {
                    request_id: selection.request_id,
                    expected: RequestState::Queued,
                    found: record.state,
                });
            }
            if !selected_keys.insert((selection.shard_id.clone(), record.key.clone()))
                || shard.head_arrival(&record.key) != Some(selection.arrival)
            {
                return Err(TransitionError::KeyNotFree {
                    request_id: selection.request_id,
                });
            }
            if shard
                .requests
                .values()
                .any(|other| other.key == record.key && other.state == RequestState::InFlight)
            {
                return Err(TransitionError::KeyNotFree {
                    request_id: selection.request_id,
                });
            }
        }

        for selection in &decision.selections {
            let Some(shard) = self.shards.get_mut(&selection.shard_id) else {
                continue;
            };
            let Some(record) = shard.requests.get_mut(&selection.arrival) else {
                continue;
            };
            record.state = RequestState::InFlight;
            shard.ready.remove(&selection.arrival);
        }
        self.epoch = decision.next_epoch;
        Ok(())
    }

    /// Marks one request terminal and promotes the next work.
    ///
    /// The method changes no receipt. It removes the request from the key
    /// queue and from the ready set. It promotes the next request on the key
    /// and every direct dependent when they become ready.
    ///
    /// A second retire of the same request is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError::UnknownShard`] when the shard is absent.
    /// Returns [`TransitionError::UnknownRequest`] when the request is absent.
    pub fn retire(
        &mut self,
        shard_id: &ShardId,
        request_id: RequestId,
    ) -> Result<(), TransitionError> {
        let Some(shard) = self.shards.get_mut(shard_id) else {
            return Err(TransitionError::UnknownShard {
                shard_id: shard_id.clone(),
            });
        };
        let Some(arrival) = shard.by_id.get(&request_id).copied() else {
            return Err(TransitionError::UnknownRequest { request_id });
        };
        let Some(record) = shard.requests.get(&arrival) else {
            return Err(TransitionError::UnknownRequest { request_id });
        };
        if record.state.is_terminal() {
            return Ok(());
        }

        let key = record.key.clone();
        let dependencies = record.dependencies.clone();

        if let Some(record) = shard.requests.get_mut(&arrival) {
            record.state = RequestState::Terminal;
        }
        shard.ready.remove(&arrival);

        let key_is_empty = if let Some(arrivals) = shard.keys.get_mut(&key) {
            arrivals.remove(&arrival);
            arrivals.is_empty()
        } else {
            false
        };
        if key_is_empty {
            shard.keys.remove(&key);
        }
        if let Some(next) = shard.head_arrival(&key) {
            shard.refresh_ready(next);
        }

        if let Some(dependents) = shard.dependents.remove(&request_id) {
            for dependent in dependents {
                if let Some(dependent_arrival) = shard.by_id.get(&dependent).copied() {
                    shard.refresh_ready(dependent_arrival);
                }
            }
        }

        for dependency in dependencies {
            let remove_entry = if let Some(dependents) = shard.dependents.get_mut(&dependency) {
                dependents.remove(&request_id);
                dependents.is_empty()
            } else {
                false
            };
            if remove_entry {
                shard.dependents.remove(&dependency);
            }
        }

        Ok(())
    }

    fn reasons_for(&self, selections: &[Selection]) -> Vec<Unselected> {
        let selected: BTreeSet<(ShardId, RequestId)> = selections
            .iter()
            .map(|selection| (selection.shard_id.clone(), selection.request_id))
            .collect();
        let mut reasons = Vec::new();
        for (shard_id, shard) in &self.shards {
            for (arrival, record) in &shard.requests {
                if record.state.is_terminal() {
                    continue;
                }
                if selected.contains(&(shard_id.clone(), record.request_id)) {
                    continue;
                }
                let reason = match record.state {
                    RequestState::InFlight => UnselectedReason::InFlight,
                    RequestState::Queued => {
                        if shard.blocked {
                            UnselectedReason::ShardBlocked
                        } else if shard.head_arrival(&record.key) != Some(*arrival) {
                            UnselectedReason::KeyBlocked
                        } else if !shard.dependencies_terminal(record) {
                            UnselectedReason::DependencyBlocked
                        } else {
                            UnselectedReason::OutsideBudget
                        }
                    }
                    RequestState::Terminal => continue,
                };
                reasons.push(Unselected {
                    shard_id: shard_id.clone(),
                    request_id: record.request_id,
                    reason,
                });
            }
        }
        reasons
    }
}

fn rotation_offset(epoch: u64, count: usize) -> usize {
    let Ok(count) = u64::try_from(count) else {
        return 0;
    };
    if count == 0 {
        return 0;
    }
    usize::try_from(epoch % count).unwrap_or(0)
}
