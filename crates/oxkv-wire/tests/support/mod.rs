//! Test support: deterministic fixtures and an in-process reference transport.
//!
//! The reference transport moves every frame through its canonical bytes. It
//! decodes the bytes again before it delivers the frame. A byte change fails
//! the test at the transport boundary.

#![allow(dead_code)]
#![allow(unreachable_pub)]
#![allow(clippy::chunks_exact_to_as_chunks)]

use oxkv_contracts::cursor::{CursorToken, ScopeGeneration};
use oxkv_contracts::version::{
    CommitVersion, DescriptorDigest, Hash32, ManifestDigest, ManifestRevision, MapDigest,
    MapVersion, PageId, PageSequence, PolicyVersion, ScopeIdentity, ScopeVersionVector, ShardId,
    SyncWireVersion, VersionVector,
};
use oxkv_wire::cursor::{CursorPolicy, CursorSigner};
use oxkv_wire::diff::{DiffBatch, DiffEntry, DiffOperation};
use oxkv_wire::frame::{Ack, Credit, Frame, SessionHello, SnapshotOffer, SnapshotPage, Subscribe};
use oxkv_wire::session::{Command, Event, Session, SessionConfig, SnapshotReady};
use oxkv_wire::types::{
    CAPABILITY_DIFF_V1, CAPABILITY_FLOW_CONTROL_V1, CAPABILITY_KEEPALIVE_V1,
    CAPABILITY_POLICY_OPAQUE_V1, CAPABILITY_RECEIPT_V1, CAPABILITY_RESUME_V1,
    CAPABILITY_SNAPSHOT_V1, Capabilities, Limits, MessageSequence, SubscriptionId,
};

/// The largest frame that the reference transport accepts.
pub const MAX_FRAME_BYTES: usize = 1_048_576;

/// The test cursor signing key.
pub const CURSOR_KEY: [u8; 32] = [7; 32];

/// The test subscription identity.
pub const SUBSCRIPTION: SubscriptionId = SubscriptionId::new(7);

/// The test scope generation.
pub const SCOPE_GENERATION: ScopeGeneration = ScopeGeneration::new(3);

/// The test scope identity.
pub const SCOPE_IDENTITY: ScopeIdentity = ScopeIdentity::from_bytes([3; 32]);

/// The test policy version.
pub const POLICY_VERSION: PolicyVersion = PolicyVersion::from_bytes([4; 32]);

/// The opaque policy payload. The wire layer must not rewrite these bytes.
pub const POLICY_PAYLOAD: &[u8] = b"policy-ast/v1:accounts:{\"all\":[]}";

/// The test scope prefix.
pub const PREFIX: &[u8] = b"account:";

/// Returns a deterministic scope version vector.
#[must_use]
pub fn vector(entries: &[(u8, u64)]) -> ScopeVersionVector {
    let mut vector = VersionVector::new();
    for (shard, version) in entries {
        vector.set(ShardId::new(vec![*shard]), CommitVersion::new(*version));
    }
    ScopeVersionVector::from_vector(vector)
}

/// Returns the reference session configuration.
#[must_use]
pub fn config() -> SessionConfig {
    SessionConfig {
        descriptor_digest: DescriptorDigest::from_bytes([1; 32]),
        manifest_digest: ManifestDigest::from_bytes([2; 32]),
        manifest_revision: ManifestRevision::new(5),
        minimum_manifest_revision: ManifestRevision::new(3),
        map_version: MapVersion::new(4),
        map_digest: MapDigest::from_bytes([6; 32]),
        minimum_sync_version: SyncWireVersion::new(1),
        maximum_sync_version: SyncWireVersion::new(1),
        capabilities: capabilities(),
        limits: Limits::standard(),
        cursor: CursorPolicy::new(
            CursorSigner::new(CURSOR_KEY),
            vector(&[(1, 4)]),
            vector(&[(1, 9)]),
            100,
            true,
        ),
    }
}

