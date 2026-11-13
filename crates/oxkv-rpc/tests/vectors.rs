//! Golden vectors and cross-layer tests for the generated RPC surface.

mod support;

use std::collections::BTreeMap;

use oxkv_contracts::version::{DescriptorDigest, Hash32, SyncWireVersion};
use oxkv_contracts::{ErrorCode, RetryClass};
use oxkv_rpc::{
    ConnectCodec, ConnectEnvelope, FramedTransport, ProtocolCore, ProtocolError, SessionConfig,
    SessionHelloFacts, Transport, TransportError, TransportFailure, reference_manifest,
    reference_surface, surface_from_manifest,
};

const GOLDEN_DIGEST: &str = "1e3374988b77c4a264d61119b66ae7ca27b5274969aee2f7a0e238dd3c166307";

fn reference_descriptor() -> oxkv_rpc::Descriptor {
    let surface = reference_surface().expect("the reference surface compiles");
    surface.descriptor().clone()
}

#[test]
fn two_clean_builds_produce_one_descriptor() {
    let first = surface_from_manifest(&reference_manifest()).expect("the first build compiles");
    let second = surface_from_manifest(&reference_manifest()).expect("the second build compiles");
    assert_eq!(
        first.descriptor().canonical_bytes(),
        second.descriptor().canonical_bytes()
    );
    assert_eq!(
        first.descriptor().canonical_text(),
        second.descriptor().canonical_text()
    );
    assert_eq!(first.descriptor_digest(), second.descriptor_digest());
    assert_eq!(
        first.descriptor().canonical_bytes(),
        include_bytes!("vectors/descriptor.bin")
    );
}

#[test]
fn the_descriptor_golden_bytes_are_pinned() {
    let descriptor = reference_descriptor();
    assert_eq!(
        descriptor.canonical_bytes(),
        include_bytes!("vectors/descriptor.bin")
    );
    assert_eq!(
        descriptor.canonical_text(),
        include_str!("vectors/descriptor.txt")
    );
}

#[test]
fn the_descriptor_digest_is_pinned() {
    let descriptor = reference_descriptor();
    assert_eq!(descriptor.digest().to_hex(), GOLDEN_DIGEST);
    let golden = DescriptorDigest::from_bytes(
        *Hash32::of(include_bytes!("vectors/descriptor.bin")).as_bytes(),
    );
    assert_eq!(golden.to_hex(), GOLDEN_DIGEST);
}

#[test]
fn every_control_operation_is_generated_once() {
    let surface = reference_surface().expect("the reference surface compiles");
    let value: serde_json::Value = serde_json::from_str(include_str!("vectors/control.json"))
        .expect("the control vector is valid JSON");
    assert_eq!(value["contract"], "descriptor/v1");
    assert_eq!(value["namespace"], "control");
    let operations = value["operations"]
        .as_array()
        .expect("the control vector has an operation list");
    assert_eq!(operations.len(), 8);
    assert_eq!(surface.control_methods().count(), operations.len());
    for operation in operations {
        let path = operation["path"]
            .as_str()
            .expect("the control operation names a path");
        let count = surface
            .methods()
            .iter()
            .filter(|method| method.path() == path)
            .count();
        assert_eq!(
            count, 1,
            "the control operation {path} is not generated once"
        );
        let method = surface.method(path).expect("the control method exists");
        assert_eq!(method.namespace().name(), "control");
        assert_eq!(
            method.request(),
            operation["request"].as_str().expect("the request name")
        );
        assert_eq!(
            method.response(),
            operation["response"].as_str().expect("the response name")
        );
        assert_eq!(
            method.streaming().name(),
            operation["streaming"]
                .as_str()
                .expect("the streaming shape")
        );
        assert_eq!(method.auth().name(), "operator");
        assert_ne!(method.errors().len(), 0);
        for code in method.errors() {
            assert!(
                ErrorCode::parse(code.as_str()).is_some(),
                "the control error {} is not registered",
                code.as_str()
            );
        }
    }
}

