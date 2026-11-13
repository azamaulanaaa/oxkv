//! Integration tests for the pure write scheduler.

mod support;

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::error::lookup;
use oxkv_contracts::receipt::RequestId;
use oxkv_contracts::version::{Hash32, ShardId};
use oxkv_scheduler::{
    AdmissionError, BudgetUsage, Decision, RequestState, SchedulerBudget, SchedulerConfig,
    SchedulerState, Selection, TransitionError, UnselectedReason, starvation_bound,
};
use support::{Sequence, admit, admit_result, key, request, shard};

fn selected_ids(decision: &Decision) -> Vec<u8> {
    decision
        .selections
        .iter()
        .map(|selection| selection.request_id.as_bytes()[0])
        .collect()
}

fn reason_of(decision: &Decision, request_id: u8) -> Option<UnselectedReason> {
    decision
        .reasons
        .iter()
        .find(|reason| reason.request_id.as_bytes()[0] == request_id)
        .map(|reason| reason.reason)
}

fn reason_map(decision: &Decision) -> BTreeMap<u8, UnselectedReason> {
    decision
        .reasons
        .iter()
        .map(|reason| (reason.request_id.as_bytes()[0], reason.reason))
        .collect()
}

#[test]
fn per_key_fifo_holds_across_interleavings() {
    for seed in 0..32_u64 {
        let mut state = SchedulerState::new();
        let shard_id = shard(1);
        let mut sequence = Sequence::new(seed);
        let mut remaining: Vec<u8> = (0_u8..4).collect();
        let mut next_index: BTreeMap<u8, u8> = BTreeMap::new();
        let mut expected: BTreeMap<u8, Vec<u8>> = BTreeMap::new();

        while !remaining.is_empty() {
            let position = sequence.below(remaining.len());
            let key_index = remaining[position];
            let index = next_index.entry(key_index).or_insert(0);
            let request_number = key_index * 3 + *index + 1;
            admit(
                &mut state,
                &shard_id,
                &key(key_index),
                request(request_number),
                &[],
            );
            *index += 1;
            if *index == 3 {
                remaining.remove(position);
            }
        }
        for key_index in 0_u8..4 {
            expected.insert(
                key_index,
                (0_u8..3).map(|index| key_index * 3 + index + 1).collect(),
            );
        }

        let mut selected: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
        let mut committed: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
        let budget = SchedulerBudget::new(3, 64);
        let mut call = 0_u32;
        while state.pending_count() > 0 {
            call += 1;
            assert!(call < 100, "the scheduler made no progress");
            let decision = state.decide(budget);
            assert!(!decision.is_empty(), "a ready request must be selected");
            for selection in &decision.selections {
                let request_number = selection.request_id.as_bytes()[0];
                let key_index = (request_number - 1) / 3;
                selected.entry(key_index).or_default().push(request_number);
            }
            state
                .apply_selection(&decision)
                .expect("the decision applies");
            for key_index in 0_u8..4 {
                assert!(
                    state.in_flight_count(&shard_id, &key(key_index)) <= 1,
                    "a key must have at most one in-flight request"
                );
            }

            let mut to_retire: Vec<(ShardId, RequestId)> = decision
                .selections
                .iter()
                .map(|selection| (selection.shard_id.clone(), selection.request_id))
                .collect();
            sequence.shuffle(&mut to_retire);
            for (selection_shard, request_id) in to_retire {
                let request_number = request_id.as_bytes()[0];
                let key_index = (request_number - 1) / 3;
                committed.entry(key_index).or_default().push(request_number);
                state
                    .retire(&selection_shard, request_id)
                    .expect("the request retires");
            }
        }

        assert_eq!(
            selected, expected,
            "selection order changed for seed {seed}"
        );
        assert_eq!(committed, expected, "commit order changed for seed {seed}");
    }
}