/// Returns the supported capabilities.
#[must_use]
pub fn capabilities() -> Capabilities {
    Capabilities::from_names([
        CAPABILITY_DIFF_V1,
        CAPABILITY_SNAPSHOT_V1,
        CAPABILITY_RESUME_V1,
        CAPABILITY_POLICY_OPAQUE_V1,
        CAPABILITY_RECEIPT_V1,
        CAPABILITY_FLOW_CONTROL_V1,
        CAPABILITY_KEEPALIVE_V1,
    ])
}

/// Returns a valid hello frame.
#[must_use]
pub fn hello() -> Frame {
    Frame::SessionHello(SessionHello {
        descriptor_digest: DescriptorDigest::from_bytes([1; 32]),
        sync_version: SyncWireVersion::new(1),
        manifest_digest: ManifestDigest::from_bytes([2; 32]),
        manifest_revision: ManifestRevision::new(5),
        map_version: MapVersion::new(4),
        map_digest: MapDigest::from_bytes([6; 32]),
        required_capabilities: Capabilities::from_names([
            CAPABILITY_DIFF_V1,
            CAPABILITY_SNAPSHOT_V1,
            CAPABILITY_RESUME_V1,
            CAPABILITY_FLOW_CONTROL_V1,
        ]),
        optional_capabilities: Capabilities::from_names([
            CAPABILITY_POLICY_OPAQUE_V1,
            CAPABILITY_RECEIPT_V1,
            CAPABILITY_KEEPALIVE_V1,
        ]),
        limits: Limits::standard(),
    })
}

/// Returns a subscribe frame with an optional cursor.
#[must_use]
pub fn subscribe(cursor: Option<Vec<u8>>) -> Frame {
    Frame::Subscribe(Subscribe {
        subscription_id: SUBSCRIPTION,
        scope_identity: SCOPE_IDENTITY,
        scope_generation: SCOPE_GENERATION,
        policy_version: POLICY_VERSION,
        policy_payload: POLICY_PAYLOAD.to_vec(),
        prefixes: vec![PREFIX.to_vec()],
        required_indexes: vec!["by_email".to_owned()],
        cursor,
        snapshot_requested: false,
        max_batch_entries: 64,
    })
}

/// Returns a credit grant that covers the reference transcript.
#[must_use]
pub fn full_credit() -> Frame {
    Frame::Credit(Credit {
        bytes: 1_048_576,
        messages: 1_024,
    })
}

/// Returns a snapshot offer for the reference subscription.
#[must_use]
pub fn snapshot_offer() -> SnapshotOffer {
    SnapshotOffer {
        subscription_id: SUBSCRIPTION,
        snapshot_id: Hash32::from_bytes([5; 32]),
        snapshot_version: vector(&[(1, 5)]),
        page_count: 2,
    }
}

/// Returns a snapshot page for the reference subscription.
#[must_use]
pub fn snapshot_page(index: u8) -> SnapshotPage {
    SnapshotPage::new(
        SUBSCRIPTION,
        PageSequence::new(u64::from(index)),
        PageId::from_bytes([index.saturating_add(10); 32]),
        vec![index; 4],
    )
}

/// Returns a snapshot event for the reference subscription.
#[must_use]
pub fn snapshot_ready() -> SnapshotReady {
    SnapshotReady {
        subscription_id: SUBSCRIPTION,
        offer: snapshot_offer(),
        pages: vec![snapshot_page(0), snapshot_page(1)],
        tail_start_version: vector(&[(1, 5)]),
    }
}

/// Returns diff entries that start at sequence zero.
#[must_use]
pub fn diffs() -> Vec<DiffEntry> {
    vec![
        DiffEntry {
            subscription_id: SUBSCRIPTION,
            scope_generation: SCOPE_GENERATION,
            sequence: MessageSequence::new(0),
            shard_id: ShardId::new(vec![1]),
            key: b"account:1".to_vec(),
            operation: DiffOperation::Put(b"one".to_vec()),
            commit_version: CommitVersion::new(6),
        },
        DiffEntry {
            subscription_id: SUBSCRIPTION,
            scope_generation: SCOPE_GENERATION,
            sequence: MessageSequence::new(1),
            shard_id: ShardId::new(vec![1]),
            key: b"account:2".to_vec(),
            operation: DiffOperation::Put(b"two".to_vec()),
            commit_version: CommitVersion::new(9),
        },
    ]
}

