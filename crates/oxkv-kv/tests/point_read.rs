//! Versioned point read tests.

mod support;

use oxkv_contracts::version::CommitVersion;
use oxkv_kv::KvError;

use support::{batch, commit, delete, for_both_engines, put};

#[test]
fn a_point_read_at_a_version_sees_only_earlier_writes() {
    for_both_engines(|engine| {
        commit(engine, 1, vec![put(b"a", b"1")]);
        commit(engine, 2, vec![put(b"a", b"2"), put(b"b", b"10")]);
        commit(engine, 3, vec![put(b"c", b"30")]);
        commit(engine, 4, vec![delete(b"a")]);

        assert_eq!(
            engine
                .get(CommitVersion::new(1), b"a")
                .expect("the read succeeds"),
            Some(b"1".to_vec())
        );
        assert_eq!(
            engine
                .get(CommitVersion::new(2), b"a")
                .expect("the read succeeds"),
            Some(b"2".to_vec())
        );
        assert_eq!(
            engine
                .get(CommitVersion::new(2), b"b")
                .expect("the read succeeds"),
            Some(b"10".to_vec())
        );
        assert_eq!(
            engine
                .get(CommitVersion::new(2), b"c")
                .expect("the read succeeds"),
            None
        );
        assert_eq!(
            engine
                .get(CommitVersion::new(3), b"c")
                .expect("the read succeeds"),
            Some(b"30".to_vec())
        );
        assert_eq!(
            engine
                .get(CommitVersion::new(4), b"a")
                .expect("the read succeeds"),
            None
        );
        assert_eq!(
            engine
                .get(CommitVersion::new(100), b"a")
                .expect("the read succeeds"),
            None
        );
    });
}

#[test]
fn an_entry_reports_the_commit_version_that_wrote_it() {
    for_both_engines(|engine| {
        commit(engine, 1, vec![put(b"a", b"1")]);
        commit(engine, 2, vec![put(b"a", b"2")]);

        let entry = engine
            .get_required(CommitVersion::new(2), b"a")
            .expect("the key exists");
        assert_eq!(entry.value, b"2");
        assert_eq!(entry.commit_version, CommitVersion::new(2));

        let old = engine
            .get_required(CommitVersion::new(1), b"a")
            .expect("the key exists");
        assert_eq!(old.value, b"1");
        assert_eq!(old.commit_version, CommitVersion::new(1));
    });
}

#[test]
fn a_missing_key_returns_key_not_found() {
    for_both_engines(|engine| {
        commit(engine, 1, vec![put(b"a", b"1")]);
        let error = engine
            .get_required(CommitVersion::new(1), b"missing")
            .expect_err("the key is absent");
        assert_eq!(error.code(), "key.not_found");
        assert_eq!(
            error,
            KvError::KeyNotFound {
                key: b"missing".to_vec()
            }
        );
    });
}

#[test]
fn one_batch_never_appears_in_part() {
    for_both_engines(|engine| {
        commit(engine, 1, vec![put(b"a", b"1")]);
        engine
            .commit(batch(2, vec![put(b"a", b"2"), put(b"b", b"20")]))
            .expect("the batch commits");

        assert_eq!(
            engine
                .get(CommitVersion::new(1), b"b")
                .expect("the read succeeds"),
            None
        );
        assert_eq!(
            engine
                .get(CommitVersion::new(2), b"a")
                .expect("the read succeeds"),
            Some(b"2".to_vec())
        );
        assert_eq!(
            engine
                .get(CommitVersion::new(2), b"b")
                .expect("the read succeeds"),
            Some(b"20".to_vec())
        );
    });
}

#[test]
fn a_point_write_uses_one_commit_version() {
    for_both_engines(|engine| {
        engine
            .put(CommitVersion::new(1), b"a".to_vec(), b"1".to_vec())
            .expect("the point write commits");
        assert_eq!(
            engine
                .get(CommitVersion::new(1), b"a")
                .expect("the read succeeds"),
            Some(b"1".to_vec())
        );
        engine
            .delete(CommitVersion::new(2), b"a".to_vec())
            .expect("the point delete commits");
        assert_eq!(
            engine
                .get(CommitVersion::new(2), b"a")
                .expect("the read succeeds"),
            None
        );
        assert_eq!(engine.last_commit_version(), Some(CommitVersion::new(2)));
    });
}

#[test]
fn a_commit_version_must_increase() {
    for_both_engines(|engine| {
        commit(engine, 2, vec![put(b"a", b"2")]);
        let error = engine
            .commit(batch(1, vec![put(b"a", b"1")]))
            .expect_err("the version regresses");
        assert_eq!(error.code(), "key.unknown_version");
        assert_eq!(
            engine
                .get(CommitVersion::new(2), b"a")
                .expect("the read succeeds"),
            Some(b"2".to_vec())
        );
    });
}
