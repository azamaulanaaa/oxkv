//! Inclusive range scan tests.

mod support;

use oxkv_contracts::CursorDirection;
use oxkv_contracts::version::CommitVersion;
use oxkv_kv::{KeyRange, KvError};

use support::{commit, delete, for_both_engines, put};

#[test]
fn a_range_scan_is_inclusive_and_returns_each_key_once() {
    for_both_engines(|engine| {
        commit(
            engine,
            1,
            vec![
                put(b"d", b"4"),
                put(b"b", b"2"),
                put(b"a", b"1"),
                put(b"c", b"3"),
            ],
        );
        let range = KeyRange::new(b"b".to_vec(), b"d".to_vec()).expect("the range is valid");

        let ascending = engine
            .scan(CommitVersion::new(1), &range, CursorDirection::Ascending)
            .expect("the scan succeeds");
        let keys: Vec<&[u8]> = ascending.iter().map(|entry| entry.key.as_slice()).collect();
        assert_eq!(keys, vec![&b"b"[..], &b"c"[..], &b"d"[..]]);

        let descending = engine
            .scan(CommitVersion::new(1), &range, CursorDirection::Descending)
            .expect("the scan succeeds");
        let keys: Vec<&[u8]> = descending
            .iter()
            .map(|entry| entry.key.as_slice())
            .collect();
        assert_eq!(keys, vec![&b"d"[..], &b"c"[..], &b"b"[..]]);
    });
}

#[test]
fn an_unbounded_scan_returns_every_live_key_in_byte_order() {
    for_both_engines(|engine| {
        commit(
            engine,
            1,
            vec![
                put(&[0xFF], b"last"),
                put(&[0x00], b"first"),
                put(b"a", b"middle"),
            ],
        );
        let entries = engine
            .scan(
                CommitVersion::new(1),
                &KeyRange::all(),
                CursorDirection::Ascending,
            )
            .expect("the scan succeeds");
        let keys: Vec<&[u8]> = entries.iter().map(|entry| entry.key.as_slice()).collect();
        assert_eq!(keys, vec![&[0x00][..], &b"a"[..], &[0xFF][..]]);
    });
}

#[test]
fn a_delete_removes_the_key_from_later_scans() {
    for_both_engines(|engine| {
        commit(
            engine,
            1,
            vec![put(b"a", b"1"), put(b"b", b"2"), put(b"c", b"3")],
        );
        commit(engine, 2, vec![delete(b"b")]);
        let range = KeyRange::all();

        let before = engine
            .scan(CommitVersion::new(1), &range, CursorDirection::Ascending)
            .expect("the scan succeeds");
        assert_eq!(before.len(), 3);

        let after = engine
            .scan(CommitVersion::new(2), &range, CursorDirection::Ascending)
            .expect("the scan succeeds");
        let keys: Vec<&[u8]> = after.iter().map(|entry| entry.key.as_slice()).collect();
        assert_eq!(keys, vec![&b"a"[..], &b"c"[..]]);
    });
}

#[test]
fn an_inverted_range_fails() {
    let error = KeyRange::new(b"d".to_vec(), b"b".to_vec()).expect_err("the range is inverted");
    assert_eq!(error, KvError::MalformedRange);
    assert_eq!(error.code(), "kv.malformed_range");
}
