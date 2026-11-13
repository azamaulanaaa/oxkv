//! Behaviour tests for the sans-IO sync wire protocol.

mod support;

use oxkv_contracts::cursor::{CursorDirection, CursorToken, KeyCursor};
use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::version::{
    DescriptorDigest, ManifestDigest, ManifestRevision, MapVersion, PolicyVersion, SnapshotVersion,
    SyncWireVersion,
};
use oxkv_wire::diff::{ApplyOutcome, DiffApplier, DiffOperation};
use oxkv_wire::error::ErrorCode;
use oxkv_wire::frame::{
    CloseFrame, Credit, Frame, FrameError, MessageType, ProtocolErrorFrame, SnapshotPage,
};
use oxkv_wire::session::{Command, Event, SessionPhase};
use oxkv_wire::types::Capabilities;
use support::{
    CURSOR_KEY, POLICY_PAYLOAD, ReferenceTransport, SCOPE_GENERATION, SCOPE_IDENTITY, SUBSCRIPTION,
    ack, config, cursor, diff_batch, diffs, full_credit, hello, snapshot_page, snapshot_ready,
    subscribe, vector,
};

fn error_code(frame: &Frame) -> ErrorCode {
    match frame {
        Frame::Error(error) => error.code,
        other => panic!("expected an Error frame, found {other:?}"),
    }
}

#[test]
fn full_transcript_keeps_order_and_policy_bytes() {
    let mut transport = ReferenceTransport::new(config());
    let ready = transport.send(hello());
    assert_eq!(ready.len(), 1);
    assert!(matches!(ready[0], Frame::SessionReady(_)));

    let subscribed = transport.send(subscribe(None));
    assert_eq!(subscribed.len(), 1);
    let Frame::Subscribed(subscribed) = &subscribed[0] else {
        panic!("expected Subscribed");
    };
    assert!(subscribed.snapshot_required);
    assert_eq!(subscribed.effective_policy_version, POLICY_PAYLOAD_VERSION);

    transport.send(full_credit());
    let snapshot = transport.deliver(Event::SnapshotReady(snapshot_ready()));
    assert_eq!(snapshot.len(), 5);
    assert!(matches!(snapshot[0], Frame::SnapshotOffer(_)));
    assert!(matches!(snapshot[1], Frame::SnapshotPage(_)));
    assert!(matches!(snapshot[2], Frame::SnapshotPage(_)));
    assert!(matches!(snapshot[3], Frame::SnapshotTail(_)));
    assert!(matches!(snapshot[4], Frame::SnapshotComplete(_)));
    for page in &snapshot[1..3] {
        let Frame::SnapshotPage(page) = page else {
            panic!("expected a page");
        };
        assert!(page.verify_integrity());
    }
    let Frame::SnapshotTail(tail) = &snapshot[3] else {
        panic!("expected a tail");
    };
    let mut resumed = ReferenceTransport::new(config());
    resumed.send(hello());
    let responses = resumed.send(subscribe(Some(tail.cursor.clone())));
    let Frame::Subscribed(subscribed) = &responses[0] else {
        panic!("expected Subscribed from the tail cursor");
    };
    assert!(!subscribed.snapshot_required);
    assert_eq!(subscribed.starting_version, tail.start_version);

    transport.send(full_credit());
    let diffs = support::diffs();
    let batch = transport.deliver(Event::DiffsReady(oxkv_wire::session::DiffsReady {
        subscription_id: SUBSCRIPTION,
        entries: diffs,
    }));
    assert_eq!(batch.len(), 1);
    assert!(matches!(batch[0], Frame::DiffBatch(_)));

    let acked = transport.send(ack());
    assert_eq!(acked.len(), 0);

    let Frame::Subscribe(subscribe_frame) = subscribe(None) else {
        panic!("expected a subscribe frame");
    };
    assert_eq!(subscribe_frame.policy_payload, POLICY_PAYLOAD);
}

const POLICY_PAYLOAD_VERSION: PolicyVersion = PolicyVersion::from_bytes([4; 32]);

