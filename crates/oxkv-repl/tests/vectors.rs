//! Golden vector runner for the replication seam.

mod support;

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptState};
use oxkv_contracts::version::{CommitVersion, LeaseTerm};
use oxkv_repl::{
    EntryPayload, FenceBook, KillPoint, KvState, LeaseDriver, LeaseDuration, LeaseKeeper, Log,
    LogIndex, NodeId, RaftDriver, ReadBound, ReadFreshness, ReplError, ReplicationDriver, Tick,
    WriteRequest, run_kill_point, submit,
};

use support::{GoldenCase, GoldenFile};

fn assert_error(case: &GoldenCase, error: ReplError) {
    assert_eq!(support::expected_str(case, "code"), error.code());
    let expected_receipt = support::expected_optional_str(case, "receipt_state")
        .map(|name| ReceiptState::from_name(name).expect("the receipt state is known"));
    assert_eq!(expected_receipt, error.receipt_state());
    let spec = error.registry_spec().expect("the code is registered");
    assert_eq!(support::expected_bool(case, "terminal"), spec.terminal);
}

fn lease_driver() -> LeaseDriver {
    LeaseDriver::new(
        NodeId::new(1),
        [NodeId::new(1), NodeId::new(2), NodeId::new(3)],
        LeaseDuration::new(10),
    )
    .expect("the lease driver is valid")
}

fn raft_driver() -> RaftDriver {
    RaftDriver::new([NodeId::new(1), NodeId::new(2), NodeId::new(3)])
        .expect("the raft driver is valid")
}

fn request() -> WriteRequest {
    WriteRequest::new(IdempotencyKey::new(vec![1]), b"k".to_vec(), b"v1".to_vec())
}

fn assert_records(case: &GoldenCase, records: &[oxkv_repl::KeyValue]) {
    let expected = case.expected["records"]
        .as_array()
        .expect("the case has a record list");
    assert_eq!(expected.len(), records.len());
    for (expected, record) in expected.iter().zip(records) {
        assert_eq!(
            expected["key"].as_str().expect("a key"),
            support::hex(&record.key)
        );
        assert_eq!(
            expected["value"].as_str().expect("a value"),
            support::hex(&record.value)
        );
    }
}

fn run_ops(driver: &mut impl ReplicationDriver, ops: &[(u8, &[u8], &[u8])]) -> Vec<u64> {
    let mut versions = Vec::new();
    for (idempotency, key, value) in ops {
        let request = WriteRequest::new(
            IdempotencyKey::new(vec![*idempotency]),
            key.to_vec(),
            value.to_vec(),
        );
        let receipt = submit(driver, &request, Tick::ZERO).expect("the write commits");
        versions.push(
            receipt
                .commit_version()
                .expect("the committed receipt has a version")
                .get(),
        );
    }
    versions
}

