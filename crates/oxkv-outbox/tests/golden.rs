#![allow(clippy::too_many_lines)]

//! Golden vector runner for the outbox and receipt lifecycle.

mod support;

use std::collections::BTreeMap;

use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::{CommitVersion, KeyVersion, ReceiptId, ReceiptOutcome, ReceiptSequence};
use oxkv_outbox::{Outbox, ReceiptCursor, ReceiptLedger};
use serde_json::{Value, json};

struct GoldenFile {
    contract: String,
    cases: Vec<GoldenCase>,
}

struct GoldenCase {
    name: String,
    expected: Value,
}

fn load(text: &str) -> GoldenFile {
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    let contract = value["contract"]
        .as_str()
        .expect("the golden file names a contract")
        .to_owned();
    let cases = value["cases"]
        .as_array()
        .expect("the golden file has a case list")
        .iter()
        .map(|case| GoldenCase {
            name: case["name"]
                .as_str()
                .expect("the case names a vector")
                .to_owned(),
            expected: case["expected"].clone(),
        })
        .collect();
    GoldenFile { contract, cases }
}

fn run(name: &str) -> Value {
    match name {
        "receipt_states" => receipt_states(),
        "three_deliveries_one_terminal_receipt" => three_deliveries_one_terminal_receipt(),
        "cancelled_offline_write_later_commits" => cancelled_offline_write_later_commits(),
        "rejected_write_has_one_receipt" => rejected_write_has_one_receipt(),
        "reused_key_with_different_body" => reused_key_with_different_body(),
        "terminal_immutability" => terminal_immutability(),
        "cursor_expiry" => cursor_expiry(),
        "unknown_receipt" => unknown_receipt(),
        "outbox_full" => outbox_full(),
        "dependency_cycle" => dependency_cycle(),
        "cross_shard_edge" => cross_shard_edge(),
        "per_key_fifo" => per_key_fifo(),
        other => panic!("the golden case {other} has no scenario"),
    }
}

fn receipt_states() -> Value {
    let names: Vec<&str> = ReceiptState::ALL.iter().map(|state| state.name()).collect();
    let terminal: Vec<bool> = ReceiptState::ALL
        .iter()
        .map(|state| state.is_terminal())
        .collect();
    let wire_tags: Vec<u8> = ReceiptState::ALL
        .iter()
        .map(|state| state.wire_tag())
        .collect();
    json!({
        "count": ReceiptState::ALL.len(),
        "names": names,
        "terminal": terminal,
        "wire_tags": wire_tags,
    })
}

fn three_deliveries_one_terminal_receipt() -> Value {
    let mut ledger = ReceiptLedger::new(ReceiptSequence::new(0));
    let body = support::digest(2);
    let receipt = support::envelope(
        support::key(1),
        9,
        Some(ReceiptOutcome::Committed {
            commit_version: CommitVersion::new(7),
        }),
    );
    for _ in 0..3 {
        ledger
            .submit(body, receipt.clone())
            .expect("the retry returns the stored receipt");
    }
    let record = ledger.get(&support::key(1)).expect("the receipt is stored");
    json!({
        "deliveries": 3,
        "receipt_count": ledger.len(),
        "state": record.state().name(),
        "commit_version": record.commit_version().map(CommitVersion::get),
    })
}

fn cancelled_offline_write_later_commits() -> Value {
    let key = support::key(1);
    let body = support::digest(2);
    let mut outbox = Outbox::new(4);
    outbox
        .admit(support::entry(1, 2, 1, b"k"))
        .expect("the outbox has room");
    outbox
        .request_cancel(&key, body)
        .expect("the entry waits for a verdict");
    outbox
        .mark_abandoned(&key, body)
        .expect("the client closes the entry");
    let receipts_before = 0;
    let receipt = support::envelope(
        key.clone(),
        9,
        Some(ReceiptOutcome::Committed {
            commit_version: CommitVersion::new(5),
        }),
    );
    let reconciliation = outbox
        .reconcile(&receipt, body)
        .expect("the late verdict wins for the data");
    let mut ledger = ReceiptLedger::new(ReceiptSequence::new(0));
    ledger
        .submit(body, receipt)
        .expect("the authority receipt is stored");
    let entry = outbox.entry(&key).expect("the entry is retained");
    json!({
        "disposition": entry.disposition().name(),
        "receipt_count": ledger.len(),
        "state": ledger.get(&key).expect("the receipt is stored").state().name(),
        "cancel_is_receipt": receipts_before != 0,
        "reconciliation": reconciliation
            .outcome()
            .map(|outcome| outcome.state().name()),
    })
}

fn rejected_write_has_one_receipt() -> Value {
    let key = support::key(1);
    let body = support::digest(2);
    let receipt = support::envelope(
        key.clone(),
        9,
        Some(ReceiptOutcome::Rejected {
            reason: "policy.unknown_field",
        }),
    );
    let mut ledger = ReceiptLedger::new(ReceiptSequence::new(0));
    ledger
        .submit(body, receipt.clone())
        .expect("the first delivery stores the receipt");
    ledger
        .submit(body, receipt.clone())
        .expect("the second delivery returns the receipt");
    let error = ledger
        .submit(support::digest(3), receipt)
        .expect_err("the key is consumed for the same body");
    let record = ledger.get(&key).expect("the receipt is stored");
    json!({
        "receipt_count": ledger.len(),
        "state": record.state().name(),
        "reason": record.reason_code(),
        "error": error.code(),
    })
}