#[test]
fn resume_after_snapshot_tail_needs_no_snapshot() {
    let cases = [
        ("after-tail", vector(&[(1, 5)])),
        ("after-complete", vector(&[(1, 5)])),
        ("after-diff", vector(&[(1, 9)])),
        ("after-ack", vector(&[(1, 9)])),
    ];
    for (name, applied) in cases {
        let mut transport = ReferenceTransport::new(config());
        transport.send(hello());
        let responses = transport.send(subscribe(Some(cursor(applied.clone()))));
        assert_eq!(responses.len(), 1, "{name}");
        let Frame::Subscribed(subscribed) = &responses[0] else {
            panic!("{name}: expected Subscribed, found {responses:?}");
        };
        assert!(!subscribed.snapshot_required, "{name}");
        assert_eq!(subscribed.starting_version, applied, "{name}");
        assert!(
            !responses
                .iter()
                .any(|frame| matches!(frame, Frame::SnapshotOffer(_))),
            "{name}: a snapshot was offered"
        );
    }
}

#[test]
fn resume_frame_reissues_the_cursor() {
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    transport.send(subscribe(None));
    let responses = transport.send(Frame::Resume(oxkv_wire::frame::Resume {
        subscription_id: SUBSCRIPTION,
        cursor: cursor(vector(&[(1, 5)])),
    }));
    assert_eq!(responses.len(), 1);
    let Frame::Resumed(resumed) = &responses[0] else {
        panic!("expected Resumed, found {responses:?}");
    };
    assert_eq!(resumed.starting_version, vector(&[(1, 5)]));
    assert_ne!(resumed.cursor.len(), 0);
}

#[test]
fn expired_cursor_returns_named_error_and_snapshot_offer() {
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    let responses = transport.send(subscribe(Some(cursor(vector(&[(1, 1)])))));
    assert_eq!(responses.len(), 1);
    let Frame::Error(error) = &responses[0] else {
        panic!("expected Error");
    };
    assert_eq!(error.code, ErrorCode::CursorExpired);
    assert!(error.snapshot_fallback);
    assert!(error.requires_resync);
}

#[test]
fn tampered_cursor_returns_cursor_invalid() {
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    let mut token = cursor(vector(&[(1, 5)]));
    let position = token.len().saturating_sub(1);
    token[position] ^= 0x01;
    let responses = transport.send(subscribe(Some(token)));
    assert_eq!(error_code(&responses[0]), ErrorCode::CursorInvalid);
}

#[test]
fn a_forged_auth_tag_is_invalid() {
    let token = cursor(vector(&[(1, 5)]));
    let mut decoded = oxkv_contracts::cursor::SyncCursor::from_token(&token).expect("valid token");
    decoded.auth_tag = oxkv_contracts::version::Hash32::from_bytes([0; 32]);
    let forged = decoded.to_token();
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    let responses = transport.send(subscribe(Some(forged)));
    assert_eq!(error_code(&responses[0]), ErrorCode::CursorInvalid);
}

#[test]
fn cursor_from_another_type_returns_type_mismatch() {
    let key_cursor = KeyCursor::new(
        TableName::parse("accounts").expect("valid table"),
        SnapshotVersion::new(3),
        b"account:1".to_vec(),
        CursorDirection::Ascending,
    );
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    let responses = transport.send(subscribe(Some(key_cursor.to_token())));
    assert_eq!(error_code(&responses[0]), ErrorCode::CursorTypeMismatch);
}

#[test]
fn data_call_before_hello_returns_negotiation_required() {
    let mut transport = ReferenceTransport::new(config());
    let responses = transport.send(subscribe(None));
    assert_eq!(responses.len(), 1);
    assert_eq!(error_code(&responses[0]), ErrorCode::NegotiationRequired);
    assert_eq!(transport.authority.phase(), SessionPhase::AwaitHello);
}

#[test]
fn an_old_manifest_returns_incompatible_manifest() {
    let mut transport = ReferenceTransport::new(config());
    let Frame::SessionHello(mut frame) = hello() else {
        panic!("expected hello");
    };
    frame.manifest_revision = ManifestRevision::new(2);
    let responses = transport.send(Frame::SessionHello(frame));
    assert_eq!(responses.len(), 2);
    assert_eq!(error_code(&responses[0]), ErrorCode::IncompatibleManifest);
    assert!(matches!(responses[1], Frame::Close(_)));
    assert_eq!(transport.authority.phase(), SessionPhase::Closed);
}

#[test]
fn unknown_required_capability_returns_unknown_capability() {
    let mut transport = ReferenceTransport::new(config());
    let Frame::SessionHello(mut frame) = hello() else {
        panic!("expected hello");
    };
    frame.required_capabilities = Capabilities::from_names(["sync.unknown.v1"]);
    let responses = transport.send(Frame::SessionHello(frame));
    assert_eq!(responses.len(), 2);
    assert_eq!(error_code(&responses[0]), ErrorCode::UnknownCapability);
    assert!(matches!(responses[1], Frame::Close(_)));
}

