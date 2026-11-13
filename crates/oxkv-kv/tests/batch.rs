//! Atomic batch tests.

mod support;

use oxkv_contracts::version::CommitVersion;
use oxkv_kv::{Batch, KvError, Limits, Operation, OrderedMap};

use support::{batch, btree, commit, delete, lsm, put, shard};

#[test]
fn a_rejected_batch_changes_no_state() {
    let limits = Limits {
        value_bytes: 2,
        ..Limits::default()
    };
    let mut btree_engine = btree(limits);
    check_rejected_batch(&mut btree_engine);
    let mut lsm_engine = lsm(limits);
    check_rejected_batch(&mut lsm_engine);
}

fn check_rejected_batch(engine: &mut dyn oxkv_kv::OrderedMap) {
    commit(engine, 1, vec![put(b"a", b"1")]);
    let rejected = Batch::new(
        shard(),
        CommitVersion::new(2),
        vec![put(b"b", b"20"), put(b"c", b"too large")],
    );
    let error = engine.commit(rejected).expect_err("the value is too large");
    assert_eq!(error, KvError::ValueTooLarge { found: 9, limit: 2 });
    assert_eq!(engine.last_commit_version(), Some(CommitVersion::new(1)));
    assert_eq!(
        engine
            .get(CommitVersion::new(2), b"a")
            .expect("the read succeeds"),
        Some(b"1".to_vec())
    );
    assert_eq!(
        engine
            .get(CommitVersion::new(2), b"b")
            .expect("the read succeeds"),
        None
    );
}

#[test]
fn a_limited_engine_rejects_an_oversize_value() {
    let limits = Limits {
        value_bytes: 2,
        ..Limits::default()
    };
    let mut engine = btree(limits);
    let error = engine
        .commit(batch(1, vec![put(b"a", b"123")]))
        .expect_err("the value is too large");
    assert_eq!(error, KvError::ValueTooLarge { found: 3, limit: 2 });
    assert_eq!(engine.last_commit_version(), None);
}

#[test]
fn a_cross_shard_batch_is_rejected_and_changes_no_state() {
    for_engine(|engine| {
        commit(engine, 1, vec![put(b"a", b"1")]);
        let rejected = Batch::new(
            support::other_shard(),
            CommitVersion::new(2),
            vec![put(b"b", b"2")],
        );
        let error = engine
            .commit(rejected)
            .expect_err("the batch is cross-shard");
        assert_eq!(error.code(), "shard.cross_mutation_rejected");
        assert_eq!(engine.last_commit_version(), Some(CommitVersion::new(1)));
        assert_eq!(
            engine
                .get(CommitVersion::new(2), b"b")
                .expect("the read succeeds"),
            None
        );
    });
}

#[test]
fn a_batch_applies_its_operations_in_order() {
    for_engine(|engine| {
        engine
            .commit(batch(
                1,
                vec![
                    put(b"a", b"1"),
                    put(b"a", b"2"),
                    delete(b"a"),
                    put(b"a", b"3"),
                ],
            ))
            .expect("the batch commits");
        assert_eq!(
            engine
                .get(CommitVersion::new(1), b"a")
                .expect("the read succeeds"),
            Some(b"3".to_vec())
        );
    });
}

#[test]
fn the_operation_log_is_deterministic() {
    let batch = Batch::new(
        shard(),
        CommitVersion::new(3),
        vec![
            Operation::put(b"a".to_vec(), b"1".to_vec()),
            Operation::delete(b"b".to_vec()),
        ],
    );
    assert_eq!(batch.to_bytes(), batch.to_bytes());
    assert_eq!(
        Batch::from_bytes(&batch.to_bytes()).expect("the log decodes"),
        batch
    );
}

#[test]
fn a_batch_over_the_operation_budget_is_rejected() {
    let limits = Limits {
        batch_operations: 1,
        ..Limits::default()
    };
    let mut engine = btree(limits);
    let error = engine
        .commit(batch(1, vec![put(b"a", b"1"), put(b"b", b"2")]))
        .expect_err("the batch is too large");
    assert_eq!(error, KvError::BatchTooLarge { found: 2, limit: 1 });
    assert_eq!(engine.last_commit_version(), None);
}

fn for_engine(check: impl Fn(&mut dyn oxkv_kv::OrderedMap)) {
    let mut btree_engine = btree(Limits::default());
    check(&mut btree_engine);
    let mut lsm_engine = lsm(Limits::default());
    check(&mut lsm_engine);
}