#[test]
fn a_policy_payload_survives_the_rpc_layer_byte_for_byte() {
    let policy = vec![0x00, 0xff, b'p', b'o', b'l', b'i', b'c', b'y', 0, 1, 2, 3];
    let mut responses = BTreeMap::new();
    responses.insert("sync/subscribe".to_owned(), vec![policy.clone()]);
    let mut transport = support::negotiated_transport(support::ScriptHandler::new(responses));
    let frames = transport
        .round_trip(&ConnectEnvelope::call(
            0,
            7,
            "sync/subscribe",
            policy.clone(),
        ))
        .expect("the call succeeds");
    assert_eq!(frames[0].payload(), Some(policy.as_slice()));
    assert_eq!(transport.core().handler().seen()[0].1, policy);
}

#[test]
fn a_sync_diff_frame_survives_the_connect_envelope() {
    let diff = vec![0x00, 0xff, 0x10, 0x20, b'd', b'i', b'f', b'f'];
    let mut responses = BTreeMap::new();
    responses.insert("sync/resume".to_owned(), vec![diff.clone()]);
    let mut transport = support::negotiated_transport(support::ScriptHandler::new(responses));
    let frames = transport
        .round_trip(&ConnectEnvelope::call(0, 9, "sync/resume", Vec::new()))
        .expect("the call succeeds");
    assert_eq!(frames[0].payload(), Some(diff.as_slice()));
    assert!(frames[1].is_end());
}

#[test]
fn the_browser_shape_completes_hello_subscribe_snapshot_tail_resume_receipt() {
    let policy = vec![0x01, 0x02, 0x03, 0xff];
    let snapshot_page = vec![b's', b'n', b'a', b'p'];
    let tail = vec![b't', b'a', b'i', b'l'];
    let diff = vec![b'd', b'i', b'f', b'f'];
    let receipt = vec![b'r', b'e', b'c', b'e', b'i', b'p', b't'];
    let mut responses = BTreeMap::new();
    responses.insert(
        "sync/subscribe".to_owned(),
        vec![snapshot_page.clone(), tail.clone()],
    );
    responses.insert("sync/resume".to_owned(), vec![tail.clone(), diff.clone()]);
    responses.insert("data/receipt_get".to_owned(), vec![receipt.clone()]);
    let mut transport = support::negotiated_transport(support::ScriptHandler::new(responses));

    let mut transcript = Vec::new();
    transcript.extend(
        transport
            .round_trip(&ConnectEnvelope::call(
                0,
                1,
                "sync/subscribe",
                policy.clone(),
            ))
            .expect("subscribe succeeds"),
    );
    transcript.extend(
        transport
            .round_trip(&ConnectEnvelope::call(1, 2, "sync/resume", tail.clone()))
            .expect("resume succeeds"),
    );
    transcript.extend(
        transport
            .round_trip(&ConnectEnvelope::call(
                2,
                3,
                "data/receipt_get",
                vec![1, 2, 3],
            ))
            .expect("the receipt call succeeds"),
    );

    let expected = vec![
        ConnectEnvelope::message(0, 1, snapshot_page),
        ConnectEnvelope::message(1, 1, tail.clone()),
        ConnectEnvelope::end_success(2, 1),
        ConnectEnvelope::message(0, 2, tail),
        ConnectEnvelope::message(1, 2, diff),
        ConnectEnvelope::end_success(2, 2),
        ConnectEnvelope::message(0, 3, receipt),
        ConnectEnvelope::end_success(1, 3),
    ];
    assert_eq!(transcript, expected);
    let seen: Vec<&str> = transport
        .core()
        .handler()
        .seen()
        .iter()
        .map(|(path, _)| path.as_str())
        .collect();
    assert_eq!(seen, ["sync/subscribe", "sync/resume", "data/receipt_get"]);
    assert_eq!(transport.core().handler().seen()[0].1, policy);
}

#[test]
fn a_descriptor_mismatch_returns_the_named_error() {
    let surface = reference_surface().expect("the reference surface compiles");
    let identity = surface.identity();
    let mut core = ProtocolCore::new(
        surface,
        support::config(identity),
        support::ScriptHandler::new(BTreeMap::new()),
    );
    let wrong = SessionHelloFacts::new(
        DescriptorDigest::from_bytes([9; 32]),
        identity.manifest_digest(),
        SyncWireVersion::new(1),
        ConnectCodec::Json,
    );
    let error = core.negotiate(&wrong).unwrap_err();
    assert_eq!(
        error,
        ProtocolError::DescriptorMismatch {
            expected: identity.descriptor_digest(),
            found: DescriptorDigest::from_bytes([9; 32]),
        }
    );
    assert_eq!(error.code().as_str(), "rpc.descriptor_mismatch");
}