#[test]
fn a_second_hello_is_rejected() {
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    let responses = transport.send(hello());
    assert_eq!(responses.len(), 2);
    assert_eq!(error_code(&responses[0]), ErrorCode::NegotiationRequired);
    assert!(matches!(responses[1], Frame::Close(_)));
}

#[test]
fn an_unsupported_sync_version_names_the_supported_range() {
    let mut transport = ReferenceTransport::new(config());
    let Frame::SessionHello(mut frame) = hello() else {
        panic!("expected hello");
    };
    frame.sync_version = SyncWireVersion::new(2);
    let responses = transport.send(Frame::SessionHello(frame));
    let Frame::Error(error) = &responses[0] else {
        panic!("expected Error");
    };
    assert_eq!(error.code, ErrorCode::UnsupportedVersion);
    assert_eq!(error.supported_min, Some(1));
    assert_eq!(error.supported_max, Some(1));
}

#[test]
fn flow_control_pauses_and_resumes() {
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    transport.send(subscribe(None));
    let paused = transport.deliver(Event::SnapshotReady(snapshot_ready()));
    assert_eq!(paused.len(), 0);
    assert!(transport.authority.is_paused());
    assert_eq!(transport.authority.pending_len(), 5);
    let resumed = transport.send(full_credit());
    assert_eq!(resumed.len(), 5);
    assert!(!transport.authority.is_paused());
}

#[test]
fn flow_control_never_exceeds_credit() {
    let mut transport = ReferenceTransport::new(config());
    transport.send(hello());
    transport.send(subscribe(None));
    let pages = snapshot_ready();
    let first = Frame::SnapshotOffer(pages.offer.clone()).encode();
    transport.send(Frame::Credit(Credit {
        bytes: u64::try_from(first.len()).expect("small frame"),
        messages: 1,
    }));
    let sent = transport.deliver(Event::SnapshotReady(pages));
    assert_eq!(sent.len(), 1);
    assert!(matches!(sent[0], Frame::SnapshotOffer(_)));
    assert!(transport.authority.is_paused());
}

#[test]
fn a_diff_stream_gap_is_detected_before_apply() {
    let mut applier = DiffApplier::new(SUBSCRIPTION, SCOPE_GENERATION);
    let entries = diffs();
    assert_eq!(applier.apply(&entries[0]), Ok(ApplyOutcome::Applied));
    assert_eq!(applier.apply(&entries[0]), Ok(ApplyOutcome::Duplicate));
    assert_eq!(
        applier.apply(&entries[1]),
        Ok(ApplyOutcome::Applied),
        "the next entry applies once"
    );
}

#[test]
fn applying_a_diff_stream_twice_equals_one_apply() {
    let mut once = DiffApplier::new(SUBSCRIPTION, SCOPE_GENERATION);
    once.apply_batch(&diff_batch()).expect("the batch applies");
    let mut twice = DiffApplier::new(SUBSCRIPTION, SCOPE_GENERATION);
    twice.apply_batch(&diff_batch()).expect("the batch applies");
    twice
        .apply_batch(&diff_batch())
        .expect("the replay is idempotent");
    assert_eq!(once, twice);
    assert_eq!(
        twice.get(
            &oxkv_contracts::version::ShardId::new(vec![1]),
            b"account:1"
        ),
        Some(b"one".as_slice())
    );
}

#[test]
fn a_gap_is_rejected_before_a_later_diff_applies() {
    let mut applier = DiffApplier::new(SUBSCRIPTION, SCOPE_GENERATION);
    let entries = diffs();
    assert_eq!(
        applier.apply(&entries[1]),
        Err(oxkv_wire::diff::DiffApplyError::Gap {
            expected: 0,
            found: 1
        })
    );
    assert!(applier.is_empty());
}

#[test]
fn leave_scope_prunes_the_key() {
    let mut applier = DiffApplier::new(SUBSCRIPTION, SCOPE_GENERATION);
    let entries = diffs();
    applier.apply(&entries[0]).expect("first entry applies");
    let mut leave = entries[1].clone();
    leave.sequence = oxkv_wire::types::MessageSequence::new(1);
    leave.key = b"account:1".to_vec();
    leave.operation = DiffOperation::LeaveScope;
    applier.apply(&leave).expect("leave scope applies");
    assert!(
        applier
            .get(
                &oxkv_contracts::version::ShardId::new(vec![1]),
                b"account:1"
            )
            .is_none()
    );
}