#[test]
fn a_blocked_key_does_not_delay_a_ready_key_in_the_same_shard() {
    let mut state = SchedulerState::new();
    let shard_id = shard(1);
    admit(&mut state, &shard_id, &key(1), request(1), &[]);
    admit(&mut state, &shard_id, &key(1), request(2), &[]);
    admit(&mut state, &shard_id, &key(2), request(3), &[]);

    let decision = state.decide(SchedulerBudget::new(2, 8));
    assert_eq!(selected_ids(&decision), vec![1, 3]);
    assert_eq!(reason_of(&decision, 2), Some(UnselectedReason::KeyBlocked));
    assert!(!decision.more_work);
    state
        .apply_selection(&decision)
        .expect("the decision applies");
    state
        .retire(&shard_id, request(3))
        .expect("the ready key retires");

    let blocked = state.decide(SchedulerBudget::new(2, 8));
    assert!(blocked.is_empty());
    assert_eq!(reason_of(&blocked, 1), Some(UnselectedReason::InFlight));
    assert_eq!(reason_of(&blocked, 2), Some(UnselectedReason::KeyBlocked));

    state
        .retire(&shard_id, request(1))
        .expect("the blocking request retires");
    let promoted = state.decide(SchedulerBudget::new(2, 8));
    assert_eq!(selected_ids(&promoted), vec![2]);
}

fn two_shard_state(blocked: bool) -> SchedulerState {
    let mut state = SchedulerState::new();
    let first = shard(1);
    let second = shard(2);
    if blocked {
        admit(&mut state, &first, &key(1), request(1), &[]);
        admit(&mut state, &first, &key(2), request(2), &[]);
        state.block_shard(&first);
    }
    for index in 0_u8..10 {
        admit(
            &mut state,
            &second,
            &key(10 + index),
            request(10 + index),
            &[],
        );
    }
    state
}

#[test]
fn a_blocked_shard_does_not_reduce_another_shard_selections() {
    let budget = SchedulerBudget::new(3, 64);
    let mut blocked = two_shard_state(true);
    let mut absent = two_shard_state(false);
    let mut blocked_batches = Vec::new();
    let mut absent_batches = Vec::new();

    for _ in 0..3 {
        let blocked_decision = blocked.decide(budget);
        let absent_decision = absent.decide(budget);
        assert_eq!(blocked_decision.selections.len(), 3);
        assert_eq!(absent_decision.selections.len(), 3);
        assert_eq!(blocked_decision.selections, absent_decision.selections);
        assert!(
            blocked_decision
                .selections
                .iter()
                .all(|selection| selection.shard_id == shard(2))
        );
        for (decision, state) in [
            (&blocked_decision, &mut blocked),
            (&absent_decision, &mut absent),
        ] {
            state
                .apply_selection(decision)
                .expect("the decision applies");
            for selection in &decision.selections {
                state
                    .retire(&selection.shard_id, selection.request_id)
                    .expect("the selected request retires");
            }
        }
        blocked_batches.push(selected_ids(&blocked_decision));
        absent_batches.push(selected_ids(&absent_decision));
    }

    assert_eq!(blocked_batches, absent_batches);
    assert_eq!(blocked_batches.iter().map(Vec::len).sum::<usize>(), 9);
    assert_eq!(
        reason_of(&blocked.decide(budget), 1),
        Some(UnselectedReason::ShardBlocked)
    );
}

fn decision_state() -> SchedulerState {
    let mut state = SchedulerState::new();
    let first = shard(1);
    let second = shard(2);
    admit(&mut state, &first, &key(1), request(1), &[]);
    admit(&mut state, &first, &key(2), request(2), &[]);
    admit(&mut state, &first, &key(2), request(3), &[]);
    admit(&mut state, &second, &key(3), request(4), &[]);
    admit(&mut state, &second, &key(4), request(5), &[request(4)]);
    state
}

