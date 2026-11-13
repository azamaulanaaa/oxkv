//! B-tree and append-only LSM differential tests.

mod support;

use oxkv_contracts::CursorDirection;
use oxkv_contracts::version::CommitVersion;
use oxkv_kv::{Batch, Cursor, KeyRange, Limits, OrderedMap};

use support::{batch, btree, delete, lsm, put, shard};

fn operation_log() -> Vec<Batch> {
    vec![
        batch(1, vec![put(b"a", b"1"), put(b"c", b"3")]),
        batch(2, vec![put(b"b", b"2"), delete(b"c")]),
        batch(3, vec![put(b"a", b"10"), put(b"d", b"4")]),
        batch(4, vec![delete(b"a"), put(b"e", b"5")]),
        batch(5, vec![put(b"c", b"30"), put(b"f", b"6")]),
    ]
}

#[test]
fn both_engines_return_the_same_golden_results() {
    let mut btree_engine = btree(Limits::default());
    let mut lsm_engine = lsm(Limits::default());
    for batch in operation_log() {
        btree_engine
            .commit(batch.clone())
            .expect("the btree batch commits");
        lsm_engine.commit(batch).expect("the lsm batch commits");
    }

    for version in 0..=5 {
        let version = CommitVersion::new(version);
        for key in [
            &b"a"[..],
            &b"b"[..],
            &b"c"[..],
            &b"d"[..],
            &b"e"[..],
            &b"f"[..],
        ] {
            assert_eq!(
                btree_engine
                    .get_entry(version, key)
                    .expect("the read succeeds"),
                lsm_engine
                    .get_entry(version, key)
                    .expect("the read succeeds"),
                "the engines disagree at version {version} for key {key:?}"
            );
        }
        assert_eq!(
            btree_engine
                .scan(version, &KeyRange::all(), CursorDirection::Ascending)
                .expect("the scan succeeds"),
            lsm_engine
                .scan(version, &KeyRange::all(), CursorDirection::Ascending)
                .expect("the scan succeeds")
        );
        assert_eq!(
            btree_engine
                .scan(version, &KeyRange::all(), CursorDirection::Descending)
                .expect("the scan succeeds"),
            lsm_engine
                .scan(version, &KeyRange::all(), CursorDirection::Descending)
                .expect("the scan succeeds")
        );
    }

    let latest = CommitVersion::new(5);
    let entries = btree_engine
        .scan(latest, &KeyRange::all(), CursorDirection::Ascending)
        .expect("the scan succeeds");
    let expected: Vec<(&[u8], &[u8])> = entries
        .iter()
        .map(|entry| (entry.key.as_slice(), entry.value.as_slice()))
        .collect();
    assert_eq!(
        expected,
        vec![
            (&b"b"[..], &b"2"[..]),
            (&b"c"[..], &b"30"[..]),
            (&b"d"[..], &b"4"[..]),
            (&b"e"[..], &b"5"[..]),
            (&b"f"[..], &b"6"[..]),
        ]
    );
}

#[test]
fn both_engines_return_the_same_pages_and_cursors() {
    let mut btree_engine = btree(Limits::default());
    let mut lsm_engine = lsm(Limits::default());
    for batch in operation_log() {
        btree_engine
            .commit(batch.clone())
            .expect("the btree batch commits");
        lsm_engine.commit(batch).expect("the lsm batch commits");
    }

    for direction in [CursorDirection::Ascending, CursorDirection::Descending] {
        let mut btree_cursor = Cursor::start(CommitVersion::new(4), direction);
        let mut lsm_cursor = Cursor::start(CommitVersion::new(4), direction);
        loop {
            let btree_page = btree_engine
                .page(&btree_cursor, &KeyRange::all(), 2)
                .expect("the btree page succeeds");
            let lsm_page = lsm_engine
                .page(&lsm_cursor, &KeyRange::all(), 2)
                .expect("the lsm page succeeds");
            assert_eq!(btree_page, lsm_page);
            match btree_page.next_cursor() {
                Some(next) => {
                    btree_cursor = next.clone();
                    lsm_cursor = next.clone();
                }
                None => break,
            }
        }
    }
}

#[test]
fn both_engines_reject_the_same_invalid_batch() {
    let mut btree_engine = btree(Limits::default());
    let mut lsm_engine = lsm(Limits::default());
    let invalid = Batch::new(
        support::other_shard(),
        CommitVersion::new(1),
        vec![put(b"a", b"1")],
    );
    let btree_error = btree_engine
        .commit(invalid.clone())
        .expect_err("the batch is cross-shard");
    let lsm_error = lsm_engine
        .commit(invalid)
        .expect_err("the batch is cross-shard");
    assert_eq!(btree_error, lsm_error);
    assert_eq!(btree_error.code(), "shard.cross_mutation_rejected");
    assert_eq!(btree_engine.last_commit_version(), None);
    assert_eq!(lsm_engine.last_commit_version(), None);
    assert_eq!(
        btree_engine
            .get(CommitVersion::new(1), b"a")
            .expect("the read succeeds"),
        lsm_engine
            .get(CommitVersion::new(1), b"a")
            .expect("the read succeeds")
    );
    assert_eq!(shard(), *btree_engine.shard_id());
}
