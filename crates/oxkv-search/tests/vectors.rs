//! Golden vectors for search order and named errors.
//!
//! A released vector does not change. A later case is additive.

mod support;

use oxkv_contracts::version::{
    CommitVersion, ScopeVersionVector, ShardId, TokenizerVersion, VersionVector,
};
use oxkv_search::{
    CursorInvalidReason, QueryFingerprint, QueryOrder, SearchError, SearchHit, SearchPin,
    SearchRequest, ShardResponse, execute,
};
use serde_json::Value;
use support::{hex, shard, vector};

fn bytes_from_hex(text: &str) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2),
        "the hex text has an even length"
    );
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let mut index = 0;
    while index < text.len() {
        let byte = u8::from_str_radix(&text[index..index + 2], 16).expect("the hex text is valid");
        bytes.push(byte);
        index += 2;
    }
    bytes
}

#[test]
fn merge_order_golden_vectors_are_pinned() {
    let file: Value =
        serde_json::from_str(include_str!("vectors/merge-order.json")).expect("valid JSON");
    assert_eq!(file["contract"], "search-result/v1");

    for case in file["cases"].as_array().expect("the case list") {
        let name = case["name"].as_str().expect("the case name");
        let order = match case["order"].as_str().expect("the order") {
            "relevance" => QueryOrder::Relevance,
            "declared" => QueryOrder::Declared,
            other => panic!("the case {name} has the unknown order {other}"),
        };
        let pin_version = case["pin_version"].as_u64().expect("the pin version");
        let limit = usize::try_from(case["limit"].as_u64().expect("the limit"))
            .expect("the limit fits in usize");

        let mut entries = Vec::new();
        let mut targets = Vec::new();
        let mut responses = Vec::new();
        for shard_value in case["shards"].as_array().expect("the shard list") {
            let id = ShardId::new(bytes_from_hex(
                shard_value["id"].as_str().expect("the shard id"),
            ));
            entries.push((id.clone(), CommitVersion::new(pin_version)));
            targets.push(id.clone());

            let mut hits = Vec::new();
            for hit_value in shard_value["hits"].as_array().expect("the hit list") {
                let key = bytes_from_hex(hit_value["key"].as_str().expect("the key"));
                let score = hit_value["score"].as_i64().expect("the score");
                let commit_version = hit_value["commit_version"]
                    .as_u64()
                    .expect("the commit version");
                let mut hit =
                    SearchHit::new(key, score, id.clone(), CommitVersion::new(commit_version));
                if order == QueryOrder::Declared {
                    hit = hit.with_sort_key(oxkv_search::SortValue::Integer(
                        hit_value["sort"].as_i64().expect("the sort value"),
                    ));
                }
                hits.push(hit);
            }
            responses.push(ShardResponse::served(
                id,
                CommitVersion::new(pin_version),
                hits,
            ));
        }

        let pin = SearchPin::new(
            ScopeVersionVector::from_vector(VersionVector::from_entries(entries)),
            QueryFingerprint::from_bytes([7; 32]),
        );
        let request = SearchRequest::new(pin, targets)
            .with_order(order)
            .with_limit(limit);
        let page = execute(&request, &responses).expect("the pin is readable");
        let actual: Vec<String> = page
            .hits()
            .iter()
            .map(|hit| hex(hit.canonical_key()))
            .collect();
        let expected: Vec<String> = case["expected"]
            .as_array()
            .expect("the expected list")
            .iter()
            .map(|key| key.as_str().expect("the key text").to_owned())
            .collect();
        assert_eq!(actual, expected, "the case {name} changed");

        let mut reversed = responses;
        reversed.reverse();
        let reversed_page = execute(&request, &reversed).expect("the pin is readable");
        assert_eq!(
            page, reversed_page,
            "the case {name} depends on arrival order"
        );
    }
}

#[test]
fn error_code_golden_vector_is_pinned() {
    let file: Value =
        serde_json::from_str(include_str!("vectors/errors.json")).expect("valid JSON");
    assert_eq!(file["contract"], "receipt-error/v1");

    let errors = vec![
        SearchError::PinExpired {
            requested: vector(&[(1, 10)]),
            oldest_readable: vector(&[(1, 20)]),
        },
        SearchError::PinMissingShard { shard_id: shard(1) },
        SearchError::CursorInvalid {
            reason: CursorInvalidReason::Malformed,
        },
        SearchError::FanoutExceeded {
            requested: 2,
            limit: 1,
        },
        SearchError::ShardUnavailable { shard_id: shard(1) },
        SearchError::IndexMismatch {
            shard_id: shard(1),
            expected: None,
            found: None,
        },
        SearchError::TokenizerMismatch {
            field: String::from("title"),
            expected: TokenizerVersion::new(1),
            found: TokenizerVersion::new(2),
        },
        SearchError::TokenizerUnknown {
            field: String::from("title"),
        },
        SearchError::TokenBudgetExceeded {
            required: 3,
            limit: 2,
        },
        SearchError::QueryBudgetExceeded {
            required: 3,
            limit: 2,
            coverage: None,
        },
        SearchError::UnorderedShard { shard_id: shard(1) },
        SearchError::DuplicateHit { key: vec![1] },
    ];

    let mut actual: Vec<String> = errors.iter().map(|error| error.code().to_owned()).collect();
    actual.sort();
    actual.dedup();
    let mut expected: Vec<String> = file["cases"]
        .as_array()
        .expect("the case list")
        .iter()
        .map(|case| case["code"].as_str().expect("the code").to_owned())
        .collect();
    expected.sort();
    expected.dedup();
    assert_eq!(actual, expected);
}
