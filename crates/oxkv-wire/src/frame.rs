//! Canonical frames for sync wire protocol v1.

use core::fmt;

use oxkv_contracts::cursor::ScopeGeneration;
use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::{
    DescriptorDigest, Hash32, ManifestDigest, ManifestRevision, MapDigest, MapVersion, PageId,
    PageSequence, PolicyVersion, ScopeIdentity, ScopeVersionVector, ShardId, SyncWireVersion,
};

use crate::codec::{CodecError, Decoder, Encoder};
use crate::diff::{DiffBatch, DiffEntry};
use crate::error::ErrorCode;
use crate::types::{Capabilities, Limits, MessageSequence, SubscriptionId};

/// The frozen sync frame format version.
pub const FRAME_VERSION: u8 = 1;

/// The frame header length in bytes.
pub const FRAME_HEADER_LENGTH: usize = 6;

/// The frame integrity length in bytes.
pub const FRAME_INTEGRITY_LENGTH: usize = 32;

const INTEGRITY_DOMAIN: &[u8] = b"oxkv/sync-frame/v1\0";

/// The frozen frame message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MessageType {
    /// The client opens a session.
    SessionHello,
    /// The authority accepts a session.
    SessionReady,
    /// The client subscribes to a scope.
    Subscribe,
    /// The authority accepts a subscription.
    Subscribed,
    /// The authority offers a snapshot.
    SnapshotOffer,
    /// The authority sends one snapshot page.
    SnapshotPage,
    /// The authority sends the snapshot tail cursor.
    SnapshotTail,
    /// The authority completes a snapshot.
    SnapshotComplete,
    /// The authority sends one diff.
    Diff,
    /// The authority sends a bounded diff batch.
    DiffBatch,
    /// The client acknowledges a diff sequence.
    Ack,
    /// The client grants byte and message credit.
    Credit,
    /// Either peer checks liveness.
    Keepalive,
    /// The authority sends a write receipt.
    Receipt,
    /// The client resumes receipt replay.
    ReceiptResume,
    /// The client resumes a diff stream.
    Resume,
    /// The authority accepts a resume.
    Resumed,
    /// Either peer sends a named error.
    Error,
    /// Either peer closes the session.
    Close,
}

impl MessageType {
    /// Every message type.
    pub const ALL: &'static [Self] = &[
        Self::SessionHello,
        Self::SessionReady,
        Self::Subscribe,
        Self::Subscribed,
        Self::SnapshotOffer,
        Self::SnapshotPage,
        Self::SnapshotTail,
        Self::SnapshotComplete,
        Self::Diff,
        Self::DiffBatch,
        Self::Ack,
        Self::Credit,
        Self::Keepalive,
        Self::Receipt,
        Self::ReceiptResume,
        Self::Resume,
        Self::Resumed,
        Self::Error,
        Self::Close,
    ];

    /// Returns the stable frame tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::SessionHello => 1,
            Self::SessionReady => 2,
            Self::Subscribe => 3,
            Self::Subscribed => 4,
            Self::SnapshotOffer => 5,
            Self::SnapshotPage => 6,
            Self::SnapshotTail => 7,
            Self::SnapshotComplete => 8,
            Self::Diff => 9,
            Self::DiffBatch => 10,
            Self::Ack => 11,
            Self::Credit => 12,
            Self::Keepalive => 13,
            Self::Receipt => 14,
            Self::ReceiptResume => 15,
            Self::Resume => 16,
            Self::Resumed => 17,
            Self::Error => 18,
            Self::Close => 19,
        }
    }

    /// Reads a message type from its stable frame tag.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::SessionHello),
            2 => Some(Self::SessionReady),
            3 => Some(Self::Subscribe),
            4 => Some(Self::Subscribed),
            5 => Some(Self::SnapshotOffer),
            6 => Some(Self::SnapshotPage),
            7 => Some(Self::SnapshotTail),
            8 => Some(Self::SnapshotComplete),
            9 => Some(Self::Diff),
            10 => Some(Self::DiffBatch),
            11 => Some(Self::Ack),
            12 => Some(Self::Credit),
            13 => Some(Self::Keepalive),
            14 => Some(Self::Receipt),
            15 => Some(Self::ReceiptResume),
            16 => Some(Self::Resume),
            17 => Some(Self::Resumed),
            18 => Some(Self::Error),
            19 => Some(Self::Close),
            _ => None,
        }
    }

    /// Returns the canonical message name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SessionHello => "SessionHello",
            Self::SessionReady => "SessionReady",
            Self::Subscribe => "Subscribe",
            Self::Subscribed => "Subscribed",
            Self::SnapshotOffer => "SnapshotOffer",
            Self::SnapshotPage => "SnapshotPage",
            Self::SnapshotTail => "SnapshotTail",
            Self::SnapshotComplete => "SnapshotComplete",
            Self::Diff => "Diff",
            Self::DiffBatch => "DiffBatch",
            Self::Ack => "Ack",
            Self::Credit => "Credit",
            Self::Keepalive => "Keepalive",
            Self::Receipt => "Receipt",
            Self::ReceiptResume => "ReceiptResume",
            Self::Resume => "Resume",
            Self::Resumed => "Resumed",
            Self::Error => "Error",
            Self::Close => "Close",
        }
    }
}

