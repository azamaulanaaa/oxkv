//! Behaviour tests for the replication seam.

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptState};
use oxkv_contracts::version::{CommitVersion, LeaseTerm};
use oxkv_repl::{
    CrashableDriver, EntryPayload, FenceBook, KillPoint, KvState, LeaseDriver, LeaseDuration,
    LeaseKeeper, Log, LogIndex, NodeId, OwnerFence, RaftDriver, ReadBarrier, ReadBound,
    ReadFreshness, ReplError, ReplicationDriver, TerminalReceipt, Tick, WarmupBudget, WriteAttempt,
    WriteRequest, run_kill_point, submit,
};

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

fn payload(byte: u8) -> EntryPayload {
    EntryPayload::Data {
        idempotency: IdempotencyKey::new(vec![byte]),
        key: b"k".to_vec(),
        value: vec![b'v', byte],
    }
}

fn request(byte: u8) -> WriteRequest {
    WriteRequest::new(
        IdempotencyKey::new(vec![byte]),
        b"k".to_vec(),
        vec![b'v', byte],
    )
}

#[test]
fn every_error_code_is_registered() {
    let errors = [
        ReplError::StaleEpoch {
            presented: 1,
            current: 2,
        },
        ReplError::LeaseExpired { term: 1 },
        ReplError::LeaseHeld { term: 1 },
        ReplError::TierUnavailable { tier: "quorum" },
        ReplError::WarmupBudgetExceeded { budget: 4, used: 5 },
        ReplError::PlacementViolation { reason: "test" },
        ReplError::InvariantViolation { reason: "test" },
        ReplError::ReadBehindCommit {
            requested: 2,
            applied: 1,
        },
        ReplError::ReadStale {
            served: 2,
            bound: 1,
        },
    ];
    for error in errors {
        let spec = error.registry_spec().expect("the code is registered");
        assert_eq!(spec.code, error.code());
        assert_eq!(spec.receipt, error.receipt_state());
    }
}

#[test]
fn a_fence_increases_and_never_repeats() {
    let mut book = FenceBook::new();
    let first = book.grant(NodeId::new(1)).expect("the first grant");
    let second = book.grant(NodeId::new(2)).expect("the second grant");
    let third = book.grant(NodeId::new(3)).expect("the third grant");
    assert_eq!(first.term_value(), 1);
    assert_eq!(second.term_value(), 2);
    assert_eq!(third.term_value(), 3);
    assert_eq!(book.current().get(), 3);
    assert_eq!(book.owner(), Some(NodeId::new(3)));
    book.observe(NodeId::new(4), LeaseTerm::new(5))
        .expect("the higher term wins");
    assert_eq!(book.current().get(), 5);
    assert_eq!(
        book.observe(NodeId::new(1), LeaseTerm::new(4))
            .unwrap_err()
            .code(),
        "replication.stale_epoch"
    );
}

#[test]
fn a_stale_lease_writer_fails_with_a_named_error() {
    let mut driver = lease_driver();
    let old = driver.acquire(Tick::ZERO).expect("the first lease");
    let new = driver
        .transfer(&old, NodeId::new(2), Tick::ZERO)
        .expect("the transfer");
    assert_eq!(new.term_value(), old.term_value() + 1);
    let error = driver
        .append(&old, payload(1), Tick::ZERO)
        .expect_err("the old token is stale");
    assert_eq!(
        error,
        ReplError::StaleEpoch {
            presented: old.term_value(),
            current: new.term_value()
        }
    );
    assert_eq!(error.code(), "replication.stale_epoch");
    assert_eq!(error.receipt_state(), Some(ReceiptState::Rejected));
}

#[test]
fn a_stale_raft_leader_fails_with_a_named_error() {
    let mut driver = raft_driver();
    let old = driver.acquire(Tick::ZERO).expect("the first election");
    driver.partition_leader();
    let new = driver.acquire(Tick::ZERO).expect("the second election");
    assert!(new.term_value() > old.term_value());
    let error = driver
        .append(&old, payload(1), Tick::ZERO)
        .expect_err("the old leader is stale");
    assert_eq!(error.code(), "replication.stale_epoch");
}