#[test]
fn an_unknown_method_returns_the_named_error() {
    let mut transport = support::negotiated_transport(support::ScriptHandler::new(BTreeMap::new()));
    let frames = transport
        .round_trip(&ConnectEnvelope::call(0, 1, "data/unknown", Vec::new()))
        .expect("the transport succeeds");
    assert_eq!(
        frames[0]
            .status()
            .expect("the response ends")
            .code()
            .map(ErrorCode::as_str),
        Some("rpc.unknown_method")
    );
    let error = transport.core_mut().call("data/unknown", &[]).unwrap_err();
    assert_eq!(error.code().as_str(), "rpc.unknown_method");
}

#[test]
fn a_transport_error_maps_to_the_named_protocol_error() {
    struct ClosedTransport;

    impl Transport for ClosedTransport {
        fn round_trip(
            &mut self,
            _request: &ConnectEnvelope,
        ) -> Result<Vec<ConnectEnvelope>, TransportError> {
            Err(TransportError::new(TransportFailure::Closed))
        }
    }

    let call = ConnectEnvelope::call(0, 1, "data/get", Vec::new());
    let error = ClosedTransport.round_trip(&call).unwrap_err();
    assert_eq!(error.code().as_str(), "sync.shard_unavailable");
    let protocol = error.into_protocol_error();
    assert_eq!(protocol.code().as_str(), "sync.shard_unavailable");
    assert_eq!(protocol.retry(), RetryClass::Backoff);
    for failure in [
        TransportFailure::Closed,
        TransportFailure::CorruptFrame,
        TransportFailure::Protocol,
        TransportFailure::Backpressure,
    ] {
        let error = TransportError::new(failure);
        assert!(ErrorCode::parse(error.code().as_str()).is_some());
    }
}

#[test]
fn the_reference_and_framed_transports_produce_one_message_sequence() {
    fn responses() -> BTreeMap<String, Vec<Vec<u8>>> {
        let mut responses = BTreeMap::new();
        responses.insert(
            "sync/subscribe".to_owned(),
            vec![vec![b's', b'n', b'a', b'p'], vec![b't', b'a', b'i', b'l']],
        );
        responses.insert("sync/resume".to_owned(), vec![vec![b'd', b'i', b'f', b'f']]);
        responses.insert(
            "data/receipt_get".to_owned(),
            vec![vec![b'r', b'e', b'c', b'e', b'i', b'p', b't']],
        );
        responses
    }

    fn transcript<T: Transport>(mut transport: T) -> Vec<ConnectEnvelope> {
        let calls = [
            ConnectEnvelope::call(0, 1, "sync/subscribe", vec![0, 0xff, 1]),
            ConnectEnvelope::call(1, 2, "sync/resume", vec![2, 3, 4]),
            ConnectEnvelope::call(2, 3, "data/receipt_get", vec![5, 6]),
        ];
        let mut transcript = Vec::new();
        for call in &calls {
            transcript.extend(transport.round_trip(call).expect("the call succeeds"));
        }
        transcript
    }

    let reference = transcript(support::negotiated_transport(support::ScriptHandler::new(
        responses(),
    )));
    let framed = transcript(FramedTransport::new(support::negotiated_transport(
        support::ScriptHandler::new(responses()),
    )));
    assert_eq!(reference, framed);
}

#[test]
fn a_session_hello_negotiates_only_supported_facts() {
    let surface = reference_surface().expect("the reference surface compiles");
    let identity = surface.identity();
    let config = SessionConfig::new(identity, SyncWireVersion::new(1), vec![ConnectCodec::Json]);
    let mut core = ProtocolCore::new(
        surface,
        config,
        support::ScriptHandler::new(BTreeMap::new()),
    );
    let hello = SessionHelloFacts::new(
        identity.descriptor_digest(),
        identity.manifest_digest(),
        SyncWireVersion::new(1),
        ConnectCodec::Json,
    );
    core.negotiate(&hello)
        .expect("the hello facts are compatible");
    let error = core.negotiate(&hello).unwrap_err();
    assert_eq!(error.code().as_str(), "rpc.already_negotiated");
}