impl fmt::Display for MessageType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The client session negotiation frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHello {
    /// The RPC descriptor identity.
    pub descriptor_digest: DescriptorDigest,
    /// The client sync wire version.
    pub sync_version: SyncWireVersion,
    /// The client manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The client manifest revision.
    pub manifest_revision: ManifestRevision,
    /// The client shard map version.
    pub map_version: MapVersion,
    /// The client shard map digest.
    pub map_digest: MapDigest,
    /// The capabilities that the client requires.
    pub required_capabilities: Capabilities,
    /// The capabilities that the client can use.
    pub optional_capabilities: Capabilities,
    /// The client size and count limits.
    pub limits: Limits,
}

impl SessionHello {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.digest(self.descriptor_digest.as_bytes());
        encoder.u16(self.sync_version.get());
        encoder.digest(self.manifest_digest.as_bytes());
        encoder.u64(self.manifest_revision.get());
        encoder.u64(self.map_version.get());
        encoder.digest(self.map_digest.as_bytes());
        self.required_capabilities.encode(encoder);
        self.optional_capabilities.encode(encoder);
        self.limits.encode(encoder);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            descriptor_digest: DescriptorDigest::from_bytes(decoder.digest()?),
            sync_version: SyncWireVersion::new(decoder.u16()?),
            manifest_digest: ManifestDigest::from_bytes(decoder.digest()?),
            manifest_revision: ManifestRevision::new(decoder.u64()?),
            map_version: MapVersion::new(decoder.u64()?),
            map_digest: MapDigest::from_bytes(decoder.digest()?),
            required_capabilities: Capabilities::decode(decoder)?,
            optional_capabilities: Capabilities::decode(decoder)?,
            limits: Limits::decode(decoder)?,
        })
    }
}

/// The authority session acceptance frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReady {
    /// The deterministic session identity.
    pub session_id: Hash32,
    /// The chosen RPC descriptor identity.
    pub descriptor_digest: DescriptorDigest,
    /// The chosen sync wire version.
    pub sync_version: SyncWireVersion,
    /// The chosen manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The chosen manifest revision.
    pub manifest_revision: ManifestRevision,
    /// The chosen shard map version.
    pub map_version: MapVersion,
    /// The chosen shard map digest.
    pub map_digest: MapDigest,
    /// The capability intersection.
    pub capabilities: Capabilities,
    /// The negotiated limits.
    pub limits: Limits,
}

impl SessionReady {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.digest(self.session_id.as_bytes());
        encoder.digest(self.descriptor_digest.as_bytes());
        encoder.u16(self.sync_version.get());
        encoder.digest(self.manifest_digest.as_bytes());
        encoder.u64(self.manifest_revision.get());
        encoder.u64(self.map_version.get());
        encoder.digest(self.map_digest.as_bytes());
        self.capabilities.encode(encoder);
        self.limits.encode(encoder);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            session_id: Hash32::from_bytes(decoder.digest()?),
            descriptor_digest: DescriptorDigest::from_bytes(decoder.digest()?),
            sync_version: SyncWireVersion::new(decoder.u16()?),
            manifest_digest: ManifestDigest::from_bytes(decoder.digest()?),
            manifest_revision: ManifestRevision::new(decoder.u64()?),
            map_version: MapVersion::new(decoder.u64()?),
            map_digest: MapDigest::from_bytes(decoder.digest()?),
            capabilities: Capabilities::decode(decoder)?,
            limits: Limits::decode(decoder)?,
        })
    }
}

