//! Contract tests for the query cost model.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use oxkv_contracts::{
    CommitVersion, CostCursor, CursorToken, ManifestRevision, MapVersion, PolicyVersion,
    ScopeVersionVector, ShardId, SortTuple, SortValue, StatisticsVersion, VersionVector,
};
use oxkv_cost::budget::CostBudget;
use oxkv_cost::coverage::{CoverageDescriptor, PageRequest, ResultMode, ShardReply, assemble_page};
use oxkv_cost::cursor::CostCursorExt;
use oxkv_cost::error::{CostCode, CostError};
use oxkv_cost::explain::ExplainRecord;
use oxkv_cost::merge::{
    MergeOptions, MergeOrder, MergeOutput, QueryRow, ShardStream, compare_rows, merge_streams,
};
use oxkv_cost::plan::{CostInputs, MergeShape, QueryPlan, ShardPlan};
use serde_json::Value;
use std::cmp::Ordering;

fn load(text: &str) -> Value {
    serde_json::from_str(text).expect("the golden file is valid JSON")
}

fn hex_bytes(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| {
            let part = std::str::from_utf8(chunk).expect("the hex text is UTF-8");
            u8::from_str_radix(part, 16).expect("the hex text is valid")
        })
        .collect()
}

fn hex_of(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn hash32(text: &str) -> [u8; 32] {
    let bytes = hex_bytes(text);
    let mut hash = [0_u8; 32];
    hash.copy_from_slice(&bytes);
    hash
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name].as_str().expect("the field is text")
}

fn u64_of(value: &Value, name: &str) -> u64 {
    value[name].as_u64().expect("the field is an integer")
}

fn optional_u64(value: &Value, name: &str) -> Option<u64> {
    value.get(name).and_then(Value::as_u64)
}

fn merge_shape(value: &str) -> MergeShape {
    match value {
        "single_shard" => MergeShape::SingleShard,
        "search" => MergeShape::Search,
        "general" => MergeShape::General,
        other => panic!("unknown merge shape: {other}"),
    }
}

fn merge_order(value: &str) -> MergeOrder {
    match value {
        "search" => MergeOrder::Search,
        "general" => MergeOrder::General,
        other => panic!("unknown merge order: {other}"),
    }
}

fn pin(value: &Value) -> ScopeVersionVector {
    let entries =
        value
            .as_object()
            .expect("the pin is an object")
            .iter()
            .map(|(shard, version)| {
                (
                    ShardId::new(hex_bytes(shard)),
                    CommitVersion::new(version.as_u64().expect("the version is an integer")),
                )
            });
    ScopeVersionVector::from_vector(VersionVector::from_entries(entries))
}

fn build_plan(case: &Value) -> QueryPlan {
    let input = &case["input"];
    let shards = input["shards"]
        .as_array()
        .expect("the input has shards")
        .iter()
        .map(|shard| {
            ShardPlan::new(
                ShardId::new(hex_bytes(text(shard, "shard_id_hex"))),
                u64_of(shard, "term_probes"),
                u64_of(shard, "posting_keys"),
                u64_of(shard, "estimated_rows"),
                u64_of(shard, "token_count"),
            )
        })
        .collect();
    let inputs = CostInputs::new(
        ManifestRevision::new(u64_of(input, "manifest_revision")),
        PolicyVersion::from_bytes(hash32(text(input, "policy_version_hex"))),
        MapVersion::new(u64_of(input, "map_version")),
        optional_u64(input, "statistics_version").map(StatisticsVersion::new),
    );
    QueryPlan::new(
        inputs,
        text(input, "policy_normalization"),
        merge_shape(text(input, "merge_shape")),
        shards,
        u64_of(input, "token_budget"),
    )
    .expect("the plan is valid")
}

fn build_budget(value: &Value) -> CostBudget {
    CostBudget::new(
        u64_of(value, "max_cost_milli"),
        u32::try_from(u64_of(value, "max_fanout")).expect("the fan-out fits u32"),
        u64_of(value, "max_tokens"),
    )
}

