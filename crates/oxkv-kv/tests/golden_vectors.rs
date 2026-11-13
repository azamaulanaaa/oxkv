//! Golden vector tests for cursor tokens and operation logs.

mod support;

use oxkv_contracts::CursorDirection;
use oxkv_contracts::version::{CommitVersion, ShardId};
use oxkv_kv::{Batch, Cursor, Operation};

const VECTORS: &str = include_str!("vectors/kv.json");

#[test]
fn cursor_tokens_match_the_golden_bytes() {
    let vectors: serde_json::Value =
        serde_json::from_str(VECTORS).expect("the vector file is valid JSON");
    assert_eq!(vectors["contract"], "kv/v1");
    let mut count = 0;
    for case in vectors["cases"]
        .as_array()
        .expect("the vector file has a case list")
    {
        if case["kind"] != "cursor" {
            continue;
        }
        let version = CommitVersion::new(
            case["snapshot_version"]
                .as_u64()
                .expect("the cursor case has a version"),
        );
        let direction = match case["direction"]
            .as_str()
            .expect("the cursor has a direction")
        {
            "ascending" => CursorDirection::Ascending,
            "descending" => CursorDirection::Descending,
            other => panic!("the cursor direction is unknown: {other}"),
        };
        let cursor = match case["position_hex"].as_str() {
            Some(text) => Cursor::after(version, support::unhex(text), direction),
            None => Cursor::start(version, direction),
        };
        let expected = support::unhex(
            case["expected_hex"]
                .as_str()
                .expect("the cursor case has expected bytes"),
        );
        assert_eq!(
            cursor.to_token(),
            expected,
            "the token bytes changed for {}",
            case["name"]
        );
        assert_eq!(
            Cursor::from_token(&expected),
            Ok(cursor),
            "the token does not decode for {}",
            case["name"]
        );
        count += 1;
    }
    assert_eq!(count, 3);
}

#[test]
fn operation_logs_match_the_golden_bytes() {
    let vectors: serde_json::Value =
        serde_json::from_str(VECTORS).expect("the vector file is valid JSON");
    let mut count = 0;
    for case in vectors["cases"]
        .as_array()
        .expect("the vector file has a case list")
    {
        if case["kind"] != "batch" {
            continue;
        }
        let shard = ShardId::new(support::unhex(
            case["shard_hex"]
                .as_str()
                .expect("the batch case has a shard"),
        ));
        let version = CommitVersion::new(
            case["commit_version"]
                .as_u64()
                .expect("the batch case has a version"),
        );
        let mut operations = Vec::new();
        for operation in case["operations"]
            .as_array()
            .expect("the batch case has an operation list")
        {
            let key = support::unhex(
                operation["key_hex"]
                    .as_str()
                    .expect("the operation has a key"),
            );
            match operation["op"].as_str().expect("the operation has a kind") {
                "put" => operations.push(Operation::put(
                    key,
                    support::unhex(
                        operation["value_hex"]
                            .as_str()
                            .expect("the put operation has a value"),
                    ),
                )),
                "delete" => operations.push(Operation::delete(key)),
                other => panic!("the operation kind is unknown: {other}"),
            }
        }
        let batch = Batch::new(shard, version, operations);
        let expected = support::unhex(
            case["expected_hex"]
                .as_str()
                .expect("the batch case has expected bytes"),
        );
        assert_eq!(
            batch.to_bytes(),
            expected,
            "the log bytes changed for {}",
            case["name"]
        );
        assert_eq!(
            Batch::from_bytes(&expected),
            Ok(batch),
            "the log does not decode for {}",
            case["name"]
        );
        count += 1;
    }
    assert_eq!(count, 2);
}