/// The client scope subscription frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscribe {
    /// The client subscription identity.
    pub subscription_id: SubscriptionId,
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
    /// The client scope generation.
    pub scope_generation: ScopeGeneration,
    /// The client policy version.
    pub policy_version: PolicyVersion,
    /// The opaque policy AST or scope descriptor.
    pub policy_payload: Vec<u8>,
    /// The key prefixes in the scope.
    pub prefixes: Vec<Vec<u8>>,
    /// The indexes that the scope requires.
    pub required_indexes: Vec<String>,
    /// The opaque resume cursor, when the client holds one.
    pub cursor: Option<Vec<u8>>,
    /// True when the client forces a snapshot.
    pub snapshot_requested: bool,
    /// The largest batch that the client accepts.
    pub max_batch_entries: u32,
}

impl Subscribe {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.digest(self.scope_identity.as_bytes());
        encoder.u64(self.scope_generation.get());
        encoder.digest(self.policy_version.as_bytes());
        encoder.bytes(&self.policy_payload);
        encoder.u32(u32::try_from(self.prefixes.len()).unwrap_or(u32::MAX));
        for prefix in &self.prefixes {
            encoder.bytes(prefix);
        }
        encoder.u32(u32::try_from(self.required_indexes.len()).unwrap_or(u32::MAX));
        for index in &self.required_indexes {
            encoder.text(index);
        }
        encoder.optional_bytes(self.cursor.as_deref());
        encoder.bool(self.snapshot_requested);
        encoder.u32(self.max_batch_entries);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        let subscription_id = SubscriptionId::new(decoder.u64()?);
        let scope_identity = ScopeIdentity::from_bytes(decoder.digest()?);
        let scope_generation = ScopeGeneration::new(decoder.u64()?);
        let policy_version = PolicyVersion::from_bytes(decoder.digest()?);
        let policy_payload = decoder.bytes()?.to_vec();
        let prefix_count = decoder.count()?;
        let mut prefixes = Vec::with_capacity(prefix_count);
        for _ in 0..prefix_count {
            prefixes.push(decoder.bytes()?.to_vec());
        }
        let index_count = decoder.count()?;
        let mut required_indexes = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            required_indexes.push(decoder.text()?);
        }
        let cursor = decoder.optional_bytes()?.map(<[u8]>::to_vec);
        let snapshot_requested = decoder.bool()?;
        let max_batch_entries = decoder.u32()?;
        Ok(Self {
            subscription_id,
            scope_identity,
            scope_generation,
            policy_version,
            policy_payload,
            prefixes,
            required_indexes,
            cursor,
            snapshot_requested,
            max_batch_entries,
        })
    }
}

/// The authority subscription acceptance frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscribed {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
    /// The effective policy version.
    pub effective_policy_version: PolicyVersion,
    /// The scope generation.
    pub scope_generation: ScopeGeneration,
    /// The first version in the diff stream.
    pub starting_version: ScopeVersionVector,
    /// The opaque resume cursor.
    pub cursor: Vec<u8>,
    /// True when the client must take a snapshot first.
    pub snapshot_required: bool,
    /// The oldest version that the authority can serve.
    pub oldest_readable: ScopeVersionVector,
}

impl Subscribed {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.digest(self.scope_identity.as_bytes());
        encoder.digest(self.effective_policy_version.as_bytes());
        encoder.u64(self.scope_generation.get());
        encoder.version_vector(&self.starting_version);
        encoder.bytes(&self.cursor);
        encoder.bool(self.snapshot_required);
        encoder.version_vector(&self.oldest_readable);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            subscription_id: SubscriptionId::new(decoder.u64()?),
            scope_identity: ScopeIdentity::from_bytes(decoder.digest()?),
            effective_policy_version: PolicyVersion::from_bytes(decoder.digest()?),
            scope_generation: ScopeGeneration::new(decoder.u64()?),
            starting_version: decoder.version_vector()?,
            cursor: decoder.bytes()?.to_vec(),
            snapshot_required: decoder.bool()?,
            oldest_readable: decoder.version_vector()?,
        })
    }
}

/// The authority snapshot offer frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotOffer {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The deterministic snapshot identity.
    pub snapshot_id: Hash32,
    /// The snapshot scope version.
    pub snapshot_version: ScopeVersionVector,
    /// The number of pages in the snapshot.
    pub page_count: u32,
}

