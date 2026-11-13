//! Golden vector tests for the sync wire protocol.
//!
//! Run the ignored `generate_vectors` test with `OXKV_UPDATE_GOLDEN=1` to
//! rewrite the vector files from the reference implementation.

mod support;

use oxkv_contracts::cursor::{CursorDirection, CursorToken, KeyCursor};
use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::version::{ManifestRevision, SnapshotVersion, SyncWireVersion};
use oxkv_wire::frame::{Credit, Frame};
use oxkv_wire::session::Event;
use support::{
    ReferenceTransport, SUBSCRIPTION, ack, config, cursor, hello, hex, snapshot_ready, subscribe,
    unhex, vector,
};

const CONTRACT: &str = "sync-wire/v1";

#[test]
fn transcript_golden_vector_is_pinned() {
    let file = support::load("transcript.json");
    assert_eq!(file["contract"], CONTRACT);
    let actual: Vec<String> = transcript_bytes().iter().map(|bytes| hex(bytes)).collect();
    let expected: Vec<String> = file["frames"]
        .as_array()
        .expect("frames is an array")
        .iter()
        .map(|value| value.as_str().expect("a frame is text").to_owned())
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 8, "the transcript pins every server frame");
}

#[test]
fn resume_golden_vectors_are_pinned() {
    let file = support::load("resume.json");
    assert_eq!(file["contract"], CONTRACT);
    let cases = file["cases"].as_array().expect("cases is an array");
    assert_eq!(cases.len(), 8, "one case per server frame boundary");
    for case in cases {
        let name = case["name"].as_str().expect("a case name");
        let expected_cursor = case["cursor"].as_str().expect("a cursor");
        let outcome = case["outcome"].as_str().expect("an outcome");
        let expected_frames = string_list(&case["frames"]);
        let mut transport = ReferenceTransport::new(config());
        transport.send(hello());
        let responses = transport.send(subscribe(Some(unhex(expected_cursor))));
        let actual: Vec<String> = transport
            .take_encoded()
            .iter()
            .map(|bytes| hex(bytes))
            .collect();
        assert_eq!(actual, expected_frames, "{name}");
        match outcome {
            "resumed" => {
                let Frame::Subscribed(subscribed) = &responses[0] else {
                    panic!("{name}: expected Subscribed, found {responses:?}");
                };
                assert!(!subscribed.snapshot_required, "{name}");
            }
            "expired" => {
                let Frame::Error(error) = &responses[0] else {
                    panic!("{name}: expected Error, found {responses:?}");
                };
                assert_eq!(error.code.as_str(), "sync.cursor_expired", "{name}");
                assert!(error.snapshot_fallback, "{name}");
            }
            other => panic!("{name}: unknown outcome {other}"),
        }
    }
}

#[test]
fn cursor_error_golden_vectors_are_pinned() {
    let file = support::load("cursor-errors.json");
    assert_eq!(file["contract"], CONTRACT);
    let cases = file["cases"].as_array().expect("cases is an array");
    assert_eq!(cases.len(), 4);
    for case in cases {
        let name = case["name"].as_str().expect("a case name");
        let request = unhex(case["cursor"].as_str().expect("a cursor"));
        let expected_code = case["error"].as_str().expect("an error code");
        let max_resume_gap = case["max_resume_gap"].as_u64().expect("a resume gap");
        let expected_frames = string_list(&case["frames"]);
        let mut config = config();
        config.cursor.max_resume_gap = max_resume_gap;
        let mut transport = ReferenceTransport::new(config);
        transport.send(hello());
        let responses = transport.send(subscribe(Some(request)));
        let actual: Vec<String> = transport
            .take_encoded()
            .iter()
            .map(|bytes| hex(bytes))
            .collect();
        assert_eq!(actual, expected_frames, "{name}");
        let Frame::Error(error) = &responses[0] else {
            panic!("{name}: expected Error, found {responses:?}");
        };
        assert_eq!(error.code.as_str(), expected_code, "{name}");
        assert!(error.snapshot_fallback, "{name}");
    }
}