#[test]
#[allow(clippy::too_many_lines)] // One case per golden row keeps the vector table readable.
fn replication_golden_vectors_are_pinned() {
    let file: GoldenFile = support::load(include_str!("vectors/replication.json"));
    assert_eq!(file.contract, "replication/v1");
    for case in &file.cases {
        match case.name.as_str() {
            "fence_sequence" => {
                let mut book = FenceBook::new();
                let mut terms = Vec::new();
                for node in [1_u64, 2, 3] {
                    let token = book.grant(NodeId::new(node)).expect("the grant succeeds");
                    terms.push(token.term_value());
                }
                assert_eq!(support::expected_u64_list(case, "terms"), terms);
            }
            "stale_epoch" => {
                let mut keeper = LeaseKeeper::new(LeaseDuration::new(5));
                let old = keeper
                    .acquire(NodeId::new(1), Tick::ZERO)
                    .expect("the first lease");
                let _new = keeper
                    .acquire(NodeId::new(2), Tick::new(5))
                    .expect("the next lease");
                let error = keeper.validate(&old).expect_err("the old token is stale");
                assert_error(case, error);
            }
            "lease_expired" => {
                let mut keeper = LeaseKeeper::new(LeaseDuration::new(5));
                let token = keeper
                    .acquire(NodeId::new(1), Tick::ZERO)
                    .expect("the lease");
                let error = keeper
                    .validate_at(&token, Tick::new(5))
                    .expect_err("the lease expired");
                assert_error(case, error);
            }
            "lease_held" => {
                let mut keeper = LeaseKeeper::new(LeaseDuration::new(5));
                let _first = keeper
                    .acquire(NodeId::new(1), Tick::ZERO)
                    .expect("the first lease");
                let error = keeper
                    .acquire(NodeId::new(2), Tick::ZERO)
                    .expect_err("the lease is held");
                assert_error(case, error);
            }
            "read_behind_commit" => {
                let mut log = Log::new();
                log.append(
                    LeaseTerm::new(1),
                    EntryPayload::Data {
                        idempotency: IdempotencyKey::new(vec![1]),
                        key: b"k".to_vec(),
                        value: b"v".to_vec(),
                    },
                )
                .expect("the record appends");
                let mut state = KvState::new();
                state
                    .apply(&log, LogIndex::new(1))
                    .expect("the record applies");
                let error = state
                    .read_at(LogIndex::new(2))
                    .expect_err("the read is past the applied index");
                assert_error(case, error);
            }
            "read_stale" => {
                let error = ReadFreshness::new(ReadBound::Exactly(CommitVersion::new(1)), None)
                    .check(CommitVersion::new(2), CommitVersion::new(2))
                    .expect_err("the read is over the bound");
                assert_error(case, error);
            }
            "record_header" => {
                let mut log = Log::new();
                let record = log
                    .append(
                        LeaseTerm::new(1),
                        EntryPayload::Data {
                            idempotency: IdempotencyKey::new(vec![1, 2]),
                            key: vec![3],
                            value: vec![4, 5],
                        },
                    )
                    .expect("the record appends");
                assert_eq!(support::expected_str(case, "text"), record.header_line());
            }
            "kill_before_append"
            | "kill_after_append_before_commit"
            | "kill_after_commit_before_apply"
            | "kill_after_apply_before_reply" => {
                let point = match case.name.as_str() {
                    "kill_before_append" => KillPoint::BeforeAppend,
                    "kill_after_append_before_commit" => KillPoint::AfterAppendBeforeCommit,
                    "kill_after_commit_before_apply" => KillPoint::AfterCommitBeforeApply,
                    _ => KillPoint::AfterApplyBeforeReply,
                };
                let mut lease = lease_driver();
                let lease_outcome = run_kill_point(&mut lease, &request(), point, Tick::ZERO)
                    .expect("the lease run finishes");
                let mut raft = raft_driver();
                let raft_outcome = run_kill_point(&mut raft, &request(), point, Tick::ZERO)
                    .expect("the raft run finishes");
                for outcome in [&lease_outcome, &raft_outcome] {
                    assert_eq!(
                        outcome.receipt.state(),
                        ReceiptState::Committed,
                        "the run {point} is not committed"
                    );
                    assert_eq!(
                        outcome.receipt.commit_version(),
                        Some(CommitVersion::new(support::expected_u64(
                            case,
                            "commit_version"
                        )))
                    );
                    assert_records(case, &outcome.state);
                }
            }
            "driver_parity" => {
                let ops = [
                    (1_u8, b"a".as_slice(), b"1".as_slice()),
                    (2, b"b".as_slice(), b"2".as_slice()),
                    (3, b"a".as_slice(), b"3".as_slice()),
                ];
                let mut lease = lease_driver();
                let mut raft = raft_driver();
                let lease_versions = run_ops(&mut lease, &ops);
                let raft_versions = run_ops(&mut raft, &ops);
                assert_eq!(
                    support::expected_u64_list(case, "commit_versions"),
                    lease_versions
                );
                assert_eq!(lease_versions, raft_versions);
                let lease_view = lease
                    .read_follower(lease.applied_index())
                    .expect("the lease read");
                let raft_view = raft
                    .read_follower(raft.applied_index())
                    .expect("the raft read");
                assert_eq!(lease_view.records(), raft_view.records());
                assert_records(case, lease_view.records());
            }
            other => panic!("the golden case {other} is unknown"),
        }
    }
}
