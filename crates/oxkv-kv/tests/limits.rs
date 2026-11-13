//! Size limit tests.

mod support;

use oxkv_contracts::CursorDirection;
use oxkv_contracts::version::CommitVersion;
use oxkv_kv::{Cursor, KvError, Limits};

use support::{batch, btree, commit, lsm, put};

#[test]
fn a_key_over_the_key_budget_is_rejected() {
    let limits = Limits {
        key_bytes: 2,
        ..Limits::default()
    };
    for_engine(limits, |engine| {
        let error = engine
            .commit(batch(1, vec![put(b"abc", b"1")]))
            .expect_err("the key is too large");
        assert_eq!(error, KvError::KeyTooLarge { found: 3, limit: 2 });
        assert_eq!(error.code(), "key.too_large");
        assert_eq!(engine.last_commit_version(), None);
    });
}

#[test]
fn a_value_over_the_value_budget_is_rejected() {
    let limits = Limits {
        value_bytes: 2,
        ..Limits::default()
    };
    for_engine(limits, |engine| {
        let error = engine
            .commit(batch(1, vec![put(b"a", b"123")]))
            .expect_err("the value is too large");
        assert_eq!(error, KvError::ValueTooLarge { found: 3, limit: 2 });
        assert_eq!(error.code(), "write.value_too_large");
        assert_eq!(engine.last_commit_version(), None);
    });
}

#[test]
fn a_batch_over_the_batch_budget_is_rejected() {
    let limits = Limits {
        batch_operations: 1,
        ..Limits::default()
    };
    for_engine(limits, |engine| {
        let error = engine
            .commit(batch(1, vec![put(b"a", b"1"), put(b"b", b"2")]))
            .expect_err("the batch is too large");
        assert_eq!(error, KvError::BatchTooLarge { found: 2, limit: 1 });
        assert_eq!(error.code(), "write.batch_too_large");
        assert_eq!(engine.last_commit_version(), None);
    });
}

#[test]
fn a_page_over_the_page_budget_is_rejected() {
    let limits = Limits {
        page_bytes: 10,
        ..Limits::default()
    };
    for_engine(limits, |engine| {
        commit(engine, 1, vec![put(b"a", b"1")]);
        let cursor = Cursor::start(CommitVersion::new(1), CursorDirection::Ascending);
        let error = engine
            .page(&cursor, &oxkv_kv::KeyRange::all(), 10)
            .expect_err("the page is too large");
        assert_eq!(error.code(), "format.page_too_large");
        match error {
            KvError::PageTooLarge { found, limit } => {
                assert_eq!(found, 18);
                assert_eq!(limit, 10);
            }
            other => panic!("the error is not a page limit error: {other:?}"),
        }
    });
}

#[test]
fn a_page_stops_before_the_byte_budget() {
    let limits = Limits {
        page_bytes: 40,
        ..Limits::default()
    };
    for_engine(limits, |engine| {
        commit(
            engine,
            1,
            vec![put(b"a", b"1"), put(b"b", b"2"), put(b"c", b"3")],
        );
        let cursor = Cursor::start(CommitVersion::new(1), CursorDirection::Ascending);
        let page = engine
            .page(&cursor, &oxkv_kv::KeyRange::all(), 10)
            .expect("the page succeeds");
        assert_eq!(page.entries().len(), 2);
        assert!(page.next_cursor().is_some());
    });
}

fn for_engine(limits: Limits, check: impl Fn(&mut dyn oxkv_kv::OrderedMap)) {
    let mut btree_engine = btree(limits);
    check(&mut btree_engine);
    let mut lsm_engine = lsm(limits);
    check(&mut lsm_engine);
}