#[test]
fn an_expired_lease_fails_with_a_named_error() {
    let mut keeper = LeaseKeeper::new(LeaseDuration::new(5));
    let token = keeper
        .acquire(NodeId::new(1), Tick::ZERO)
        .expect("the lease is granted");
    assert_eq!(
        keeper.renew(&token, Tick::new(5)),
        Err(ReplError::LeaseExpired { term: 1 })
    );
    assert_eq!(
        keeper.validate_at(&token, Tick::new(5)).unwrap_err().code(),
        "replication.lease_expired"
    );
}

#[test]
fn a_held_lease_fails_with_a_named_error() {
    let mut keeper = LeaseKeeper::new(LeaseDuration::new(5));
    let _first = keeper
        .acquire(NodeId::new(1), Tick::ZERO)
        .expect("the first lease");
    assert_eq!(
        keeper.acquire(NodeId::new(2), Tick::ZERO),
        Err(ReplError::LeaseHeld { term: 1 })
    );
    let second = keeper
        .acquire(NodeId::new(2), Tick::new(5))
        .expect("the expired lease is free");
    assert_eq!(second.term_value(), 2);
}

#[test]
fn a_read_past_the_applied_index_fails_with_a_named_error() {
    let mut log = Log::new();
    log.append(
        LeaseTerm::new(1),
        OwnerFence::new(LeaseTerm::new(1)),
        payload(1),
    )
    .expect("the record appends");
    let mut state = KvState::new();
    state
        .apply(&log, LogIndex::new(1))
        .expect("the record applies");
    let error = state
        .read_at(LogIndex::new(2))
        .expect_err("the read is past the applied index");
    assert_eq!(
        error,
        ReplError::ReadBehindCommit {
            requested: 2,
            applied: 1
        }
    );
    assert_eq!(error.code(), "read.behind_commit");
    assert_eq!(error.receipt_state(), None);
    assert_eq!(
        ReadBarrier::new(LogIndex::new(1))
            .admit(LogIndex::new(2))
            .unwrap_err(),
        ReplError::ReadBehindCommit {
            requested: 2,
            applied: 1
        }
    );
}

#[test]
fn a_read_over_its_bound_fails_with_a_named_error() {
    let below = ReadFreshness::new(ReadBound::AtLeast(CommitVersion::new(2)), None)
        .check(CommitVersion::new(1), CommitVersion::new(2))
        .unwrap_err();
    assert_eq!(below.code(), "read.behind_commit");
    let over = ReadFreshness::new(ReadBound::Exactly(CommitVersion::new(1)), None)
        .check(CommitVersion::new(2), CommitVersion::new(2))
        .unwrap_err();
    assert_eq!(over.code(), "read.stale");
    let limit = ReadFreshness::new(ReadBound::Unbounded, Some(1))
        .check(CommitVersion::new(1), CommitVersion::new(3))
        .unwrap_err();
    assert_eq!(limit.code(), "read.stale");
}

#[test]
fn the_lease_kill_points_converge_to_one_committed_state() {
    for point in KillPoint::ALL {
        let mut driver = lease_driver();
        let request = request(1);
        let outcome = run_kill_point(&mut driver, &request, point, Tick::ZERO)
            .unwrap_or_else(|error| panic!("the run {point} failed: {error}"));
        assert_eq!(outcome.receipt.state(), ReceiptState::Committed);
        assert_eq!(
            outcome.receipt.commit_version(),
            Some(CommitVersion::new(1)),
            "the run {point} lost or duplicated the commit"
        );
        assert_eq!(outcome.value(b"k"), Some(b"v\x01".as_slice()));
        assert_eq!(outcome.applied_index, outcome.commit_index);
        let mut retry = WriteAttempt::new(request);
        let receipt = retry
            .run(&mut driver, Tick::ZERO)
            .expect("the retry sees the receipt");
        assert_eq!(receipt.commit_version(), Some(CommitVersion::new(1)));
        let view = driver
            .read_follower(driver.applied_index())
            .expect("the applied read");
        assert_eq!(view.records().len(), 1);
    }
}