#[test]
fn equal_state_and_budget_yield_equal_batches() {
    let budget = SchedulerBudget::new(2, 8);
    let left = decision_state();
    let right = decision_state();
    assert_eq!(left.digest(), right.digest());
    let left_decision = left.decide(budget);
    let right_decision = right.decide(budget);
    assert_eq!(left_decision, right_decision);
    assert_eq!(left_decision.usage.positions, 2);
    assert!(left_decision.usage.positions <= budget.max_positions);
    assert!(left_decision.usage.edges <= budget.max_edges);

    let mut left = left;
    let mut right = right;
    left.apply_selection(&left_decision)
        .expect("the left decision applies");
    right
        .apply_selection(&right_decision)
        .expect("the right decision applies");
    assert_eq!(left.digest(), right.digest());
}

fn bounds_state() -> SchedulerState {
    let mut state = SchedulerState::new();
    let shard_id = shard(1);
    admit(&mut state, &shard_id, &key(0), request(1), &[]);
    let base = state.decide(SchedulerBudget::new(1, 0));
    state
        .apply_selection(&base)
        .expect("the base request applies");
    state
        .retire(&shard_id, request(1))
        .expect("the base request retires");
    for index in 0_u8..5 {
        admit(
            &mut state,
            &shard_id,
            &key(index + 1),
            request(index + 2),
            &[request(1)],
        );
    }
    state
}

#[test]
fn position_and_edge_bounds_are_observed() {
    let budgets = [
        SchedulerBudget::new(0, 0),
        SchedulerBudget::new(1, 0),
        SchedulerBudget::new(1, 1),
        SchedulerBudget::new(2, 1),
        SchedulerBudget::new(2, 2),
        SchedulerBudget::new(5, 5),
        SchedulerBudget::new(10, 100),
    ];
    for budget in budgets {
        let state = bounds_state();
        let decision = state.decide(budget);
        assert!(
            decision.usage.positions <= budget.max_positions,
            "positions {} exceed {}",
            decision.usage.positions,
            budget.max_positions
        );
        assert!(
            decision.usage.edges <= budget.max_edges,
            "edges {} exceed {}",
            decision.usage.edges,
            budget.max_edges
        );
        if budget.max_positions == 0 {
            assert!(decision.is_empty());
            assert!(decision.more_work);
        }
        assert!(
            budget.max_edges >= 1 || decision.is_empty(),
            "the edge budget is too small for a selected edge"
        );
    }

    let partial = bounds_state().decide(SchedulerBudget::new(4, 1));
    assert_eq!(selected_ids(&partial), vec![2]);
    assert_eq!(partial.usage.positions, 2);
    assert_eq!(partial.usage.edges, 1);
    assert!(partial.more_work);
}

fn starvation_state() -> SchedulerState {
    let mut state = SchedulerState::new();
    for shard_index in 0_u8..2 {
        let shard_id = shard(shard_index + 1);
        for key_index in 0_u8..5 {
            let request_number = shard_index * 5 + key_index + 1;
            admit(
                &mut state,
                &shard_id,
                &key(key_index),
                request(request_number),
                &[],
            );
        }
    }
    state
}

#[test]
fn a_ready_request_is_selected_within_the_starvation_bound() {
    let mut state = starvation_state();
    let budget = SchedulerBudget::new(3, 64);
    let bound = starvation_bound(10, budget.max_positions);
    assert_eq!(bound, 4);
    let mut selected_at: BTreeMap<u8, usize> = BTreeMap::new();
    let mut call = 0_usize;
    while state.pending_count() > 0 {
        call += 1;
        let decision = state.decide(budget);
        for selection in &decision.selections {
            selected_at.insert(selection.request_id.as_bytes()[0], call);
        }
        state
            .apply_selection(&decision)
            .expect("the decision applies");
        for selection in &decision.selections {
            state
                .retire(&selection.shard_id, selection.request_id)
                .expect("the selected request retires");
        }
    }
    assert_eq!(selected_at.len(), 10);
    for (request_number, selected_call) in &selected_at {
        assert!(
            *selected_call <= bound,
            "request {request_number} waited {selected_call} calls"
        );
    }
    assert_eq!(starvation_bound(0, 3), 0);
    assert_eq!(starvation_bound(10, 0), usize::MAX);
    assert_eq!(starvation_bound(10, 4), 3);
}

