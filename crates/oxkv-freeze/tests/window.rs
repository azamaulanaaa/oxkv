//! Reader window and wire window golden vectors.

use oxkv_contracts::error::lookup;
use oxkv_contracts::version::{SnapshotPageVersion, SyncWireVersion};
use oxkv_freeze::{
    ReaderWindow, UNSUPPORTED_FORMAT_CODE, UNSUPPORTED_WIRE_CODE, WindowError, WireWindow,
    WireWindowError,
};
use serde_json::Value;

fn golden_window() -> Value {
    serde_json::from_str(include_str!("vectors/window.json")).expect("the vector file is JSON")
}

fn parse_format(text: &str) -> SnapshotPageVersion {
    text.parse()
        .unwrap_or_else(|error| panic!("the format version {text} is invalid: {error}"))
}

#[test]
fn a_v1_snapshot_opens_on_the_release_build_and_the_previous_minor() {
    let window = ReaderWindow::one_zero();
    assert_eq!(window.current(), SnapshotPageVersion::new(1, 0));
    assert_eq!(window.back(), Some(SnapshotPageVersion::new(0, 12)));
    assert_eq!(window.accepts(SnapshotPageVersion::new(1, 0)), Ok(()));
    assert_eq!(window.accepts(SnapshotPageVersion::new(0, 12)), Ok(()));
    assert_eq!(
        window.accepted_versions(),
        vec![
            SnapshotPageVersion::new(1, 0),
            SnapshotPageVersion::new(0, 12)
        ]
    );
}

#[test]
fn an_unknown_major_fails_with_the_named_error() {
    let error = ReaderWindow::one_zero()
        .accepts(SnapshotPageVersion::new(2, 0))
        .expect_err("major 2 is unknown");
    assert_eq!(error.found(), SnapshotPageVersion::new(2, 0));
    assert_eq!(WindowError::CODE, "format.unsupported_version");
    assert_eq!(UNSUPPORTED_FORMAT_CODE, "format.unsupported_version");
    assert!(lookup(WindowError::CODE).is_some());
}

#[test]
fn golden_reader_window_cases_are_pinned() {
    let file = golden_window();
    assert_eq!(file["contract"], "freeze-window/v1");
    let reader = &file["reader"];
    let window = ReaderWindow::new(
        parse_format(
            reader["current"]
                .as_str()
                .expect("the window has a current"),
        ),
        Some(parse_format(
            reader["back"].as_str().expect("the window has a back"),
        )),
    );
    let cases = reader["cases"].as_array().expect("the reader has cases");
    for case in cases {
        let version = parse_format(case["version"].as_str().expect("the case has a version"));
        let accepted = case["accepted"]
            .as_bool()
            .expect("the case has an acceptance flag");
        assert_eq!(window.is_accepted(version), accepted, "{case:?}");
        let result = window.accepts(version);
        if accepted {
            assert_eq!(result, Ok(()), "{case:?}");
        } else {
            assert_eq!(
                case["error_code"]
                    .as_str()
                    .expect("the case has an error code"),
                WindowError::CODE,
                "{case:?}"
            );
            assert_eq!(result.expect_err("the case is rejected").found(), version);
        }
    }
}

#[test]
fn the_wire_peer_reads_the_current_protocol_and_one_extra_version() {
    let window = WireWindow::one_zero();
    assert_eq!(window.current(), SyncWireVersion::new(1));
    assert_eq!(window.extra_read(), Some(SyncWireVersion::new(2)));
    assert!(window.accepts_write(SyncWireVersion::new(1)));
    assert!(!window.accepts_write(SyncWireVersion::new(2)));
    assert_eq!(window.accepts_read(SyncWireVersion::new(1)), Ok(()));
    assert_eq!(window.accepts_read(SyncWireVersion::new(2)), Ok(()));
    assert_eq!(
        window
            .accepts_read(SyncWireVersion::new(3))
            .expect_err("version 3 is outside the window")
            .found(),
        SyncWireVersion::new(3)
    );
    assert_eq!(WireWindowError::CODE, "sync.unsupported_version");
    assert_eq!(UNSUPPORTED_WIRE_CODE, "sync.unsupported_version");
    assert!(lookup(WireWindowError::CODE).is_some());
}

#[test]
fn golden_wire_window_cases_are_pinned() {
    let file = golden_window();
    let wire = &file["wire"];
    let window = WireWindow::new(
        SyncWireVersion::new(
            u16::try_from(wire["current"].as_u64().expect("the wire has a current"))
                .expect("the current version fits u16"),
        ),
        Some(SyncWireVersion::new(
            u16::try_from(
                wire["extra_read"]
                    .as_u64()
                    .expect("the wire has an extra read"),
            )
            .expect("the extra read version fits u16"),
        )),
    );
    let cases = wire["cases"].as_array().expect("the wire has cases");
    for case in cases {
        let version = SyncWireVersion::new(
            u16::try_from(case["version"].as_u64().expect("the case has a version"))
                .expect("the case version fits u16"),
        );
        let read = case["read"].as_bool().expect("the case has a read flag");
        let write = case["write"].as_bool().expect("the case has a write flag");
        assert_eq!(window.accepts_write(version), write, "{case:?}");
        assert_eq!(window.accepts_read(version).is_ok(), read, "{case:?}");
        if !read {
            assert_eq!(
                case["error_code"]
                    .as_str()
                    .expect("the case has an error code"),
                WireWindowError::CODE,
                "{case:?}"
            );
        }
    }
}