/// Returns a diff batch frame for the reference subscription.
#[must_use]
pub fn diff_batch() -> DiffBatch {
    DiffBatch::new(SUBSCRIPTION, diffs())
}

/// Returns an acknowledgement for the reference diff batch.
#[must_use]
pub fn ack() -> Frame {
    Frame::Ack(Ack {
        subscription_id: SUBSCRIPTION,
        sequence: MessageSequence::new(1),
        applied_version: vector(&[(1, 9)]),
    })
}

/// Returns a signed cursor at one applied version.
#[must_use]
pub fn cursor(applied: ScopeVersionVector) -> Vec<u8> {
    let policy = config().cursor;
    policy
        .signer
        .issue(
            &oxkv_wire::cursor::CursorContext::new(SCOPE_IDENTITY, SCOPE_GENERATION),
            applied,
            policy.retention_floor,
        )
        .to_token()
}

/// The in-process reference transport.
///
/// The transport is the only test driver. The session never sees a socket.
pub struct ReferenceTransport {
    /// The authority session under test.
    pub authority: Session,
    /// The server frames in delivery order.
    pub frames: Vec<Frame>,
    /// The server frame bytes in delivery order.
    pub encoded: Vec<Vec<u8>>,
}

impl ReferenceTransport {
    /// Creates a transport around one authority session.
    #[must_use]
    pub fn new(config: SessionConfig) -> Self {
        Self {
            authority: Session::new(config),
            frames: Vec::new(),
            encoded: Vec::new(),
        }
    }

    /// Sends one client frame through its canonical bytes.
    #[allow(clippy::needless_pass_by_value)]
    pub fn send(&mut self, frame: Frame) -> Vec<Frame> {
        let bytes = frame.encode();
        let decoded = Frame::decode(&bytes, MAX_FRAME_BYTES).expect("the client frame round trips");
        assert_eq!(decoded.encode(), bytes, "the client frame is canonical");
        self.deliver(Event::Incoming(decoded))
    }

    /// Delivers one local event to the authority.
    pub fn deliver(&mut self, event: Event) -> Vec<Frame> {
        let start = self.frames.len();
        let commands = self.authority.on_event(event);
        for command in commands {
            let frame = match command {
                Command::Send(frame) => frame,
                Command::Close(close) => Frame::Close(close),
            };
            let bytes = frame.encode();
            let decoded =
                Frame::decode(&bytes, MAX_FRAME_BYTES).expect("the server frame round trips");
            assert_eq!(decoded.encode(), bytes, "the server frame is canonical");
            self.encoded.push(bytes);
            self.frames.push(decoded);
        }
        self.frames[start..].to_vec()
    }

    /// Resets the recording and returns the bytes of the frames that follow.
    pub fn take_encoded(&mut self) -> Vec<Vec<u8>> {
        core::mem::take(&mut self.encoded)
    }
}

/// Returns the lowercase hex form of bytes.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// Decodes lowercase or uppercase hex text.
///
/// # Panics
///
/// Panics when the text has an odd length or a non-hex character.
#[must_use]
pub fn unhex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0, "hex text has an even length");
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_value(pair[0]);
            let low = hex_value(pair[1]);
            (high << 4) | low
        })
        .collect()
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("the hex text has a non-hex character"),
    }
}

/// Loads one golden vector file relative to `tests/vectors`.
#[must_use]
pub fn load(relative: &str) -> serde_json::Value {
    let path = format!("{}/tests/vectors/{relative}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read the golden file {path}: {error}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("cannot parse the golden file {path}: {error}"))
}