impl SnapshotOffer {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.digest(self.snapshot_id.as_bytes());
        encoder.version_vector(&self.snapshot_version);
        encoder.u32(self.page_count);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            subscription_id: SubscriptionId::new(decoder.u64()?),
            snapshot_id: Hash32::from_bytes(decoder.digest()?),
            snapshot_version: decoder.version_vector()?,
            page_count: decoder.u32()?,
        })
    }
}

/// One snapshot page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPage {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The page sequence inside the snapshot.
    pub page_sequence: PageSequence,
    /// The deterministic page identity.
    pub page_id: PageId,
    /// The page bytes.
    pub payload: Vec<u8>,
    /// The CRC32C page integrity value.
    pub integrity: u32,
}

impl SnapshotPage {
    /// Creates a page and computes its CRC32C integrity value.
    #[must_use]
    pub fn new(
        subscription_id: SubscriptionId,
        page_sequence: PageSequence,
        page_id: PageId,
        payload: Vec<u8>,
    ) -> Self {
        let integrity = page_integrity(&payload);
        Self {
            subscription_id,
            page_sequence,
            page_id,
            payload,
            integrity,
        }
    }

    /// Returns true when the page integrity value matches its bytes.
    #[must_use]
    pub fn verify_integrity(&self) -> bool {
        self.integrity == page_integrity(&self.payload)
    }

    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.u64(self.page_sequence.get());
        encoder.digest(self.page_id.as_bytes());
        encoder.bytes(&self.payload);
        encoder.u32(self.integrity);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            subscription_id: SubscriptionId::new(decoder.u64()?),
            page_sequence: PageSequence::new(decoder.u64()?),
            page_id: PageId::from_bytes(decoder.digest()?),
            payload: decoder.bytes()?.to_vec(),
            integrity: decoder.u32()?,
        })
    }
}

fn page_integrity(payload: &[u8]) -> u32 {
    crate::crc32c::crc32c(payload)
}

/// The atomic handoff from a snapshot to the diff tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotTail {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The snapshot scope version.
    pub snapshot_version: ScopeVersionVector,
    /// The tail resume cursor.
    pub cursor: Vec<u8>,
    /// The first diff version after the snapshot.
    pub start_version: ScopeVersionVector,
}

impl SnapshotTail {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.version_vector(&self.snapshot_version);
        encoder.bytes(&self.cursor);
        encoder.version_vector(&self.start_version);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            subscription_id: SubscriptionId::new(decoder.u64()?),
            snapshot_version: decoder.version_vector()?,
            cursor: decoder.bytes()?.to_vec(),
            start_version: decoder.version_vector()?,
        })
    }
}

/// The snapshot completion frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotComplete {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The snapshot scope version.
    pub snapshot_version: ScopeVersionVector,
    /// The tail resume cursor.
    pub cursor: Vec<u8>,
}

impl SnapshotComplete {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.version_vector(&self.snapshot_version);
        encoder.bytes(&self.cursor);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            subscription_id: SubscriptionId::new(decoder.u64()?),
            snapshot_version: decoder.version_vector()?,
            cursor: decoder.bytes()?.to_vec(),
        })
    }
}

/// The client acknowledgement frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The last applied stream sequence.
    pub sequence: MessageSequence,
    /// The applied scope version.
    pub applied_version: ScopeVersionVector,
}

impl Ack {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.u64(self.sequence.get());
        encoder.version_vector(&self.applied_version);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            subscription_id: SubscriptionId::new(decoder.u64()?),
            sequence: MessageSequence::new(decoder.u64()?),
            applied_version: decoder.version_vector()?,
        })
    }
}

/// The client flow-control frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Credit {
    /// The additional byte credit.
    pub bytes: u64,
    /// The additional message credit.
    pub messages: u32,
}

impl Credit {
    pub(crate) fn encode(self, encoder: &mut Encoder) {
        encoder.u64(self.bytes);
        encoder.u32(self.messages);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            bytes: decoder.u64()?,
            messages: decoder.u32()?,
        })
    }
}

/// A liveness frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keepalive {
    /// The caller supplied nonce.
    pub nonce: u64,
}

impl Keepalive {
    pub(crate) fn encode(self, encoder: &mut Encoder) {
        encoder.u64(self.nonce);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            nonce: decoder.u64()?,
        })
    }
}