#[test]
fn negotiation_error_golden_vectors_are_pinned() {
    let file = support::load("negotiation-errors.json");
    assert_eq!(file["contract"], CONTRACT);
    let cases = file["cases"].as_array().expect("cases is an array");
    assert_eq!(cases.len(), 4);
    for case in cases {
        let name = case["name"].as_str().expect("a case name");
        let request = unhex(case["request"].as_str().expect("a request frame"));
        let expected_code = case["error"].as_str().expect("an error code");
        let expected_frames = string_list(&case["frames"]);
        let mut transport = ReferenceTransport::new(config());
        let responses = transport.send(
            Frame::decode(&request, support::MAX_FRAME_BYTES).expect("the request frame decodes"),
        );
        let actual: Vec<String> = transport
            .take_encoded()
            .iter()
            .map(|bytes| hex(bytes))
            .collect();
        assert_eq!(actual, expected_frames, "{name}");
        let Frame::Error(error) = &responses[0] else {
            panic!("{name}: expected Error, found {responses:?}");
        };
        assert_eq!(error.code.as_str(), expected_code, "{name}");
    }
}

#[test]
fn flow_control_golden_vectors_are_pinned() {
    let file = support::load("flow-control.json");
    assert_eq!(file["contract"], CONTRACT);
    assert_eq!(file["pending_after_pause"], 5);
    assert_eq!(
        file["paused_frames"]
            .as_array()
            .expect("paused frames")
            .len(),
        0
    );

    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    transport.send(subscribe(None));
    transport.take_encoded();
    let paused = transport.deliver(Event::SnapshotReady(snapshot_ready()));
    assert_eq!(paused.len(), 0);
    assert_eq!(transport.authority.pending_len(), 5);

    for grant in file["grants"].as_array().expect("grants is an array") {
        let bytes = grant["bytes"].as_u64().expect("byte credit");
        let messages = u32::try_from(grant["messages"].as_u64().expect("message credit"))
            .expect("message credit fits");
        let granted = transport.send(Frame::Credit(Credit { bytes, messages }));
        let actual: Vec<String> = transport
            .take_encoded()
            .iter()
            .map(|bytes| hex(bytes))
            .collect();
        assert_eq!(actual, string_list(&grant["frames"]));
        assert_eq!(granted.len(), actual.len());
    }
}

#[test]
#[ignore = "regenerates the golden vector files"]
fn generate_vectors() {
    assert_eq!(
        std::env::var("OXKV_UPDATE_GOLDEN").as_deref(),
        Ok("1"),
        "set OXKV_UPDATE_GOLDEN=1 to rewrite the vectors"
    );
    write_golden("transcript.json", &transcript_vector());
    write_golden("resume.json", &resume_vector());
    write_golden("cursor-errors.json", &cursor_error_vector());
    write_golden("negotiation-errors.json", &negotiation_error_vector());
    write_golden("flow-control.json", &flow_control_vector());
}

fn transcript_bytes() -> Vec<Vec<u8>> {
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    transport.send(subscribe(None));
    transport.send(support::full_credit());
    transport.deliver(Event::SnapshotReady(snapshot_ready()));
    transport.deliver(Event::DiffsReady(oxkv_wire::session::DiffsReady {
        subscription_id: SUBSCRIPTION,
        entries: support::diffs(),
    }));
    transport.send(ack());
    transport.take_encoded()
}

fn transcript_vector() -> serde_json::Value {
    serde_json::json!({
        "contract": CONTRACT,
        "frames": transcript_bytes().iter().map(|bytes| hex(bytes)).collect::<Vec<_>>(),
    })
}

