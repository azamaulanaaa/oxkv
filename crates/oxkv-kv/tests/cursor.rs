//! Bidirectional cursor tests.

mod support;

use oxkv_contracts::CursorDirection;
use oxkv_contracts::version::CommitVersion;
use oxkv_kv::{Cursor, KeyRange};

use support::{commit, delete, for_both_engines, put};

fn keys(page: &oxkv_kv::Page) -> Vec<&[u8]> {
    page.entries()
        .iter()
        .map(|entry| entry.key.as_slice())
        .collect()
}

#[test]
fn pages_walk_forward_and_backward_without_duplicates() {
    for_both_engines(|engine| {
        commit(
            engine,
            1,
            vec![
                put(b"a", b"1"),
                put(b"b", b"2"),
                put(b"c", b"3"),
                put(b"d", b"4"),
            ],
        );
        let range = KeyRange::all();

        let mut cursor = Cursor::start(CommitVersion::new(1), CursorDirection::Ascending);
        let mut forward = Vec::new();
        loop {
            let page = engine.page(&cursor, &range, 2).expect("the page succeeds");
            forward.extend(page.entries().iter().map(|entry| entry.key.clone()));
            match page.next_cursor() {
                Some(next) => cursor = next.clone(),
                None => break,
            }
        }
        assert_eq!(
            forward,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
        );

        let mut cursor = Cursor::start(CommitVersion::new(1), CursorDirection::Descending);
        let mut backward = Vec::new();
        loop {
            let page = engine.page(&cursor, &range, 2).expect("the page succeeds");
            backward.extend(page.entries().iter().map(|entry| entry.key.clone()));
            match page.next_cursor() {
                Some(next) => cursor = next.clone(),
                None => break,
            }
        }
        assert_eq!(
            backward,
            vec![b"d".to_vec(), b"c".to_vec(), b"b".to_vec(), b"a".to_vec()]
        );
    });
}

#[test]
fn a_cursor_resumes_after_the_last_key_in_each_direction() {
    for_both_engines(|engine| {
        commit(
            engine,
            1,
            vec![put(b"a", b"1"), put(b"b", b"2"), put(b"c", b"3")],
        );
        let range = KeyRange::all();

        let forward = Cursor::after(
            CommitVersion::new(1),
            b"a".to_vec(),
            CursorDirection::Ascending,
        );
        let page = engine
            .page(&forward, &range, 10)
            .expect("the page succeeds");
        assert_eq!(keys(&page), vec![&b"b"[..], &b"c"[..]]);

        let backward = Cursor::after(
            CommitVersion::new(1),
            b"c".to_vec(),
            CursorDirection::Descending,
        );
        let page = engine
            .page(&backward, &range, 10)
            .expect("the page succeeds");
        assert_eq!(keys(&page), vec![&b"b"[..], &b"a"[..]]);

        let past_end = Cursor::after(
            CommitVersion::new(1),
            b"z".to_vec(),
            CursorDirection::Ascending,
        );
        let page = engine
            .page(&past_end, &range, 10)
            .expect("the page succeeds");
        assert!(page.is_empty());
        assert!(page.next_cursor().is_none());
    });
}

#[test]
fn a_pinned_cursor_is_stable_while_writes_continue() {
    for_both_engines(|engine| {
        commit(
            engine,
            1,
            vec![
                put(b"a", b"1"),
                put(b"b", b"2"),
                put(b"c", b"3"),
                put(b"d", b"4"),
            ],
        );
        let range = KeyRange::all();
        let cursor = Cursor::start(CommitVersion::new(1), CursorDirection::Ascending);
        let first = engine.page(&cursor, &range, 2).expect("the page succeeds");
        assert_eq!(keys(&first), vec![&b"a"[..], &b"b"[..]]);

        commit(engine, 2, vec![put(b"bb", b"new"), delete(b"c")]);
        commit(engine, 3, vec![put(b"d", b"later")]);

        let mut seen = first.entries().to_vec();
        let mut next = first
            .next_cursor()
            .expect("the first page has a continuation")
            .clone();
        loop {
            let page = engine.page(&next, &range, 2).expect("the page succeeds");
            seen.extend(page.entries().to_vec());
            match page.next_cursor() {
                Some(cursor) => next = cursor.clone(),
                None => break,
            }
        }

        let expected = engine
            .scan(CommitVersion::new(1), &range, CursorDirection::Ascending)
            .expect("the scan succeeds");
        assert_eq!(seen, expected);
        let names: Vec<&[u8]> = seen.iter().map(|entry| entry.key.as_slice()).collect();
        assert_eq!(names, vec![&b"a"[..], &b"b"[..], &b"c"[..], &b"d"[..]]);
    });
}

#[test]
fn a_reverse_cursor_is_stable_while_writes_continue() {
    for_both_engines(|engine| {
        commit(
            engine,
            1,
            vec![
                put(b"a", b"1"),
                put(b"b", b"2"),
                put(b"c", b"3"),
                put(b"d", b"4"),
            ],
        );
        let range = KeyRange::all();
        let cursor = Cursor::start(CommitVersion::new(1), CursorDirection::Descending);
        let first = engine.page(&cursor, &range, 2).expect("the page succeeds");
        assert_eq!(keys(&first), vec![&b"d"[..], &b"c"[..]]);

        commit(engine, 2, vec![put(b"cc", b"new"), put(b"a", b"changed")]);

        let mut seen = first.entries().to_vec();
        let mut next = first
            .next_cursor()
            .expect("the first page has a continuation")
            .clone();
        loop {
            let page = engine.page(&next, &range, 2).expect("the page succeeds");
            seen.extend(page.entries().to_vec());
            match page.next_cursor() {
                Some(cursor) => next = cursor.clone(),
                None => break,
            }
        }

        let expected = engine
            .scan(CommitVersion::new(1), &range, CursorDirection::Descending)
            .expect("the scan succeeds");
        assert_eq!(seen, expected);
        let names: Vec<&[u8]> = seen.iter().map(|entry| entry.key.as_slice()).collect();
        assert_eq!(names, vec![&b"d"[..], &b"c"[..], &b"b"[..], &b"a"[..]]);
    });
}

#[test]
fn a_cursor_token_round_trips() {
    let cursor = Cursor::after(
        CommitVersion::new(9),
        b"resume".to_vec(),
        CursorDirection::Descending,
    );
    assert_eq!(Cursor::from_token(&cursor.to_token()), Ok(cursor));
}

#[test]
fn a_zero_page_size_fails() {
    for_both_engines(|engine| {
        commit(engine, 1, vec![put(b"a", b"1")]);
        let cursor = Cursor::start(CommitVersion::new(1), CursorDirection::Ascending);
        let error = engine
            .page(&cursor, &KeyRange::all(), 0)
            .expect_err("the page size is zero");
        assert_eq!(error.code(), "kv.malformed_page");
    });
}