/// One write receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptFrame {
    /// The receipt identity.
    pub receipt_id: Hash32,
    /// The receipt state.
    pub state: ReceiptState,
    /// The opaque receipt payload.
    pub payload: Vec<u8>,
}

impl ReceiptFrame {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.digest(self.receipt_id.as_bytes());
        encoder.u8(self.state.wire_tag());
        encoder.bytes(&self.payload);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        let receipt_id = Hash32::from_bytes(decoder.digest()?);
        let state = ReceiptState::from_wire_tag(decoder.u8()?).ok_or(CodecError::InvalidTag)?;
        let payload = decoder.bytes()?.to_vec();
        Ok(Self {
            receipt_id,
            state,
            payload,
        })
    }
}

/// The client receipt replay resume frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptResume {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The opaque receipt cursor.
    pub cursor: Vec<u8>,
}

impl ReceiptResume {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.bytes(&self.cursor);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            subscription_id: SubscriptionId::new(decoder.u64()?),
            cursor: decoder.bytes()?.to_vec(),
        })
    }
}

/// The client diff resume frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resume {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The opaque sync cursor.
    pub cursor: Vec<u8>,
}

impl Resume {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.bytes(&self.cursor);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            subscription_id: SubscriptionId::new(decoder.u64()?),
            cursor: decoder.bytes()?.to_vec(),
        })
    }
}

/// The authority resume acceptance frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resumed {
    /// The subscription identity.
    pub subscription_id: SubscriptionId,
    /// The first version in the resumed diff stream.
    pub starting_version: ScopeVersionVector,
    /// The refreshed opaque cursor.
    pub cursor: Vec<u8>,
    /// The oldest version that the authority can serve.
    pub oldest_readable: ScopeVersionVector,
}

impl Resumed {
    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.u64(self.subscription_id.get());
        encoder.version_vector(&self.starting_version);
        encoder.bytes(&self.cursor);
        encoder.version_vector(&self.oldest_readable);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            subscription_id: SubscriptionId::new(decoder.u64()?),
            starting_version: decoder.version_vector()?,
            cursor: decoder.bytes()?.to_vec(),
            oldest_readable: decoder.version_vector()?,
        })
    }
}

/// A named protocol error with recovery flags.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProtocolErrorFrame {
    /// The frozen error code.
    pub code: ErrorCode,
    /// True when the client may retry the call.
    pub retryable: bool,
    /// True when the client must resynchronize.
    pub requires_resync: bool,
    /// True when a cursor remains valid.
    pub cursor_valid: bool,
    /// True when a snapshot fallback is available.
    pub snapshot_fallback: bool,
    /// The human-readable detail.
    pub detail: String,
    /// The lowest supported wire version, when the error is version related.
    pub supported_min: Option<u16>,
    /// The highest supported wire version, when the error is version related.
    pub supported_max: Option<u16>,
}

impl ProtocolErrorFrame {
    /// Creates an error frame with the recovery flags of its code.
    #[must_use]
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            retryable: code.retryable(),
            requires_resync: code.requires_resync(),
            cursor_valid: code.cursor_stays_valid(),
            snapshot_fallback: code.offers_snapshot(),
            detail: String::new(),
            supported_min: None,
            supported_max: None,
        }
    }

    /// Sets the human-readable detail.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// Sets the supported wire version range.
    #[must_use]
    pub const fn with_supported_range(mut self, minimum: u16, maximum: u16) -> Self {
        self.supported_min = Some(minimum);
        self.supported_max = Some(maximum);
        self
    }

    /// Sets the snapshot fallback flag.
    #[must_use]
    pub const fn with_snapshot_fallback(mut self, available: bool) -> Self {
        self.snapshot_fallback = available;
        self
    }

    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.text(self.code.as_str());
        encoder.bool(self.retryable);
        encoder.bool(self.requires_resync);
        encoder.bool(self.cursor_valid);
        encoder.bool(self.snapshot_fallback);
        encoder.text(&self.detail);
        encoder.bool(self.supported_min.is_some());
        if let (Some(minimum), Some(maximum)) = (self.supported_min, self.supported_max) {
            encoder.u16(minimum);
            encoder.u16(maximum);
        }
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        let code_text = decoder.text()?;
        let code = ErrorCode::from_code(&code_text).ok_or(CodecError::InvalidTag)?;
        let retryable = decoder.bool()?;
        let requires_resync = decoder.bool()?;
        let cursor_valid = decoder.bool()?;
        let snapshot_fallback = decoder.bool()?;
        let detail = decoder.text()?;
        let (supported_min, supported_max) = if decoder.bool()? {
            (Some(decoder.u16()?), Some(decoder.u16()?))
        } else {
            (None, None)
        };
        Ok(Self {
            code,
            retryable,
            requires_resync,
            cursor_valid,
            snapshot_fallback,
            detail,
            supported_min,
            supported_max,
        })
    }
}