fn resume_vector() -> serde_json::Value {
    let empty = vector(&[]);
    let cases = [
        ("after-session-ready", empty.clone(), "expired"),
        ("after-subscribed", empty.clone(), "expired"),
        ("after-snapshot-offer", empty.clone(), "expired"),
        ("after-snapshot-page-0", empty.clone(), "expired"),
        ("after-snapshot-page-1", empty.clone(), "expired"),
        ("after-snapshot-tail", vector(&[(1, 5)]), "resumed"),
        ("after-snapshot-complete", vector(&[(1, 5)]), "resumed"),
        ("after-diff-batch", vector(&[(1, 9)]), "resumed"),
    ];
    let cases: Vec<serde_json::Value> = cases
        .into_iter()
        .map(|(name, applied, outcome)| {
            let token = cursor(applied);
            let mut transport = ReferenceTransport::new(config());
            transport.send(hello());
            transport.send(subscribe(Some(token.clone())));
            serde_json::json!({
                "name": name,
                "cursor": hex(&token),
                "outcome": outcome,
                "frames": transport.take_encoded().iter().map(|bytes| hex(bytes)).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({"contract": CONTRACT, "cases": cases})
}

fn cursor_error_vector() -> serde_json::Value {
    let expired = cursor(vector(&[(1, 1)]));
    let mut tampered = cursor(vector(&[(1, 5)]));
    let last = tampered.len().saturating_sub(1);
    tampered[last] ^= 0x01;
    let key_cursor = KeyCursor::new(
        TableName::parse("accounts").expect("valid table"),
        SnapshotVersion::new(3),
        b"account:1".to_vec(),
        CursorDirection::Ascending,
    );
    let cases = [
        ("expired", expired, "sync.cursor_expired", 100),
        ("tampered", tampered, "sync.cursor_invalid", 100),
        (
            "type-mismatch",
            key_cursor.to_token(),
            "sync.cursor_type_mismatch",
            100,
        ),
        (
            "gap-too-large",
            cursor(vector(&[(1, 4)])),
            "sync.gap_too_large",
            2,
        ),
    ];
    let cases: Vec<serde_json::Value> = cases
        .into_iter()
        .map(|(name, token, code, max_resume_gap)| {
            let mut config = config();
            config.cursor.max_resume_gap = max_resume_gap;
            let mut transport = ReferenceTransport::new(config);
            transport.send(hello());
            transport.send(subscribe(Some(token.clone())));
            serde_json::json!({
                "name": name,
                "cursor": hex(&token),
                "error": code,
                "max_resume_gap": max_resume_gap,
                "frames": transport.take_encoded().iter().map(|bytes| hex(bytes)).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({"contract": CONTRACT, "cases": cases})
}

fn negotiation_error_vector() -> serde_json::Value {
    let mut old_manifest = hello();
    let Frame::SessionHello(hello_frame) = &mut old_manifest else {
        panic!("expected hello");
    };
    hello_frame.manifest_revision = ManifestRevision::new(2);

    let mut unknown_capability = hello();
    let Frame::SessionHello(hello_frame) = &mut unknown_capability else {
        panic!("expected hello");
    };
    hello_frame.required_capabilities =
        oxkv_wire::types::Capabilities::from_names(["sync.unknown.v1"]);

    let mut unsupported_version = hello();
    let Frame::SessionHello(hello_frame) = &mut unsupported_version else {
        panic!("expected hello");
    };
    hello_frame.sync_version = SyncWireVersion::new(2);

    let cases = [
        (
            "negotiation-required",
            subscribe(None),
            "sync.negotiation_required",
        ),
        (
            "incompatible-manifest",
            old_manifest,
            "sync.incompatible_manifest",
        ),
        (
            "unknown-capability",
            unknown_capability,
            "sync.unknown_capability",
        ),
        (
            "unsupported-version",
            unsupported_version,
            "sync.unsupported_version",
        ),
    ];
    let cases: Vec<serde_json::Value> = cases
        .into_iter()
        .map(|(name, request, code)| {
            let request_bytes = request.encode();
            let mut transport = ReferenceTransport::new(config());
            transport.send(request);
            serde_json::json!({
                "name": name,
                "request": hex(&request_bytes),
                "error": code,
                "frames": transport.take_encoded().iter().map(|bytes| hex(bytes)).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({"contract": CONTRACT, "cases": cases})
}

fn flow_control_vector() -> serde_json::Value {
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    transport.send(subscribe(None));
    transport.take_encoded();
    let paused = transport.deliver(Event::SnapshotReady(snapshot_ready()));
    let first = transport.send(Frame::Credit(Credit {
        bytes: 1_048_576,
        messages: 1,
    }));
    let first_frames: Vec<String> = transport
        .take_encoded()
        .iter()
        .map(|bytes| hex(bytes))
        .collect();
    let second = transport.send(Frame::Credit(Credit {
        bytes: 1_048_576,
        messages: 1_024,
    }));
    let second_frames: Vec<String> = transport
        .take_encoded()
        .iter()
        .map(|bytes| hex(bytes))
        .collect();
    serde_json::json!({
        "contract": CONTRACT,
        "paused_frames": paused.iter().map(oxkv_wire::Frame::encode).map(|bytes| hex(&bytes)).collect::<Vec<_>>(),
        "pending_after_pause": 5,
        "grants": [
            {
                "bytes": 1_048_576,
                "messages": 1,
                "frames": first_frames,
                "sent": first.len(),
            },
            {
                "bytes": 1_048_576,
                "messages": 1_024,
                "frames": second_frames,
                "sent": second.len(),
            }
        ],
    })
}

fn string_list(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .expect("a string array")
        .iter()
        .map(|entry| entry.as_str().expect("a string").to_owned())
        .collect()
}

fn write_golden(name: &str, value: &serde_json::Value) {
    let path = format!("{}/tests/vectors/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = serde_json::to_string_pretty(value).expect("the vector serializes");
    std::fs::write(&path, format!("{text}\n"))
        .unwrap_or_else(|error| panic!("cannot write the golden file {path}: {error}"));
}
