//! Golden vector runner for the write scheduler.

#![allow(clippy::too_many_lines)]

mod support;

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::receipt::RequestId;
use oxkv_scheduler::{Decision, RequestState, SchedulerBudget, SchedulerConfig, SchedulerState};
use serde_json::Value;
use support::{admit, key_text, request, shard_text};

fn as_usize(value: &Value) -> usize {
    let number = value.as_u64().expect("the golden field is a number");
    usize::try_from(number).expect("the golden number fits usize")
}

fn as_u8(value: &Value) -> u8 {
    let number = value.as_u64().expect("the golden identity is a number");
    u8::try_from(number).expect("the golden identity fits u8")
}

fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("the golden field {field} is text"))
}

fn selected_ids(decision: &Decision) -> Vec<u8> {
    decision
        .selections
        .iter()
        .map(|selection| selection.request_id.as_bytes()[0])
        .collect()
}

fn reason_map(decision: &Decision) -> BTreeMap<u8, &'static str> {
    decision
        .reasons
        .iter()
        .map(|reason| (reason.request_id.as_bytes()[0], reason.reason.name()))
        .collect()
}

fn expected_ids(value: &Value) -> Vec<u8> {
    value["selected"]
        .as_array()
        .expect("the decision has a selected list")
        .iter()
        .map(as_u8)
        .collect()
}

fn expected_reasons(value: &Value) -> BTreeMap<u8, String> {
    value["reasons"]
        .as_object()
        .expect("the decision has a reason map")
        .iter()
        .map(|(request_id, reason)| {
            let request_id = request_id
                .parse::<u8>()
                .expect("the reason request id is a number");
            (
                request_id,
                reason.as_str().expect("the reason name is text").to_owned(),
            )
        })
        .collect()
}

fn assert_decision(
    name: &str,
    expected: &Value,
    decision: &Decision,
    state: &SchedulerState,
    shards: &BTreeMap<u8, oxkv_contracts::version::ShardId>,
) {
    assert_eq!(
        selected_ids(decision),
        expected_ids(expected),
        "the case {name} selected other requests"
    );
    assert_eq!(
        decision.more_work,
        expected["more_work"]
            .as_bool()
            .expect("the decision names more_work"),
        "the case {name} reports other more_work"
    );
    assert_eq!(
        decision.usage.positions,
        as_usize(&expected["positions"]),
        "the case {name} inspected other positions"
    );
    assert_eq!(
        decision.usage.edges,
        as_usize(&expected["edges"]),
        "the case {name} inspected other edges"
    );
    let actual_reasons: BTreeMap<u8, String> = reason_map(decision)
        .into_iter()
        .map(|(request_id, reason)| (request_id, reason.to_owned()))
        .collect();
    assert_eq!(
        actual_reasons,
        expected_reasons(expected),
        "the case {name} reports other reasons"
    );

    let mut seen = BTreeSet::new();
    for request_id in selected_ids(decision) {
        assert!(seen.insert(request_id), "the case {name} selected twice");
    }
    for reason in &decision.reasons {
        assert!(
            seen.insert(reason.request_id.as_bytes()[0]),
            "the case {name} reported a request twice"
        );
    }
    for (request_id, shard_id) in shards {
        let request_state = state
            .request_state(shard_id, request(*request_id))
            .expect("the case request is in the state");
        if request_state == RequestState::Terminal {
            assert!(
                !seen.contains(request_id),
                "the case {name} reports a terminal request"
            );
        } else {
            assert!(
                seen.remove(request_id),
                "the case {name} has no reason for request {request_id}"
            );
        }
    }
    assert!(
        seen.is_empty(),
        "the case {name} reports an unknown request"
    );
}

fn run_case(case: &Value) -> Vec<Decision> {
    let name = text(case, "name");
    let config = SchedulerConfig::new(as_usize(&case["max_fan_in"]));
    let budget = SchedulerBudget::new(
        as_usize(&case["budget"]["max_positions"]),
        as_usize(&case["budget"]["max_edges"]),
    );
    let mut state = SchedulerState::with_config(config);
    let mut shards: BTreeMap<u8, oxkv_contracts::version::ShardId> = BTreeMap::new();

    for shard_value in case["shards"]
        .as_array()
        .expect("the case has a shard list")
    {
        let shard_id = shard_text(text(shard_value, "id"));
        if shard_value["blocked"]
            .as_bool()
            .expect("the shard names blocked")
        {
            state.block_shard(&shard_id);
        }
        for request_value in shard_value["requests"]
            .as_array()
            .expect("the shard has a request list")
        {
            let request_id = as_u8(&request_value["id"]);
            let key_id = key_text(text(request_value, "key"));
            let dependencies: Vec<RequestId> = request_value["deps"]
                .as_array()
                .expect("the request has a dependency list")
                .iter()
                .map(as_u8)
                .map(request)
                .collect();
            admit(
                &mut state,
                &shard_id,
                &key_id,
                request(request_id),
                &dependencies,
            );
            shards.insert(request_id, shard_id.clone());
        }
    }

    let mut decisions = Vec::new();
    let mut last_decision: Option<Decision> = None;
    for operation in case["operations"]
        .as_array()
        .expect("the case has an operation list")
    {
        if let Some(expected) = operation.get("decide") {
            let decision = state.decide(budget);
            assert_decision(name, expected, &decision, &state, &shards);
            last_decision = Some(decision.clone());
            decisions.push(decision);
        } else if operation.get("apply").is_some() {
            let decision = last_decision.as_ref().expect("an apply follows a decide");
            state
                .apply_selection(decision)
                .expect("the golden decision applies");
        } else if let Some(retire) = operation.get("retire") {
            for request_value in retire.as_array().expect("retire names a request list") {
                let request_id = as_u8(request_value);
                let shard_id = shards
                    .get(&request_id)
                    .expect("the retired request has a shard");
                state
                    .retire(shard_id, request(request_id))
                    .expect("the golden request retires");
            }
        } else {
            panic!("the case {name} has an unknown operation");
        }
    }
    decisions
}

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn scheduler_golden_vectors_are_pinned_and_replayable() {
    let file: Value = serde_json::from_str(include_str!("vectors/scheduler.json"))
        .expect("the vector file is JSON");
    assert_eq!(
        file["contract"].as_str(),
        Some("scheduler/v1"),
        "the vector contract changed"
    );
    let cases = file["cases"].as_array().expect("the file has cases");
    assert!(!cases.is_empty(), "the vector file has no case");
    for case in cases {
        let name = text(case, "name");
        let first = run_case(case);
        let second = run_case(case);
        assert_eq!(first, second, "the case {name} does not replay");
        assert!(!first.is_empty(), "the case {name} has no decision");
    }
}