/// The session close frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseFrame {
    /// The frozen error code that names the close cause.
    pub code: ErrorCode,
    /// The human-readable reason.
    pub reason: String,
}

impl CloseFrame {
    /// Creates a close frame.
    #[must_use]
    pub fn new(code: ErrorCode, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }

    pub(crate) fn encode(&self, encoder: &mut Encoder) {
        encoder.text(self.code.as_str());
        encoder.text(&self.reason);
    }

    pub(crate) fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CodecError> {
        let code = ErrorCode::from_code(&decoder.text()?).ok_or(CodecError::InvalidTag)?;
        let reason = decoder.text()?;
        Ok(Self { code, reason })
    }
}

/// One sync frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A client session hello.
    SessionHello(SessionHello),
    /// An authority session ready.
    SessionReady(SessionReady),
    /// A client scope subscription.
    Subscribe(Subscribe),
    /// An authority subscription acceptance.
    Subscribed(Subscribed),
    /// An authority snapshot offer.
    SnapshotOffer(SnapshotOffer),
    /// One snapshot page.
    SnapshotPage(SnapshotPage),
    /// A snapshot tail handoff.
    SnapshotTail(SnapshotTail),
    /// A snapshot completion.
    SnapshotComplete(SnapshotComplete),
    /// One diff entry.
    Diff(DiffEntry),
    /// A bounded diff batch.
    DiffBatch(DiffBatch),
    /// A client acknowledgement.
    Ack(Ack),
    /// A client credit grant.
    Credit(Credit),
    /// A liveness check.
    Keepalive(Keepalive),
    /// A write receipt.
    Receipt(ReceiptFrame),
    /// A client receipt replay resume.
    ReceiptResume(ReceiptResume),
    /// A client diff resume.
    Resume(Resume),
    /// An authority resume acceptance.
    Resumed(Resumed),
    /// A named protocol error.
    Error(ProtocolErrorFrame),
    /// A session close.
    Close(CloseFrame),
}

impl Frame {
    /// Returns the message type.
    #[must_use]
    pub const fn message_type(&self) -> MessageType {
        match self {
            Self::SessionHello(_) => MessageType::SessionHello,
            Self::SessionReady(_) => MessageType::SessionReady,
            Self::Subscribe(_) => MessageType::Subscribe,
            Self::Subscribed(_) => MessageType::Subscribed,
            Self::SnapshotOffer(_) => MessageType::SnapshotOffer,
            Self::SnapshotPage(_) => MessageType::SnapshotPage,
            Self::SnapshotTail(_) => MessageType::SnapshotTail,
            Self::SnapshotComplete(_) => MessageType::SnapshotComplete,
            Self::Diff(_) => MessageType::Diff,
            Self::DiffBatch(_) => MessageType::DiffBatch,
            Self::Ack(_) => MessageType::Ack,
            Self::Credit(_) => MessageType::Credit,
            Self::Keepalive(_) => MessageType::Keepalive,
            Self::Receipt(_) => MessageType::Receipt,
            Self::ReceiptResume(_) => MessageType::ReceiptResume,
            Self::Resume(_) => MessageType::Resume,
            Self::Resumed(_) => MessageType::Resumed,
            Self::Error(_) => MessageType::Error,
            Self::Close(_) => MessageType::Close,
        }
    }