#[test]
fn a_sparse_shard_is_not_starved_by_a_skewed_shard() {
    let mut state = SchedulerState::new();
    let busy = shard(1);
    let sparse = shard(2);
    for index in 0_u8..10 {
        admit(&mut state, &busy, &key(index), request(index + 1), &[]);
    }
    admit(&mut state, &sparse, &key(99), request(99), &[]);
    let decision = state.decide(SchedulerBudget::new(3, 64));
    assert!(
        decision.selections.iter().any(|selection| {
            selection.request_id == request(99) && selection.shard_id == sparse
        })
    );
}

#[test]
fn admission_rejects_cycle_forward_cross_shard_and_fan_in() {
    let mut state = SchedulerState::with_config(SchedulerConfig::new(2));
    let first = shard(1);
    let second = shard(2);

    let cycle = admit_result(&mut state, &first, &key(1), request(1), &[request(1)])
        .expect_err("a self-edge is a cycle");
    assert!(matches!(cycle, AdmissionError::Cycle { .. }));
    assert_eq!(cycle.code(), "outbox.dependency_cycle");

    let forward = admit_result(&mut state, &first, &key(1), request(2), &[request(3)])
        .expect_err("an unknown dependency is a forward reference");
    assert!(matches!(forward, AdmissionError::ForwardReference { .. }));
    assert_eq!(forward.code(), "outbox.dependency_cycle");

    admit(&mut state, &second, &key(3), request(3), &[]);
    let cross = admit_result(&mut state, &first, &key(2), request(2), &[request(3)])
        .expect_err("a dependency in another shard is a cross-shard edge");
    assert!(matches!(cross, AdmissionError::CrossShardEdge { .. }));
    assert_eq!(cross.code(), "outbox.cross_shard_edge");

    admit(&mut state, &first, &key(10), request(10), &[]);
    admit(&mut state, &first, &key(11), request(11), &[]);
    admit(&mut state, &first, &key(12), request(12), &[]);
    let fan_in = admit_result(
        &mut state,
        &first,
        &key(13),
        request(13),
        &[request(10), request(11), request(12)],
    )
    .expect_err("three edges exceed a limit of two");
    assert!(matches!(fan_in, AdmissionError::FanInExceeded { .. }));
    assert_eq!(fan_in.code(), "write.batch_too_large");

    admit(&mut state, &first, &key(20), request(20), &[]);
    let duplicate = admit_result(&mut state, &first, &key(21), request(20), &[])
        .expect_err("a request id is admitted once");
    assert!(matches!(duplicate, AdmissionError::DuplicateRequest { .. }));
    assert_eq!(duplicate.code(), "write.idempotency_key_reuse");

    admit(&mut state, &first, &key(21), request(21), &[request(20)]);
    assert_eq!(
        state.request_state(&first, request(21)),
        Some(RequestState::Queued)
    );

    for code in [
        "outbox.dependency_cycle",
        "outbox.cross_shard_edge",
        "write.batch_too_large",
        "write.idempotency_key_reuse",
        "internal.resource_exhausted",
    ] {
        assert!(lookup(code).is_some(), "the code {code} is registered");
    }
}