#[test]
fn every_message_type_round_trips() {
    let frames = vec![
        hello(),
        Frame::SessionReady(ready_frame()),
        subscribe(None),
        Frame::Subscribed(subscribed_frame()),
        Frame::SnapshotOffer(snapshot_offer()),
        Frame::SnapshotPage(snapshot_page(0)),
        Frame::SnapshotTail(snapshot_tail()),
        Frame::SnapshotComplete(snapshot_complete()),
        Frame::Diff(diffs()[0].clone()),
        Frame::DiffBatch(diff_batch()),
        ack(),
        full_credit(),
        Frame::Keepalive(oxkv_wire::frame::Keepalive { nonce: 9 }),
        Frame::Receipt(receipt_frame()),
        Frame::ReceiptResume(oxkv_wire::frame::ReceiptResume {
            subscription_id: SUBSCRIPTION,
            cursor: vec![1, 2],
        }),
        Frame::Resume(oxkv_wire::frame::Resume {
            subscription_id: SUBSCRIPTION,
            cursor: cursor(vector(&[(1, 5)])),
        }),
        Frame::Resumed(oxkv_wire::frame::Resumed {
            subscription_id: SUBSCRIPTION,
            starting_version: vector(&[(1, 5)]),
            cursor: cursor(vector(&[(1, 5)])),
            oldest_readable: vector(&[(1, 4)]),
        }),
        Frame::Error(ProtocolErrorFrame::new(ErrorCode::CursorExpired)),
        Frame::Close(CloseFrame::new(ErrorCode::RateLimited, "slow")),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for frame in &frames {
        let bytes = frame.encode();
        let decoded = Frame::decode(&bytes, support::MAX_FRAME_BYTES).expect("frame decodes");
        assert_eq!(&decoded, frame);
        assert_eq!(decoded.encode(), bytes);
        assert!(seen.insert(decoded.message_type()));
    }
    for message_type in MessageType::ALL {
        assert!(seen.contains(message_type), "{message_type} has no vector");
    }
}

#[test]
fn a_changed_frame_byte_fails_integrity() {
    let mut bytes = hello().encode();
    let position = bytes.len() / 2;
    bytes[position] ^= 0x01;
    assert_eq!(
        Frame::decode(&bytes, support::MAX_FRAME_BYTES),
        Err(FrameError::IntegrityMismatch)
    );
}

#[test]
fn an_unknown_frame_version_is_named() {
    let mut bytes = hello().encode();
    bytes[0] = 2;
    assert!(matches!(
        Frame::decode(&bytes, support::MAX_FRAME_BYTES),
        Err(FrameError::UnsupportedVersion {
            found: 2,
            supported: 1
        })
    ));
}

#[test]
fn a_snapshot_page_checks_its_integrity() {
    let mut page = SnapshotPage::new(
        SUBSCRIPTION,
        oxkv_contracts::version::PageSequence::new(0),
        oxkv_contracts::version::PageId::from_bytes([1; 32]),
        b"page".to_vec(),
    );
    assert!(page.verify_integrity());
    page.payload.push(0);
    assert!(!page.verify_integrity());
}

#[test]
fn cursor_signer_rejects_a_wrong_scope() {
    let policy = config().cursor;
    let mut token = cursor(vector(&[(1, 5)]));
    let last = token.len().saturating_sub(1);
    token[last] ^= 0x01;
    let context = oxkv_wire::cursor::CursorContext::new(SCOPE_IDENTITY, SCOPE_GENERATION);
    assert!(policy.signer.verify(&context, &token).is_err());
    assert_eq!(
        policy
            .signer
            .issue(&context, vector(&[(1, 5)]), vector(&[(1, 4)]))
            .auth_tag
            .as_bytes()
            .len(),
        32
    );
    assert_eq!(CURSOR_KEY.len(), 32);
}

#[test]
fn every_named_error_is_in_the_frozen_registry() {
    for code in ErrorCode::ALL {
        assert!(
            oxkv_contracts::error::lookup(code.as_str()).is_some(),
            "{code} is not registered"
        );
    }
    let required = [
        ErrorCode::UnsupportedVersion,
        ErrorCode::UnknownCapability,
        ErrorCode::ScopeRejected,
        ErrorCode::CursorExpired,
        ErrorCode::CursorInvalid,
        ErrorCode::CursorTypeMismatch,
        ErrorCode::GapTooLarge,
        ErrorCode::SnapshotUnavailable,
        ErrorCode::RateLimited,
        ErrorCode::Unauthorized,
        ErrorCode::ShardUnavailable,
        ErrorCode::NegotiationRequired,
        ErrorCode::IncompatibleManifest,
    ];
    for code in required {
        assert!(ErrorCode::ALL.contains(&code), "{code} is missing");
    }
}

#[test]
fn a_disconnect_keeps_a_valid_cursor() {
    let applied = vector(&[(1, 5)]);
    for boundary in ["tail", "complete", "diff"] {
        let token = cursor(applied.clone());
        let mut first = ReferenceTransport::new(config());
        first.send(hello());
        first.send(subscribe(Some(token.clone())));
        first.deliver(Event::Disconnected);
        assert_eq!(first.authority.phase(), SessionPhase::Closed, "{boundary}");

        let mut second = ReferenceTransport::new(config());
        second.send(hello());
        let responses = second.send(subscribe(Some(token)));
        let Frame::Subscribed(subscribed) = &responses[0] else {
            panic!("{boundary}: expected Subscribed, found {responses:?}");
        };
        assert!(!subscribed.snapshot_required, "{boundary}");
        assert_eq!(subscribed.starting_version, applied, "{boundary}");
    }
}

#[test]
fn a_snapshot_offer_requires_available_snapshot_data() {
    let mut config = config();
    config.cursor.snapshot_available = false;
    let mut transport = ReferenceTransport::new(config);
    transport.send(hello());
    let responses = transport.send(subscribe(None));
    assert_eq!(error_code(&responses[0]), ErrorCode::SnapshotUnavailable);
}

#[test]
fn a_close_command_has_a_named_code() {
    let command = Command::Close(CloseFrame::new(ErrorCode::RateLimited, "slow"));
    match command {
        Command::Close(close) => assert_eq!(close.code, ErrorCode::RateLimited),
        Command::Send(frame) => panic!("expected a close, found {frame:?}"),
    }
}

fn ready_frame() -> oxkv_wire::frame::SessionReady {
    oxkv_wire::frame::SessionReady {
        session_id: oxkv_contracts::version::Hash32::from_bytes([1; 32]),
        descriptor_digest: DescriptorDigest::from_bytes([1; 32]),
        sync_version: SyncWireVersion::new(1),
        manifest_digest: ManifestDigest::from_bytes([2; 32]),
        manifest_revision: ManifestRevision::new(5),
        map_version: MapVersion::new(4),
        capabilities: Capabilities::from_names(["sync.diff.v1"]),
        limits: oxkv_wire::types::Limits::standard(),
    }
}

fn subscribed_frame() -> oxkv_wire::frame::Subscribed {
    oxkv_wire::frame::Subscribed {
        subscription_id: SUBSCRIPTION,
        scope_identity: SCOPE_IDENTITY,
        effective_policy_version: PolicyVersion::from_bytes([4; 32]),
        scope_generation: SCOPE_GENERATION,
        starting_version: vector(&[(1, 5)]),
        cursor: cursor(vector(&[(1, 5)])),
        snapshot_required: false,
        oldest_readable: vector(&[(1, 4)]),
    }
}

fn snapshot_offer() -> oxkv_wire::frame::SnapshotOffer {
    support::snapshot_offer()
}

fn snapshot_tail() -> oxkv_wire::frame::SnapshotTail {
    oxkv_wire::frame::SnapshotTail {
        subscription_id: SUBSCRIPTION,
        snapshot_version: vector(&[(1, 5)]),
        cursor: cursor(vector(&[(1, 5)])),
        start_version: vector(&[(1, 5)]),
    }
}

fn snapshot_complete() -> oxkv_wire::frame::SnapshotComplete {
    oxkv_wire::frame::SnapshotComplete {
        subscription_id: SUBSCRIPTION,
        snapshot_version: vector(&[(1, 5)]),
        cursor: cursor(vector(&[(1, 5)])),
    }
}

fn receipt_frame() -> oxkv_wire::frame::ReceiptFrame {
    oxkv_wire::frame::ReceiptFrame {
        receipt_id: oxkv_contracts::version::Hash32::from_bytes([6; 32]),
        state: oxkv_contracts::receipt::ReceiptState::Committed,
        payload: b"receipt".to_vec(),
    }
}