    /// Returns the canonical payload bytes.
    #[must_use]
    pub fn encode_payload(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        match self {
            Self::SessionHello(frame) => frame.encode(&mut encoder),
            Self::SessionReady(frame) => frame.encode(&mut encoder),
            Self::Subscribe(frame) => frame.encode(&mut encoder),
            Self::Subscribed(frame) => frame.encode(&mut encoder),
            Self::SnapshotOffer(frame) => frame.encode(&mut encoder),
            Self::SnapshotPage(frame) => frame.encode(&mut encoder),
            Self::SnapshotTail(frame) => frame.encode(&mut encoder),
            Self::SnapshotComplete(frame) => frame.encode(&mut encoder),
            Self::Diff(frame) => frame.encode(&mut encoder),
            Self::DiffBatch(frame) => {
                encoder.u64(frame.subscription_id.get());
                encoder.u32(u32::try_from(frame.entries.len()).unwrap_or(u32::MAX));
                for entry in &frame.entries {
                    entry.encode(&mut encoder);
                }
            }
            Self::Ack(frame) => frame.encode(&mut encoder),
            Self::Credit(frame) => frame.encode(&mut encoder),
            Self::Keepalive(frame) => frame.encode(&mut encoder),
            Self::Receipt(frame) => frame.encode(&mut encoder),
            Self::ReceiptResume(frame) => frame.encode(&mut encoder),
            Self::Resume(frame) => frame.encode(&mut encoder),
            Self::Resumed(frame) => frame.encode(&mut encoder),
            Self::Error(frame) => frame.encode(&mut encoder),
            Self::Close(frame) => frame.encode(&mut encoder),
        }
        encoder.finish()
    }

    /// Encodes the frame as canonical bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let payload = self.encode_payload();
        self.encode_parts(&payload)
    }

    fn encode_parts(&self, payload: &[u8]) -> Vec<u8> {
        let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        let mut bytes =
            Vec::with_capacity(FRAME_HEADER_LENGTH + payload.len() + FRAME_INTEGRITY_LENGTH);
        bytes.push(FRAME_VERSION);
        bytes.push(self.message_type().tag());
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(payload);
        let integrity = frame_integrity(&bytes);
        bytes.extend_from_slice(integrity.as_bytes());
        bytes
    }

    /// Decodes one canonical frame.
    ///
    /// # Errors
    ///
    /// Returns a [`FrameError`] when the frame is truncated, too large, has an
    /// unknown version or type, or fails its integrity check.
    pub fn decode(bytes: &[u8], max_frame_bytes: usize) -> Result<Self, FrameError> {
        if bytes.len() > max_frame_bytes {
            return Err(FrameError::FrameTooLarge {
                length: bytes.len(),
                limit: max_frame_bytes,
            });
        }
        if bytes.len() < FRAME_HEADER_LENGTH + FRAME_INTEGRITY_LENGTH {
            return Err(FrameError::Truncated);
        }
        let version = bytes[0];
        if version != FRAME_VERSION {
            return Err(FrameError::UnsupportedVersion {
                found: version,
                supported: FRAME_VERSION,
            });
        }
        let message_type =
            MessageType::from_tag(bytes[1]).ok_or(FrameError::UnknownMessageType(bytes[1]))?;
        let length = usize::try_from(u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]))
            .map_err(|_| FrameError::LengthOverflow)?;
        let expected = FRAME_HEADER_LENGTH
            .checked_add(length)
            .and_then(|value| value.checked_add(FRAME_INTEGRITY_LENGTH))
            .ok_or(FrameError::LengthOverflow)?;
        if bytes.len() != expected {
            return Err(FrameError::LengthMismatch {
                expected,
                found: bytes.len(),
            });
        }
        let body_end = FRAME_HEADER_LENGTH + length;
        let integrity = frame_integrity(&bytes[..body_end]);
        if integrity.as_bytes().as_slice() != &bytes[body_end..] {
            return Err(FrameError::IntegrityMismatch);
        }
        let mut decoder = Decoder::new(&bytes[FRAME_HEADER_LENGTH..body_end]);
        let frame = Self::decode_payload(message_type, &mut decoder)
            .map_err(|_| FrameError::MalformedPayload)?;
        decoder.finish().map_err(|_| FrameError::MalformedPayload)?;
        Ok(frame)
    }

    fn decode_payload(
        message_type: MessageType,
        decoder: &mut Decoder<'_>,
    ) -> Result<Self, CodecError> {
        Ok(match message_type {
            MessageType::SessionHello => Self::SessionHello(SessionHello::decode(decoder)?),
            MessageType::SessionReady => Self::SessionReady(SessionReady::decode(decoder)?),
            MessageType::Subscribe => Self::Subscribe(Subscribe::decode(decoder)?),
            MessageType::Subscribed => Self::Subscribed(Subscribed::decode(decoder)?),
            MessageType::SnapshotOffer => Self::SnapshotOffer(SnapshotOffer::decode(decoder)?),
            MessageType::SnapshotPage => Self::SnapshotPage(SnapshotPage::decode(decoder)?),
            MessageType::SnapshotTail => Self::SnapshotTail(SnapshotTail::decode(decoder)?),
            MessageType::SnapshotComplete => {
                Self::SnapshotComplete(SnapshotComplete::decode(decoder)?)
            }
            MessageType::Diff => Self::Diff(DiffEntry::decode(decoder)?),
            MessageType::DiffBatch => {
                let subscription_id = SubscriptionId::new(decoder.u64()?);
                let count = decoder.count()?;
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    entries.push(DiffEntry::decode(decoder)?);
                }
                Self::DiffBatch(DiffBatch::new(subscription_id, entries))
            }
            MessageType::Ack => Self::Ack(Ack::decode(decoder)?),
            MessageType::Credit => Self::Credit(Credit::decode(decoder)?),
            MessageType::Keepalive => Self::Keepalive(Keepalive::decode(decoder)?),
            MessageType::Receipt => Self::Receipt(ReceiptFrame::decode(decoder)?),
            MessageType::ReceiptResume => Self::ReceiptResume(ReceiptResume::decode(decoder)?),
            MessageType::Resume => Self::Resume(Resume::decode(decoder)?),
            MessageType::Resumed => Self::Resumed(Resumed::decode(decoder)?),
            MessageType::Error => Self::Error(ProtocolErrorFrame::decode(decoder)?),
            MessageType::Close => Self::Close(CloseFrame::decode(decoder)?),
        })
    }
}