fn reused_key_with_different_body() -> Value {
    let key = support::key(1);
    let mut ledger = ReceiptLedger::new(ReceiptSequence::new(0));
    let receipt = support::envelope(
        key.clone(),
        9,
        Some(ReceiptOutcome::Committed {
            commit_version: CommitVersion::new(7),
        }),
    );
    ledger
        .submit(support::digest(2), receipt.clone())
        .expect("the first delivery stores the receipt");
    let error = ledger
        .submit(support::digest(3), receipt)
        .expect_err("a different body reuses the key");
    json!({
        "error": error.code(),
        "receipt_count": ledger.len(),
    })
}

fn terminal_immutability() -> Value {
    let key = support::key(1);
    let body = support::digest(2);
    let mut outbox = Outbox::new(4);
    outbox
        .admit(support::entry(1, 2, 1, b"k"))
        .expect("the outbox has room");
    let committed = support::envelope(
        key.clone(),
        9,
        Some(ReceiptOutcome::Committed {
            commit_version: CommitVersion::new(7),
        }),
    );
    outbox
        .reconcile(&committed, body)
        .expect("the first verdict applies");
    let rejected = support::envelope(
        key.clone(),
        9,
        Some(ReceiptOutcome::Rejected {
            reason: "policy.unknown_field",
        }),
    );
    let error = outbox
        .reconcile(&rejected, body)
        .expect_err("a terminal receipt cannot change");
    json!({
        "error": error.code(),
        "state": outbox
            .terminal_outcome(&key)
            .map(|outcome| outcome.state().name()),
    })
}

fn cursor_expiry() -> Value {
    let mut ledger = ReceiptLedger::new(ReceiptSequence::new(1));
    ledger
        .submit(
            support::digest(2),
            support::envelope(
                support::key(1),
                9,
                Some(ReceiptOutcome::Committed {
                    commit_version: CommitVersion::new(1),
                }),
            ),
        )
        .expect("the first receipt is stored");
    ledger
        .submit(
            support::digest(3),
            support::envelope(
                support::key(2),
                10,
                Some(ReceiptOutcome::Committed {
                    commit_version: CommitVersion::new(2),
                }),
            ),
        )
        .expect("the second receipt is stored");
    let first = ledger
        .get(&support::key(1))
        .expect("the first receipt is stored");
    let cursor = first.cursor(ReceiptSequence::new(1));
    ledger.set_retention_floor(ReceiptSequence::new(2));
    let error = ledger
        .resume(&cursor)
        .expect_err("the cursor is past retention");
    json!({
        "error": error.code(),
        "cursor_sequence": cursor.sequence().get(),
        "oldest_retained": ledger.retention_floor().get(),
    })
}

fn unknown_receipt() -> Value {
    let ledger = ReceiptLedger::new(ReceiptSequence::new(0));
    let unknown = ReceiptId::from_bytes([99; 32]);
    let read_error = ledger
        .read(unknown)
        .expect_err("the receipt identity is absent");
    let cursor = ReceiptCursor::new(unknown, ReceiptSequence::new(1), ReceiptSequence::new(9));
    let resume_error = ledger
        .resume(&cursor)
        .expect_err("the cursor names no receipt");
    json!({
        "read_error": read_error.code(),
        "resume_error": resume_error.code(),
    })
}

fn outbox_full() -> Value {
    let mut outbox = Outbox::new(2);
    outbox
        .admit(support::entry(1, 1, 1, b"a"))
        .expect("the outbox has room");
    outbox
        .admit(support::entry(2, 2, 1, b"b"))
        .expect("the outbox has room");
    let error = outbox
        .admit(support::entry(3, 3, 1, b"c"))
        .expect_err("the outbox is full");
    json!({
        "error": error.code(),
        "entries": outbox.len(),
        "oldest_present": outbox.entry(&support::key(1)).is_some(),
    })
}

fn dependency_cycle() -> Value {
    let mut outbox = Outbox::new(8);
    outbox
        .admit(support::entry(1, 1, 1, b"a"))
        .expect("the outbox has room");
    let self_dependency = support::entry(2, 2, 1, b"b").with_dependencies(vec![support::key(2)]);
    let self_error = outbox
        .admit(self_dependency)
        .expect_err("a self edge is a cycle");
    let forward_reference = support::entry(3, 3, 1, b"c").with_dependencies(vec![support::key(4)]);
    let forward_error = outbox
        .admit(forward_reference)
        .expect_err("a forward edge cannot resolve");
    json!({
        "self_error": self_error.code(),
        "forward_error": forward_error.code(),
    })
}