fn build_explain(case: &Value) -> ExplainRecord {
    ExplainRecord::new(build_plan(case), build_budget(&case["budget"]))
}

fn build_row(shard_id: &ShardId, value: &Value) -> QueryRow {
    let sort_key = value
        .get("sort_key_ints")
        .and_then(Value::as_array)
        .map(|values| {
            SortTuple::new(
                values
                    .iter()
                    .map(|value| {
                        SortValue::Integer(value.as_i64().expect("the sort value is an integer"))
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    QueryRow::new(
        shard_id.clone(),
        hex_bytes(text(value, "key_hex")),
        value["score"].as_i64().expect("the score is an integer"),
        sort_key,
        hex_bytes(text(value, "payload_hex")),
    )
}

fn build_streams(case: &Value) -> Vec<ShardStream> {
    case["streams"]
        .as_array()
        .expect("the case has streams")
        .iter()
        .map(|stream| {
            let shard_id = ShardId::new(hex_bytes(text(stream, "shard_id_hex")));
            let rows = stream["rows"]
                .as_array()
                .expect("the stream has rows")
                .iter()
                .map(|row| build_row(&shard_id, row))
                .collect();
            ShardStream::new(shard_id, rows).with_latency_ticks(u64_of(stream, "latency_ticks"))
        })
        .collect()
}

fn run_merge(case: &Value) -> MergeOutput {
    merge_streams(
        &build_streams(case),
        MergeOptions::new(
            merge_order(text(case, "order")),
            usize::try_from(u64_of(case, "limit")).expect("the limit fits usize"),
            16,
        ),
    )
    .expect("the golden merge succeeds")
}

fn build_reply(value: &Value) -> ShardReply {
    let shard_id = ShardId::new(hex_bytes(text(value, "shard_id_hex")));
    let rows = value["rows"]
        .as_array()
        .expect("the reply has rows")
        .iter()
        .map(|row| build_row(&shard_id, row))
        .collect();
    let latency_ticks = u64_of(value, "latency_ticks");
    if let Some(version) = optional_u64(value, "served_version") {
        ShardReply::served(shard_id, CommitVersion::new(version), rows)
            .with_latency_ticks(latency_ticks)
    } else {
        ShardReply::unavailable(shard_id).with_latency_ticks(latency_ticks)
    }
}

fn run_coverage(case: &Value) -> oxkv_cost::coverage::QueryPage {
    let pin = pin(&case["pin"]);
    let budget = CostBudget::new(100_000, 16, 1000);
    let request = PageRequest::new(
        ResultMode::Coverage,
        &pin,
        MergeOrder::Search,
        50,
        &budget,
        None,
    );
    let replies: Vec<ShardReply> = case["replies"]
        .as_array()
        .expect("the case has replies")
        .iter()
        .map(build_reply)
        .collect();
    assemble_page(&request, &replies).expect("the golden coverage page succeeds")
}

fn expected_keys(case: &Value) -> Vec<Vec<u8>> {
    case["expected"]["keys_hex"]
        .as_array()
        .expect("the expected keys are an array")
        .iter()
        .map(|key| hex_bytes(key.as_str().expect("the key is text")))
        .collect()
}

#[test]
fn explain_golden_vectors_are_pinned() {
    let file = load(include_str!("vectors/explain.json"));
    assert_eq!(text(&file, "contract"), "cost/v1");
    for case in file["cases"].as_array().expect("the file has cases") {
        let record = build_explain(case);
        assert_eq!(
            record.to_canonical_json(),
            text(&case["expected"], "canonical_json"),
            "the canonical explain bytes changed for {}",
            text(case, "name")
        );
        assert_eq!(
            record.to_text(),
            text(&case["expected"], "text"),
            "the explain text changed for {}",
            text(case, "name")
        );
    }
}

#[test]
fn merge_golden_vectors_are_pinned() {
    let file = load(include_str!("vectors/merge.json"));
    assert_eq!(text(&file, "contract"), "cost/v1");
    for case in file["cases"].as_array().expect("the file has cases") {
        let output = run_merge(case);
        let keys: Vec<Vec<u8>> = output
            .rows
            .iter()
            .map(|row| row.canonical_key.clone())
            .collect();
        assert_eq!(
            keys,
            expected_keys(case),
            "the merge order changed for {}",
            text(case, "name")
        );
        let expected_frontiers = case["expected"]["frontiers"]
            .as_object()
            .expect("the expected frontiers are an object");
        for (shard, frontier) in &output.frontiers {
            let expected = expected_frontiers
                .get(&hex_of(shard.as_bytes()))
                .expect("the expected frontier is present")
                .as_u64()
                .expect("the frontier is an integer");
            assert_eq!(frontier.keys_examined, expected);
        }
    }
}

#[test]
fn coverage_golden_vectors_are_pinned() {
    let file = load(include_str!("vectors/coverage.json"));
    assert_eq!(text(&file, "contract"), "cost/v1");
    for case in file["cases"].as_array().expect("the file has cases") {
        let page = run_coverage(case);
        let coverage = page.coverage.expect("the page has a descriptor");
        let expected = &case["expected"];
        assert_eq!(
            coverage.to_canonical_json(),
            text(expected, "canonical_json"),
            "the coverage descriptor changed for {}",
            text(case, "name")
        );
        assert_eq!(
            coverage.is_complete(),
            expected["complete"].as_bool().expect("complete is a bool")
        );
        assert_eq!(
            page.partial,
            expected["partial"].as_bool().expect("partial is a bool")
        );
        let expected_keys: Vec<Vec<u8>> = expected["rows_hex"]
            .as_array()
            .expect("the expected rows are an array")
            .iter()
            .map(|key| hex_bytes(key.as_str().expect("the key is text")))
            .collect();
        assert_eq!(
            page.rows
                .iter()
                .map(|row| row.canonical_key.clone())
                .collect::<Vec<_>>(),
            expected_keys
        );
    }
}

#[test]
fn explain_output_is_byte_identical_on_two_runs() {
    let file = load(include_str!("vectors/explain.json"));
    let case = &file["cases"][0];
    assert_eq!(
        build_explain(case).to_canonical_json(),
        build_explain(case).to_canonical_json()
    );
    assert_eq!(build_explain(case).to_text(), build_explain(case).to_text());
}

#[test]
fn explain_output_is_stable_under_injected_latency() {
    let file = load(include_str!("vectors/coverage.json"));
    let case = &file["cases"][1];
    let before = build_explain(&load(include_str!("vectors/explain.json"))["cases"][0]);
    let pin = pin(&case["pin"]);
    let budget = CostBudget::new(100_000, 16, 1000);
    let request = PageRequest::new(
        ResultMode::Exact,
        &pin,
        MergeOrder::Search,
        10,
        &budget,
        None,
    );
    let replies: Vec<ShardReply> = case["replies"]
        .as_array()
        .expect("the case has replies")
        .iter()
        .map(build_reply)
        .collect();
    assemble_page(&request, &replies).expect("the page succeeds");
    let after = build_explain(&load(include_str!("vectors/explain.json"))["cases"][0]);
    assert_eq!(before.to_canonical_json(), after.to_canonical_json());
    assert_eq!(before.to_text(), after.to_text());
}

#[test]
fn budget_exhaustion_returns_query_budget_exceeded() {
    let file = load(include_str!("vectors/explain.json"));
    let plan = build_plan(&file["cases"][1]);
    let budget = CostBudget::new(100, 4, 1000);
    let error = budget
        .admit(&plan)
        .expect_err("the cost is over the budget");
    assert_eq!(error.code(), CostCode::QueryBudgetExceeded);
    let pin = pin(&serde_json::json!({"01": 5}));
    let page_budget = CostBudget::new(1, 4, 1000);
    let request = PageRequest::new(
        ResultMode::Exact,
        &pin,
        MergeOrder::Search,
        10,
        &page_budget,
        None,
    );
    let replies = vec![ShardReply::served(
        ShardId::new(vec![1]),
        CommitVersion::new(5),
        vec![QueryRow::new(
            ShardId::new(vec![1]),
            b"a".to_vec(),
            1,
            SortTuple::default(),
            Vec::new(),
        )],
    )];
    let error = assemble_page(&request, &replies).expect_err("the page is over the budget");
    assert_eq!(error.code(), CostCode::QueryBudgetExceeded);
    assert!(error.coverage().is_some());
}

#[test]
fn fanout_limit_returns_search_fanout_exceeded() {
    let file = load(include_str!("vectors/explain.json"));
    let plan = build_plan(&file["cases"][1]);
    let budget = CostBudget::new(100_000, 2, 1000);
    let error = budget
        .admit(&plan)
        .expect_err("the fan-out is over the limit");
    assert_eq!(error.code(), CostCode::SearchFanoutExceeded);
    let pin = pin(&serde_json::json!({"01": 5, "02": 5, "03": 5}));
    let request = PageRequest::new(
        ResultMode::Coverage,
        &pin,
        MergeOrder::Search,
        10,
        &budget,
        None,
    );
    let replies = vec![
        ShardReply::unavailable(ShardId::new(vec![1])),
        ShardReply::unavailable(ShardId::new(vec![2])),
        ShardReply::unavailable(ShardId::new(vec![3])),
    ];
    let error = assemble_page(&request, &replies).expect_err("the fan-out is over the limit");
    assert_eq!(error.code(), CostCode::SearchFanoutExceeded);
}

#[test]
fn search_merge_breaks_deliberate_ties_by_canonical_key() {
    let row = |shard: u8, key: &[u8]| {
        QueryRow::new(
            ShardId::new(vec![shard]),
            key.to_vec(),
            7,
            SortTuple::default(),
            Vec::new(),
        )
    };
    let streams = vec![
        ShardStream::new(ShardId::new(vec![9]), vec![row(9, b"b")]).with_latency_ticks(100),
        ShardStream::new(ShardId::new(vec![1]), vec![row(1, b"a")]).with_latency_ticks(0),
    ];
    let output = merge_streams(&streams, MergeOptions::new(MergeOrder::Search, 10, 4))
        .expect("the streams merge");
    let keys: Vec<Vec<u8>> = output
        .rows
        .iter()
        .map(|row| row.canonical_key.clone())
        .collect();
    assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec()]);
}

#[test]
fn general_merge_breaks_deliberate_ties_by_canonical_key() {
    let row = |shard: u8, key: &[u8], sort: i64| {
        QueryRow::new(
            ShardId::new(vec![shard]),
            key.to_vec(),
            0,
            SortTuple::new(vec![SortValue::Integer(sort)]),
            Vec::new(),
        )
    };
    let streams = vec![
        ShardStream::new(ShardId::new(vec![9]), vec![row(9, b"b", 1)]),
        ShardStream::new(ShardId::new(vec![1]), vec![row(1, b"a", 1)]),
        ShardStream::new(ShardId::new(vec![2]), vec![row(2, b"c", 0)]),
    ];
    let output = merge_streams(&streams, MergeOptions::new(MergeOrder::General, 10, 4))
        .expect("the streams merge");
    let keys: Vec<Vec<u8>> = output
        .rows
        .iter()
        .map(|row| row.canonical_key.clone())
        .collect();
    assert_eq!(keys, vec![b"c".to_vec(), b"a".to_vec(), b"b".to_vec()]);
}

#[test]
fn the_shard_identifier_is_not_a_semantic_tie_break() {
    let left = QueryRow::new(
        ShardId::new(vec![9]),
        b"a".to_vec(),
        1,
        SortTuple::default(),
        Vec::new(),
    );
    let right = QueryRow::new(
        ShardId::new(vec![1]),
        b"a".to_vec(),
        1,
        SortTuple::default(),
        Vec::new(),
    );
    assert_eq!(
        compare_rows(MergeOrder::Search, &left, &right),
        Ordering::Equal
    );
    assert_eq!(
        compare_rows(MergeOrder::General, &left, &right),
        Ordering::Equal
    );
}

#[test]
fn a_slow_shard_does_not_change_the_merge_order() {
    let row = |shard: u8, key: &[u8], score: i64| {
        QueryRow::new(
            ShardId::new(vec![shard]),
            key.to_vec(),
            score,
            SortTuple::default(),
            Vec::new(),
        )
    };
    let first = vec![
        ShardStream::new(
            ShardId::new(vec![1]),
            vec![row(1, b"a", 9), row(1, b"d", 7)],
        )
        .with_latency_ticks(1),
        ShardStream::new(
            ShardId::new(vec![2]),
            vec![row(2, b"b", 9), row(2, b"c", 7)],
        )
        .with_latency_ticks(500),
    ];
    let second = vec![
        ShardStream::new(
            ShardId::new(vec![2]),
            vec![row(2, b"c", 7), row(2, b"b", 9)],
        )
        .with_latency_ticks(0),
        ShardStream::new(
            ShardId::new(vec![1]),
            vec![row(1, b"d", 7), row(1, b"a", 9)],
        )
        .with_latency_ticks(200),
    ];
    let left = merge_streams(&first, MergeOptions::new(MergeOrder::Search, 10, 4))
        .expect("the streams merge");
    let right = merge_streams(&second, MergeOptions::new(MergeOrder::Search, 10, 4))
        .expect("the streams merge");
    let left_keys: Vec<Vec<u8>> = left
        .rows
        .iter()
        .map(|row| row.canonical_key.clone())
        .collect();
    let right_keys: Vec<Vec<u8>> = right
        .rows
        .iter()
        .map(|row| row.canonical_key.clone())
        .collect();
    assert_eq!(left_keys, right_keys);
    assert_eq!(left.frontiers, right.frontiers);
}

#[test]
fn exact_mode_with_one_unavailable_shard_returns_an_error_and_no_data() {
    let pin = pin(&serde_json::json!({"01": 5, "02": 5}));
    let budget = CostBudget::new(100_000, 4, 1000);
    let request = PageRequest::new(
        ResultMode::Exact,
        &pin,
        MergeOrder::Search,
        10,
        &budget,
        None,
    );
    let replies = vec![
        ShardReply::served(
            ShardId::new(vec![1]),
            CommitVersion::new(5),
            vec![QueryRow::new(
                ShardId::new(vec![1]),
                b"a".to_vec(),
                1,
                SortTuple::default(),
                Vec::new(),
            )],
        ),
        ShardReply::unavailable(ShardId::new(vec![2])),
    ];
    let error = assemble_page(&request, &replies).expect_err("the exact page fails");
    assert_eq!(error.code(), CostCode::ScopeShardUnreachable);
    assert_eq!(
        error
            .coverage()
            .expect("the error has coverage")
            .missing_count(),
        1
    );
}

#[test]
fn coverage_page_does_not_advance_an_exact_cursor() {
    let pin = pin(&serde_json::json!({"01": 5, "02": 5}));
    let budget = CostBudget::new(100_000, 4, 1000);
    let replies = vec![
        ShardReply::served(
            ShardId::new(vec![1]),
            CommitVersion::new(5),
            vec![QueryRow::new(
                ShardId::new(vec![1]),
                b"a".to_vec(),
                1,
                SortTuple::default(),
                Vec::new(),
            )],
        ),
        ShardReply::served(
            ShardId::new(vec![2]),
            CommitVersion::new(5),
            vec![QueryRow::new(
                ShardId::new(vec![2]),
                b"b".to_vec(),
                2,
                SortTuple::default(),
                Vec::new(),
            )],
        ),
    ];
    let exact_request = PageRequest::new(
        ResultMode::Exact,
        &pin,
        MergeOrder::Search,
        10,
        &budget,
        None,
    );
    let exact_page = assemble_page(&exact_request, &replies).expect("the exact page succeeds");
    let cursor = exact_page.cursor.expect("the exact page has a cursor");
    let before = cursor.to_token();
    let coverage_request = PageRequest::new(
        ResultMode::Coverage,
        &pin,
        MergeOrder::Search,
        10,
        &budget,
        Some(&cursor),
    );
    let coverage_page =
        assemble_page(&coverage_request, &replies).expect("the coverage page succeeds");
    assert!(coverage_page.cursor.is_none());
    assert_eq!(cursor.to_token(), before);
}

#[test]
fn coverage_mode_is_opt_in_and_marks_the_page_partial() {
    assert_eq!(ResultMode::default(), ResultMode::Exact);
    let pin = pin(&serde_json::json!({"01": 5}));
    let budget = CostBudget::new(100_000, 4, 1000);
    let request = PageRequest::new(
        ResultMode::Coverage,
        &pin,
        MergeOrder::Search,
        10,
        &budget,
        None,
    );
    let replies = vec![ShardReply::served(
        ShardId::new(vec![1]),
        CommitVersion::new(5),
        Vec::new(),
    )];
    let page = assemble_page(&request, &replies).expect("the coverage page succeeds");
    assert!(page.partial);
    assert!(page.coverage.expect("the page has coverage").is_complete());
}

#[test]
fn cost_cursor_expiry_reports_budget_exhaustion_or_pin_expiry() {
    let pin_vector = pin(&serde_json::json!({"01": 5}));
    let budget = CostBudget::new(1000, 4, 100);
    let mut cursor = CostCursor::from_budget(&pin_vector, &budget);
    cursor.remaining_budget = 0;
    let expiry = cursor
        .expiry(&pin_vector)
        .expect_err("the cursor has no budget");
    assert_eq!(expiry.code(), CostCode::QueryBudgetExceeded);
    let cursor = CostCursor::from_budget(&pin_vector, &budget);
    let oldest = pin(&serde_json::json!({"01": 6}));
    let expiry = cursor.expiry(&oldest).expect_err("the pin is too old");
    assert_eq!(expiry.code(), CostCode::SearchPinExpired);
}

#[test]
fn a_duplicate_key_with_different_content_is_rejected() {
    let row = |shard: u8, payload: &[u8]| {
        QueryRow::new(
            ShardId::new(vec![shard]),
            b"a".to_vec(),
            1,
            SortTuple::default(),
            payload.to_vec(),
        )
    };
    let streams = vec![
        ShardStream::new(ShardId::new(vec![1]), vec![row(1, b"one")]),
        ShardStream::new(ShardId::new(vec![2]), vec![row(2, b"two")]),
    ];
    let error = merge_streams(&streams, MergeOptions::new(MergeOrder::Search, 10, 2))
        .expect_err("the key has two contents");
    assert_eq!(error.code(), CostCode::InternalInvariantViolation);
    assert_eq!(error, CostError::DuplicateKey { key: b"a".to_vec() });
}

#[test]
fn a_coverage_descriptor_lists_versions_and_missing_shards() {
    let mut answered = BTreeMap::new();
    answered.insert(ShardId::new(vec![1]), CommitVersion::new(5));
    let mut missing = std::collections::BTreeSet::new();
    missing.insert(ShardId::new(vec![2]));
    let descriptor = CoverageDescriptor::new(answered, missing);
    assert_eq!(
        descriptor.served_version(&ShardId::new(vec![1])),
        Some(CommitVersion::new(5))
    );
    assert_eq!(descriptor.missing_count(), 1);
    assert!(!descriptor.is_complete());
    assert_eq!(descriptor.answered_count(), 1);
}