fn frame_integrity(bytes: &[u8]) -> Hash32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(INTEGRITY_DOMAIN);
    hasher.update(bytes);
    Hash32::from_bytes(*hasher.finalize().as_bytes())
}

/// An error from frame encoding or decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The input is shorter than a frame header plus integrity value.
    Truncated,
    /// The frame length prefix is too large.
    LengthOverflow,
    /// The frame format version is not supported.
    UnsupportedVersion {
        /// The version in the frame.
        found: u8,
        /// The supported frame version.
        supported: u8,
    },
    /// The message type tag is unknown.
    UnknownMessageType(u8),
    /// The declared length and the input length disagree.
    LengthMismatch {
        /// The expected length.
        expected: usize,
        /// The input length.
        found: usize,
    },
    /// The frame integrity value does not match.
    IntegrityMismatch,
    /// The frame is larger than the negotiated limit.
    FrameTooLarge {
        /// The input length.
        length: usize,
        /// The byte limit.
        limit: usize,
    },
    /// The payload is malformed.
    MalformedPayload,
}

impl FrameError {
    /// Returns the frozen wire error code for this failure.
    #[must_use]
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::UnsupportedVersion { .. } => ErrorCode::UnsupportedVersion,
            Self::Truncated
            | Self::LengthOverflow
            | Self::UnknownMessageType(_)
            | Self::LengthMismatch { .. }
            | Self::IntegrityMismatch
            | Self::FrameTooLarge { .. }
            | Self::MalformedPayload => ErrorCode::RpcCodecUnsupported,
        }
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("The frame is truncated."),
            Self::LengthOverflow => formatter.write_str("The frame length is too large."),
            Self::UnsupportedVersion { found, supported } => write!(
                formatter,
                "The frame version {found} is unsupported. Expected {supported}."
            ),
            Self::UnknownMessageType(tag) => {
                write!(formatter, "The frame message type {tag} is unknown.")
            }
            Self::LengthMismatch { expected, found } => write!(
                formatter,
                "The frame length is {found}. Expected {expected}."
            ),
            Self::IntegrityMismatch => formatter.write_str("The frame integrity check failed."),
            Self::FrameTooLarge { length, limit } => write!(
                formatter,
                "The frame has {length} bytes. The limit is {limit}."
            ),
            Self::MalformedPayload => formatter.write_str("The frame payload is malformed."),
        }
    }
}

impl std::error::Error for FrameError {}

/// Returns the shard identifier order for one diff entry.
#[must_use]
pub fn diff_order(entry: &DiffEntry) -> (&ShardId, &[u8]) {
    entry.order_key()
}