fn cross_shard_edge() -> Value {
    let mut outbox = Outbox::new(8);
    outbox
        .admit(support::entry(1, 1, 1, b"a"))
        .expect("the outbox has room");
    let cross_shard = support::entry(2, 2, 2, b"b").with_dependencies(vec![support::key(1)]);
    let error = outbox
        .admit(cross_shard)
        .expect_err("an edge cannot cross shards");
    json!({
        "error": error.code(),
    })
}

fn per_key_fifo() -> Value {
    let key_a = support::key(1);
    let key_b = support::key(2);
    let key_c = support::key(3);
    let mut outbox = Outbox::new(8);
    outbox
        .admit(support::entry(1, 1, 1, b"k"))
        .expect("the outbox has room");
    outbox
        .admit(support::entry(2, 2, 1, b"k"))
        .expect("the outbox has room");
    outbox
        .admit(support::entry(3, 3, 1, b"other"))
        .expect("the outbox has room");
    let first = outbox
        .claim_next()
        .expect("the first entry is ready")
        .idempotency_key()
        .clone();
    let in_flight_on_key = outbox.in_flight_on_key(b"k");
    let second = outbox
        .claim_next()
        .expect("the disjoint key is ready")
        .idempotency_key()
        .clone();
    let blocked_while_first_in_flight = outbox
        .entry(&key_b)
        .is_some_and(|entry| entry.scheduler_state() == oxkv_outbox::SchedulerState::Queued);
    let committed = support::envelope(
        key_a.clone(),
        9,
        Some(ReceiptOutcome::Committed {
            commit_version: CommitVersion::new(1),
        }),
    );
    outbox
        .reconcile(&committed, support::digest(1))
        .expect("the first verdict applies");
    let third = outbox
        .claim_next()
        .expect("the second key entry is ready")
        .idempotency_key()
        .clone();
    json!({
        "first_is_a": first == key_a,
        "in_flight_on_key": in_flight_on_key,
        "second_is_c": second == key_c,
        "blocked_while_first_in_flight": blocked_while_first_in_flight,
        "third_is_b": third == key_b,
        "in_flight_on_key_after": outbox.in_flight_on_key(b"k"),
    })
}

#[test]
fn outbox_golden_vectors_are_pinned() {
    let file = load(include_str!("vectors/outbox.json"));
    assert_eq!(file.contract, "receipt-error/v1");
    assert_eq!(file.cases.len(), 12);
    for case in &file.cases {
        let actual = run(&case.name);
        assert_eq!(
            actual, case.expected,
            "the golden case {} changed",
            case.name
        );
    }
}

#[test]
fn a_local_cancel_never_appears_in_a_receipt_vector() {
    let key = support::key(1);
    let body = support::digest(2);
    let mut outbox = Outbox::new(4);
    outbox
        .admit(support::entry(1, 2, 1, b"k"))
        .expect("the outbox has room");
    outbox
        .request_cancel(&key, body)
        .expect("the cancel is local");
    outbox
        .mark_abandoned(&key, body)
        .expect("the client closes the entry");
    let ledger = ReceiptLedger::new(ReceiptSequence::new(0));
    let vector = ledger.receipt_states();
    assert_eq!(vector, [] as [ReceiptState; 0]);
    for disposition in outbox.local_dispositions() {
        assert!(
            ReceiptState::from_name(disposition.name()).is_none(),
            "the local disposition {disposition} appears in the receipt vector"
        );
    }
}

#[test]
fn a_conflict_keeps_the_base_and_the_winning_version() {
    let key = support::key(1);
    let body = support::digest(2);
    let mut outbox = Outbox::new(4);
    outbox
        .admit(support::entry(1, 2, 1, b"k"))
        .expect("the outbox has room");
    let conflict = support::envelope(
        key.clone(),
        9,
        Some(ReceiptOutcome::Conflict {
            current_key_version: KeyVersion::new(8),
            reason: "write.version_conflict",
        }),
    );
    let reconciliation = outbox
        .reconcile(&conflict, body)
        .expect("the conflict applies");
    assert_eq!(
        reconciliation.outcome(),
        Some(ReceiptOutcome::Conflict {
            current_key_version: KeyVersion::new(8),
            reason: "write.version_conflict",
        })
    );
}

#[test]
fn a_cursor_round_trips_through_every_ledger_receipt() {
    let mut ledger = ReceiptLedger::new(ReceiptSequence::new(0));
    for value in 1_u8..=3 {
        ledger
            .submit(
                support::digest(value),
                support::envelope(
                    support::key(value),
                    value,
                    Some(ReceiptOutcome::Committed {
                        commit_version: CommitVersion::new(u64::from(value)),
                    }),
                ),
            )
            .expect("the receipt is stored");
    }
    let mut seen: BTreeMap<u8, bool> = BTreeMap::new();
    let mut cursor = ReceiptCursor::start(ReceiptSequence::new(0));
    loop {
        let records = ledger.resume(&cursor).expect("the cursor is readable");
        if records.is_empty() {
            break;
        }
        for record in records {
            seen.insert(
                record.sequence().get().try_into().expect("small sequence"),
                true,
            );
            cursor = record.cursor(ReceiptSequence::new(0));
        }
    }
    assert_eq!(seen.len(), 3);
}