#[test]
fn the_raft_kill_points_converge_to_one_committed_state() {
    for point in KillPoint::ALL {
        let mut driver = raft_driver();
        let request = request(1);
        let outcome = run_kill_point(&mut driver, &request, point, Tick::ZERO)
            .unwrap_or_else(|error| panic!("the run {point} failed: {error}"));
        assert_eq!(outcome.receipt.state(), ReceiptState::Committed);
        assert_eq!(
            outcome.receipt.commit_version(),
            Some(CommitVersion::new(1)),
            "the run {point} lost or duplicated the commit"
        );
        assert_eq!(outcome.value(b"k"), Some(b"v\x01".as_slice()));
        assert_eq!(outcome.applied_index, outcome.commit_index);
        let mut retry = WriteAttempt::new(request);
        let receipt = retry
            .run(&mut driver, Tick::ZERO)
            .expect("the retry sees the receipt");
        assert_eq!(receipt.commit_version(), Some(CommitVersion::new(1)));
        let view = driver
            .read_follower(driver.applied_index())
            .expect("the applied read");
        assert_eq!(view.records().len(), 1);
    }
}

#[test]
fn two_raft_leaders_do_not_commit_two_states() {
    let mut driver = raft_driver();
    let old = driver.acquire(Tick::ZERO).expect("the first election");
    driver.partition_leader();
    let new = driver.acquire(Tick::ZERO).expect("the second election");
    assert_eq!(
        driver
            .append(&old, payload(1), Tick::ZERO)
            .unwrap_err()
            .code(),
        "replication.stale_epoch"
    );
    let request = request(2);
    let receipt = submit(&mut driver, &request, Tick::ZERO).expect("the new leader commits");
    assert_eq!(receipt.state(), ReceiptState::Committed);
    driver.heal();
    driver
        .advance(&new, Tick::ZERO)
        .expect("the healed group advances");
    let view = driver
        .read_follower(driver.applied_index())
        .expect("the applied read");
    assert_eq!(view.records().len(), 1);
    assert_eq!(view.value(b"k"), Some(b"v\x02".as_slice()));
}

#[test]
fn two_lease_holders_do_not_commit_two_states() {
    let mut driver = lease_driver();
    let old = driver.acquire(Tick::ZERO).expect("the first lease");
    let new = driver
        .transfer(&old, NodeId::new(2), Tick::ZERO)
        .expect("the transfer");
    assert_eq!(
        driver
            .append(&old, payload(1), Tick::ZERO)
            .unwrap_err()
            .code(),
        "replication.stale_epoch"
    );
    let request = request(2);
    let receipt = submit(&mut driver, &request, Tick::ZERO).expect("the new holder commits");
    assert_eq!(receipt.state(), ReceiptState::Committed);
    let view = driver
        .read_follower(driver.applied_index())
        .expect("the applied read");
    assert_eq!(view.records().len(), 1);
    assert_eq!(view.value(b"k"), Some(b"v\x02".as_slice()));
    assert!(new.term_value() > old.term_value());
}

#[test]
fn both_drivers_return_the_same_logical_result() {
    let ops = [
        (1_u8, b"a".to_vec(), b"1".to_vec()),
        (2, b"b".to_vec(), b"2".to_vec()),
        (3, b"a".to_vec(), b"3".to_vec()),
    ];
    let mut lease = lease_driver();
    let mut raft = raft_driver();
    let lease_versions = run_ops(&mut lease, &ops);
    let raft_versions = run_ops(&mut raft, &ops);
    assert_eq!(lease_versions, raft_versions);
    assert_eq!(lease_versions, vec![1, 2, 3]);
    let lease_view = lease
        .read_follower(lease.applied_index())
        .expect("the lease read");
    let raft_view = raft
        .read_follower(raft.applied_index())
        .expect("the raft read");
    assert_eq!(lease_view.records(), raft_view.records());
}