fn replay_run() -> (Vec<Decision>, Vec<Hash32>) {
    let mut state = SchedulerState::new();
    let first = shard(1);
    let second = shard(2);
    for index in 0_u8..4 {
        admit(&mut state, &first, &key(index), request(index + 1), &[]);
    }
    for index in 0_u8..3 {
        admit(
            &mut state,
            &second,
            &key(20 + index),
            request(20 + index),
            &[],
        );
    }
    admit(&mut state, &second, &key(30), request(30), &[request(20)]);

    let mut decisions = Vec::new();
    let mut digests = vec![state.digest()];
    let budgets = [
        SchedulerBudget::new(2, 8),
        SchedulerBudget::new(1, 2),
        SchedulerBudget::new(3, 64),
        SchedulerBudget::new(2, 1),
    ];
    for (step, budget) in budgets.into_iter().enumerate() {
        if step == 1 {
            state.block_shard(&second);
        }
        if step == 3 {
            state.unblock_shard(&second);
        }
        let decision = state.decide(budget);
        decisions.push(decision.clone());
        digests.push(state.digest());
        state
            .apply_selection(&decision)
            .expect("the replayed decision applies");
        for selection in decision.selections.iter().rev() {
            state
                .retire(&selection.shard_id, selection.request_id)
                .expect("the replayed request retires");
        }
        digests.push(state.digest());
    }
    (decisions, digests)
}

#[test]
fn replay_rebuilds_the_batch_sequence() {
    let (first_decisions, first_digests) = replay_run();
    let (second_decisions, second_digests) = replay_run();
    assert_eq!(first_decisions, second_decisions);
    assert_eq!(first_digests, second_digests);
    assert!(first_decisions.iter().any(|decision| decision.more_work));
    assert!(
        first_decisions
            .iter()
            .all(|decision| { decision.usage.positions <= 3 && decision.usage.edges <= 64 })
    );
}

#[test]
fn every_unselected_request_has_exactly_one_reason() {
    let mut state = SchedulerState::new();
    let blocked_shard = shard(1);
    let active_shard = shard(2);
    admit(&mut state, &blocked_shard, &key(1), request(1), &[]);
    admit(&mut state, &active_shard, &key(2), request(2), &[]);
    admit(&mut state, &active_shard, &key(2), request(3), &[]);
    admit(
        &mut state,
        &active_shard,
        &key(3),
        request(4),
        &[request(3)],
    );
    for index in 0_u8..5 {
        admit(
            &mut state,
            &active_shard,
            &key(10 + index),
            request(10 + index),
            &[],
        );
    }
    state.block_shard(&blocked_shard);

    let first = state.decide(SchedulerBudget::new(2, 8));
    state
        .apply_selection(&first)
        .expect("the first decision applies");
    let second = state.decide(SchedulerBudget::new(2, 8));
    state
        .apply_selection(&second)
        .expect("the second decision applies");

    let expected = BTreeMap::from([
        (1, UnselectedReason::ShardBlocked),
        (2, UnselectedReason::InFlight),
        (3, UnselectedReason::KeyBlocked),
        (4, UnselectedReason::DependencyBlocked),
        (10, UnselectedReason::InFlight),
        (13, UnselectedReason::OutsideBudget),
        (14, UnselectedReason::OutsideBudget),
    ]);
    assert_eq!(reason_map(&second), expected);
    assert_eq!(selected_ids(&second), vec![11, 12]);

    let mut seen: BTreeSet<u8> = selected_ids(&second).into_iter().collect();
    for reason in &second.reasons {
        assert!(
            seen.insert(reason.request_id.as_bytes()[0]),
            "a request has two reasons"
        );
    }
    for request_number in [1_u8, 2, 3, 4, 10, 11, 12, 13, 14] {
        let request_id = request(request_number);
        let request_state = state
            .request_state(&blocked_shard, request_id)
            .or_else(|| state.request_state(&active_shard, request_id))
            .expect("the request is in the state");
        if request_state == RequestState::Terminal {
            assert!(!seen.contains(&request_number));
        } else {
            assert!(
                seen.remove(&request_number),
                "request {request_number} has no reason"
            );
        }
    }
    assert!(seen.is_empty());

    state
        .retire(&active_shard, request(12))
        .expect("the terminal removal succeeds");
    let third = state.decide(SchedulerBudget::new(2, 8));
    assert!(!selected_ids(&third).contains(&12));
    assert!(reason_of(&third, 12).is_none());
}