fn run_ops(driver: &mut impl ReplicationDriver, ops: &[(u8, Vec<u8>, Vec<u8>)]) -> Vec<u64> {
    let mut versions = Vec::new();
    for (idempotency, key, value) in ops {
        let request = WriteRequest::new(
            IdempotencyKey::new(vec![*idempotency]),
            key.clone(),
            value.clone(),
        );
        let receipt = submit(driver, &request, Tick::ZERO).expect("the write commits");
        assert_eq!(receipt.state(), ReceiptState::Committed);
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
fn a_raft_election_is_deterministic() {
    let mut first = raft_driver();
    let mut second = raft_driver();
    let first_token = first.acquire(Tick::ZERO).expect("the first election");
    let second_token = second.acquire(Tick::ZERO).expect("the second election");
    assert_eq!(first_token, second_token);
    first.partition_leader();
    second.partition_leader();
    let first_next = first.acquire(Tick::ZERO).expect("the next election");
    let second_next = second.acquire(Tick::ZERO).expect("the next election");
    assert_eq!(first_next, second_next);
}

#[test]
fn a_receipt_is_committed_or_rejected_and_never_both() {
    let committed =
        TerminalReceipt::committed(LeaseTerm::new(1), LogIndex::new(1), CommitVersion::new(1));
    assert_eq!(committed.state(), ReceiptState::Committed);
    assert!(committed.is_terminal());
    let rejected =
        TerminalReceipt::from_rejection(LeaseTerm::new(1), ReplError::LeaseExpired { term: 1 })
            .expect("the error is a rejection");
    assert_eq!(rejected.state(), ReceiptState::Rejected);
    assert_eq!(rejected.code(), Some("replication.lease_expired"));
    assert!(rejected.is_terminal());
    assert!(
        TerminalReceipt::from_rejection(
            LeaseTerm::new(1),
            ReplError::TierUnavailable { tier: "quorum" }
        )
        .is_err()
    );
}

#[test]
fn an_uncommitted_index_is_never_applied() {
    let mut driver = lease_driver();
    let _token = driver.acquire(Tick::ZERO).expect("the lease");
    let error = driver
        .apply(LogIndex::new(1))
        .expect_err("the index is not committed");
    assert_eq!(error.code(), "internal.invariant_violation");
    assert_eq!(driver.applied_index(), LogIndex::ZERO);
}

#[test]
fn repeated_kills_duplicate_no_commit() {
    let mut lease = lease_driver();
    let mut raft = raft_driver();
    repeated_kills(&mut lease);
    repeated_kills(&mut raft);
}

fn repeated_kills(driver: &mut impl CrashableDriver) {
    let request = request(7);
    let first = submit(driver, &request, Tick::ZERO).expect("the first write commits");
    let version = first.commit_version();
    assert_eq!(version, Some(CommitVersion::new(1)));
    for _ in 0..8 {
        driver.crash_leader().expect("the leader crashes");
        driver.restart().expect("the authority restarts");
        let receipt = submit(driver, &request, Tick::ZERO).expect("the retry commits");
        assert_eq!(receipt.state(), ReceiptState::Committed);
        assert_eq!(receipt.commit_version(), version);
    }
    let view = driver
        .read_follower(driver.applied_index())
        .expect("the applied read");
    assert_eq!(view.records().len(), 1);
    assert_eq!(view.value(b"k"), Some(b"v\x07".as_slice()));
}

#[test]
fn a_warmup_budget_is_named() {
    let budget = WarmupBudget::new(4);
    assert_eq!(budget.limit(), 4);
    budget.check(4).expect("the budget is met");
    let error = budget.check(5).expect_err("the budget is exceeded");
    assert_eq!(error.code(), "replication.warmup_budget_exceeded");
    assert_eq!(error.receipt_state(), None);
}

#[test]
fn the_read_barrier_never_moves_backwards() {
    let mut barrier = ReadBarrier::new(LogIndex::new(2));
    assert_eq!(
        barrier.advance(LogIndex::new(1)).unwrap_err().code(),
        "internal.invariant_violation"
    );
    barrier
        .advance(LogIndex::new(3))
        .expect("the barrier advances");
    assert_eq!(barrier.applied(), LogIndex::new(3));
}

#[test]
fn the_record_header_is_readable() {
    let mut log = Log::new();
    let record = log
        .append(
            LeaseTerm::new(1),
            OwnerFence::new(LeaseTerm::new(1)),
            EntryPayload::Data {
                idempotency: IdempotencyKey::new(vec![1, 2]),
                key: vec![3],
                value: vec![4, 5],
            },
        )
        .expect("the record appends");
    assert_eq!(record.header_line(), "index=1 term=1 len=5");
}

#[test]
fn a_stale_fence_at_the_log_boundary_returns_stale_epoch() {
    let mut log = Log::new();
    log.append(
        LeaseTerm::new(2),
        OwnerFence::new(LeaseTerm::new(2)),
        payload(1),
    )
    .expect("the higher fence appends");
    let error = log
        .append(
            LeaseTerm::new(1),
            OwnerFence::new(LeaseTerm::new(1)),
            payload(2),
        )
        .expect_err("the lower fence is stale");
    assert_eq!(error.code(), "replication.stale_epoch");
    assert_eq!(
        error,
        ReplError::StaleEpoch {
            presented: 1,
            current: 2
        }
    );
    assert_eq!(log.len(), 1);
}