#[test]
fn the_scheduler_does_not_infer_a_dependency() {
    let mut state = SchedulerState::new();
    let shard_id = shard(1);
    admit(&mut state, &shard_id, &key(1), request(1), &[]);
    admit(&mut state, &shard_id, &key(2), request(2), &[]);
    admit(&mut state, &shard_id, &key(1), request(3), &[]);

    let decision = state.decide(SchedulerBudget::new(2, 8));
    assert_eq!(selected_ids(&decision), vec![1, 2]);
    assert_eq!(reason_of(&decision, 3), Some(UnselectedReason::KeyBlocked));
}

#[test]
fn a_terminal_removal_keeps_per_key_order() {
    let mut state = SchedulerState::new();
    let shard_id = shard(1);
    admit(&mut state, &shard_id, &key(1), request(1), &[]);
    admit(&mut state, &shard_id, &key(1), request(2), &[]);
    admit(&mut state, &shard_id, &key(1), request(3), &[]);
    state
        .retire(&shard_id, request(1))
        .expect("the first request retires before selection");

    let second = state.decide(SchedulerBudget::new(1, 4));
    assert_eq!(selected_ids(&second), vec![2]);
    state
        .apply_selection(&second)
        .expect("the second request applies");
    state
        .retire(&shard_id, request(2))
        .expect("the second request retires");
    let third = state.decide(SchedulerBudget::new(1, 4));
    assert_eq!(selected_ids(&third), vec![3]);
}

#[test]
fn apply_selection_rejects_a_stale_decision() {
    let mut state = SchedulerState::new();
    let shard_id = shard(1);
    admit(&mut state, &shard_id, &key(1), request(1), &[]);
    let decision = state.decide(SchedulerBudget::new(1, 4));
    state
        .apply_selection(&decision)
        .expect("the first application succeeds");
    let digest = state.digest();
    let second_application = state.apply_selection(&decision);
    assert!(matches!(
        second_application,
        Err(TransitionError::InvalidTransition { .. })
    ));
    assert_eq!(state.digest(), digest);
    assert_eq!(
        state
            .apply_selection(&decision)
            .expect_err("the stale decision fails")
            .code(),
        "internal.invariant_violation"
    );
}

#[test]
fn apply_selection_rejects_a_non_head_batch() {
    let mut state = SchedulerState::new();
    let shard_id = shard(1);
    admit(&mut state, &shard_id, &key(1), request(1), &[]);
    admit(&mut state, &shard_id, &key(1), request(2), &[]);
    let second_arrival = state
        .request_arrival(&shard_id, request(2))
        .expect("the second request is admitted");
    let forged = Decision {
        selections: vec![Selection {
            shard_id: shard_id.clone(),
            key: key(1),
            request_id: request(2),
            arrival: second_arrival,
        }],
        reasons: Vec::new(),
        more_work: false,
        usage: BudgetUsage::default(),
        next_epoch: 0,
    };
    assert!(matches!(
        state.apply_selection(&forged),
        Err(TransitionError::KeyNotFree { .. })
    ));
    assert_eq!(state.pending_count(), 2);
}

#[test]
fn the_core_has_no_clock_randomness_io_or_float() {
    let sources = [
        include_str!("../src/lib.rs"),
        include_str!("../src/state.rs"),
        include_str!("../src/admit.rs"),
        include_str!("../src/budget.rs"),
        include_str!("../src/select.rs"),
    ];
    for source in sources {
        for forbidden in [
            "std::time",
            "SystemTime",
            "Instant::",
            "rand::",
            "std::fs",
            "std::net",
            "std::io",
            "f32",
            "f64",
            "HashMap",
            "HashSet",
            "ReceiptState",
            "ReceiptEnvelope",
        ] {
            assert!(!source.contains(forbidden), "the core contains {forbidden}");
        }
    }
    assert!(sources[0].contains("#![forbid(unsafe_code)]"));
}
