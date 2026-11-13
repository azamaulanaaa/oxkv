//! The eight cursor token types.
//!
//! Each cursor type has its own field set, token version, expiry rule, and
//! named error. The token format is canonical and opaque to the client.
//!
//! A token has this layout:
//!
//! ```text
//! magic "OXC1" | cursor type | token version (u16) | payload | BLAKE3-256
//! ```
//!
//! The trailing hash covers the header and the payload. A tampered token
//! fails with `sync.cursor_invalid`. A token from one cursor type fails in
//! another type with `sync.cursor_type_mismatch`.

use core::fmt;
use std::collections::BTreeMap;

use crate::keyspace::TableName;
use crate::version::{
    CommitVersion, ContentDigest, DigestDomain, Hash32, PageSequence, ProjectionEpoch,
    ProjectionWatermarks, ScopeVersionVector, ShardId, SnapshotIdentity, SnapshotVersion,
    VersionVector,
};

/// The token magic bytes.
const MAGIC: [u8; 4] = *b"OXC1";

/// The token header length.
const HEADER_LENGTH: usize = 7;

/// The token digest length.
const DIGEST_LENGTH: usize = 32;

/// The default token version for every cursor type.
const DEFAULT_TOKEN_VERSION: u16 = 1;

/// One cursor type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CursorType {
    /// A key range cursor.
    Key,
    /// A scope diff cursor.
    Sync,
    /// A snapshot hydration cursor.
    Hydration,
    /// A receipt list cursor.
    Receipt,
    /// A per-key event cursor.
    Event,
    /// A search pagination cursor.
    Search,
    /// A projection watermark cursor.
    Projection,
    /// A cost frontier cursor.
    Cost,
}

impl CursorType {
    /// Every cursor type.
    pub const ALL: &'static [Self] = &[
        Self::Key,
        Self::Sync,
        Self::Hydration,
        Self::Receipt,
        Self::Event,
        Self::Search,
        Self::Projection,
        Self::Cost,
    ];

    /// Returns the canonical type name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Key => "KeyCursor",
            Self::Sync => "SyncCursor",
            Self::Hydration => "HydrationCursor",
            Self::Receipt => "ReceiptCursor",
            Self::Event => "EventCursor",
            Self::Search => "SearchCursor",
            Self::Projection => "ProjectionCursor",
            Self::Cost => "CostCursor",
        }
    }

    /// Returns the stable token type tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Key => 1,
            Self::Sync => 2,
            Self::Hydration => 3,
            Self::Receipt => 4,
            Self::Event => 5,
            Self::Search => 6,
            Self::Projection => 7,
            Self::Cost => 8,
        }
    }

    /// Reads a cursor type from its tag.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Key),
            2 => Some(Self::Sync),
            3 => Some(Self::Hydration),
            4 => Some(Self::Receipt),
            5 => Some(Self::Event),
            6 => Some(Self::Search),
            7 => Some(Self::Projection),
            8 => Some(Self::Cost),
            _ => None,
        }
    }

    /// Returns the named expiry error for this cursor type.
    #[must_use]
    pub const fn expiry_error(self) -> &'static str {
        match self {
            Self::Key | Self::Sync | Self::Hydration => "sync.cursor_expired",
            Self::Receipt => "receipt.expired",
            Self::Event => "event.retention_gap",
            Self::Search => "search.cursor_invalid",
            Self::Projection => "projection.epoch_invalid",
            Self::Cost => "query.budget_exceeded",
        }
    }

    /// Returns the version fields that this cursor type carries.
    #[must_use]
    pub const fn version_fields(self) -> &'static [&'static str] {
        match self {
            Self::Key => &["snapshot_version"],
            Self::Sync => &["version_vector", "retention_floor"],
            Self::Hydration => &["snapshot_identity", "page_sequence"],
            Self::Receipt => &["sequence"],
            Self::Event => &["position"],
            Self::Search => &["pin"],
            Self::Projection => &["source_watermarks", "epoch"],
            Self::Cost => &["frontiers", "versions"],
        }
    }
}

impl fmt::Display for CursorType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The expiry rule of one cursor type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorExpiry {
    /// The cursor expires with snapshot retention.
    SnapshotRetention,
    /// The cursor expires with diff retention.
    DiffRetention,
    /// The cursor expires with receipt retention.
    ReceiptRetention,
    /// The cursor expires with event retention.
    EventRetention,
    /// The cursor expires with the read pin lifetime.
    PinLifetime,
    /// The cursor expires with projection retention.
    ProjectionRetention,
    /// The cursor expires when its query budget is exhausted.
    QueryBudget,
}

impl CursorExpiry {
    /// Returns the canonical rule text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SnapshotRetention => "snapshot retention",
            Self::DiffRetention => "diff retention",
            Self::ReceiptRetention => "receipt retention",
            Self::EventRetention => "event retention",
            Self::PinLifetime => "pin lifetime",
            Self::ProjectionRetention => "projection retention",
            Self::QueryBudget => "query budget",
        }
    }
}

impl fmt::Display for CursorExpiry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// An error from cursor token handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CursorError {
    /// The token is malformed or tampered.
    #[error("The cursor token is invalid.")]
    Invalid,
    /// The token version is not supported.
    #[error("The cursor token version is unsupported. Expected {expected}, found {found}.")]
    UnsupportedVersion {
        /// The expected token version.
        expected: u16,
        /// The token version in the token.
        found: u16,
    },
    /// The token belongs to another cursor type.
    #[error("The cursor type does not match. Expected {expected}, found {found}.")]
    TypeMismatch {
        /// The expected cursor type.
        expected: CursorType,
        /// The cursor type in the token.
        found: CursorType,
    },
    /// The cursor is past its retention rule.
    #[error("The cursor expired under the {rule} rule.")]
    Expired {
        /// The cursor type.
        cursor: CursorType,
        /// The expiry rule.
        rule: CursorExpiry,
    },
}

impl CursorError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "sync.cursor_invalid",
            Self::UnsupportedVersion { .. } => "sync.unsupported_version",
            Self::TypeMismatch { .. } => "sync.cursor_type_mismatch",
            Self::Expired { cursor, .. } => cursor.expiry_error(),
        }
    }
}

/// The direction of a key range cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorDirection {
    /// Ascending byte order.
    Ascending,
    /// Descending byte order.
    Descending,
}

impl CursorDirection {
    /// Returns the stable wire tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Ascending => 0,
            Self::Descending => 1,
        }
    }

    /// Reads a direction from its tag.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Ascending),
            1 => Some(Self::Descending),
            _ => None,
        }
    }
}

/// A scope generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeGeneration(u64);

impl ScopeGeneration {
    /// Creates a scope generation.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A receipt sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReceiptSequence(u64);

impl ReceiptSequence {
    /// Creates a receipt sequence.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// An event sequence for one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventSequence(u64);

impl EventSequence {
    /// Creates an event sequence.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A query fingerprint.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueryFingerprint(Hash32);

impl QueryFingerprint {
    /// Creates a query fingerprint from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Hash32::from_bytes(bytes))
    }

    /// Returns the raw fingerprint bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for QueryFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("QueryFingerprint")
            .field(&self.0.to_hex())
            .finish()
    }
}

/// One component of a search sort tuple.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SortValue {
    /// A text value.
    Text(String),
    /// An opaque bytes value.
    Bytes(Vec<u8>),
    /// A signed integer value.
    Integer(i64),
}

impl SortValue {
    fn encode(&self, encoder: &mut Encoder) {
        match self {
            Self::Text(value) => {
                encoder.u8(1);
                encoder.text(value);
            }
            Self::Bytes(value) => {
                encoder.u8(2);
                encoder.bytes(value);
            }
            Self::Integer(value) => {
                encoder.u8(3);
                encoder.i64(*value);
            }
        }
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CursorError> {
        match decoder.u8()? {
            1 => Ok(Self::Text(decoder.text()?)),
            2 => Ok(Self::Bytes(decoder.bytes()?.to_vec())),
            3 => Ok(Self::Integer(decoder.i64()?)),
            _ => Err(CursorError::Invalid),
        }
    }
}

/// A search sort tuple.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct SortTuple(Vec<SortValue>);

impl SortTuple {
    /// Creates a sort tuple.
    #[must_use]
    pub fn new(values: Vec<SortValue>) -> Self {
        Self(values)
    }

    /// Returns the values.
    #[must_use]
    pub fn values(&self) -> &[SortValue] {
        &self.0
    }
}

/// A per-key event position or one compact watermark.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventPosition {
    /// One watermark for every key.
    Compact(EventSequence),
    /// One sequence per key.
    PerKey(BTreeMap<Vec<u8>, EventSequence>),
}

impl EventPosition {
    fn minimum(&self) -> EventSequence {
        match self {
            Self::Compact(sequence) => *sequence,
            Self::PerKey(entries) => entries
                .values()
                .copied()
                .min()
                .unwrap_or_else(|| EventSequence::new(0)),
        }
    }

    fn encode(&self, encoder: &mut Encoder) {
        match self {
            Self::Compact(sequence) => {
                encoder.u8(0);
                encoder.u64(sequence.get());
            }
            Self::PerKey(entries) => {
                encoder.u8(1);
                encoder.u64(u64::try_from(entries.len()).unwrap_or(u64::MAX));
                for (key, sequence) in entries {
                    encoder.bytes(key);
                    encoder.u64(sequence.get());
                }
            }
        }
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, CursorError> {
        match decoder.u8()? {
            0 => Ok(Self::Compact(EventSequence::new(decoder.u64()?))),
            1 => {
                let count = decoder.count()?;
                let mut entries = BTreeMap::new();
                for _ in 0..count {
                    let key = decoder.bytes()?.to_vec();
                    let sequence = EventSequence::new(decoder.u64()?);
                    entries.insert(key, sequence);
                }
                Ok(Self::PerKey(entries))
            }
            _ => Err(CursorError::Invalid),
        }
    }
}

/// One per-shard cost frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CostFrontier {
    /// The number of keys examined in the shard.
    pub keys_examined: u64,
}

impl CostFrontier {
    /// Creates a cost frontier.
    #[must_use]
    pub const fn new(keys_examined: u64) -> Self {
        Self { keys_examined }
    }
}

/// A key range cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCursor {
    /// The table name.
    pub table: TableName,
    /// The snapshot version.
    pub snapshot_version: SnapshotVersion,
    /// The last returned key.
    pub last_key: Vec<u8>,
    /// The scan direction.
    pub direction: CursorDirection,
}

impl KeyCursor {
    /// Creates a key range cursor.
    #[must_use]
    pub const fn new(
        table: TableName,
        snapshot_version: SnapshotVersion,
        last_key: Vec<u8>,
        direction: CursorDirection,
    ) -> Self {
        Self {
            table,
            snapshot_version,
            last_key,
            direction,
        }
    }

    /// Returns true when the cursor is still readable.
    #[must_use]
    pub const fn is_readable_at(&self, oldest_readable: SnapshotVersion) -> bool {
        self.snapshot_version.get() >= oldest_readable.get()
    }
}

impl CursorToken for KeyCursor {
    const CURSOR_TYPE: CursorType = CursorType::Key;

    fn encode_payload(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.text(self.table.as_str());
        encoder.u64(self.snapshot_version.get());
        encoder.bytes(&self.last_key);
        encoder.u8(self.direction.tag());
        encoder.finish()
    }

    fn decode_payload(payload: &[u8]) -> Result<Self, CursorError> {
        let mut decoder = Decoder::new(payload);
        let table = TableName::parse(&decoder.text()?).map_err(|_| CursorError::Invalid)?;
        let snapshot_version = SnapshotVersion::new(decoder.u64()?);
        let last_key = decoder.bytes()?.to_vec();
        let direction = CursorDirection::from_tag(decoder.u8()?).ok_or(CursorError::Invalid)?;
        decoder.finish()?;
        Ok(Self::new(table, snapshot_version, last_key, direction))
    }

    fn expiry_rule(&self) -> CursorExpiry {
        CursorExpiry::SnapshotRetention
    }
}

/// A scope diff cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SyncCursor {
    /// The scope generation.
    pub scope_generation: ScopeGeneration,
    /// The applied version vector.
    pub version_vector: ScopeVersionVector,
    /// The retention floor of the authority.
    pub retention_floor: ScopeVersionVector,
    /// The authentication tag.
    pub auth_tag: Hash32,
}

impl SyncCursor {
    /// Creates a scope diff cursor.
    #[must_use]
    pub const fn new(
        scope_generation: ScopeGeneration,
        version_vector: ScopeVersionVector,
        retention_floor: ScopeVersionVector,
        auth_tag: Hash32,
    ) -> Self {
        Self {
            scope_generation,
            version_vector,
            retention_floor,
            auth_tag,
        }
    }

    /// Returns true when the authority still retains the cursor.
    #[must_use]
    pub fn is_readable_at(&self, authority_floor: &ScopeVersionVector) -> bool {
        self.version_vector
            .as_vector()
            .entails(authority_floor.as_vector())
    }
}

impl CursorToken for SyncCursor {
    const CURSOR_TYPE: CursorType = CursorType::Sync;

    fn encode_payload(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u64(self.scope_generation.get());
        encoder.vector(self.version_vector.as_vector());
        encoder.vector(self.retention_floor.as_vector());
        encoder.hash(self.auth_tag.as_bytes());
        encoder.finish()
    }

    fn decode_payload(payload: &[u8]) -> Result<Self, CursorError> {
        let mut decoder = Decoder::new(payload);
        let scope_generation = ScopeGeneration::new(decoder.u64()?);
        let version_vector = ScopeVersionVector::from_vector(decoder.vector()?);
        let retention_floor = ScopeVersionVector::from_vector(decoder.vector()?);
        let auth_tag = Hash32::from_bytes(decoder.hash()?);
        decoder.finish()?;
        Ok(Self::new(
            scope_generation,
            version_vector,
            retention_floor,
            auth_tag,
        ))
    }

    fn expiry_rule(&self) -> CursorExpiry {
        CursorExpiry::DiffRetention
    }
}

/// A snapshot hydration cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HydrationCursor {
    /// The snapshot identity.
    pub snapshot_identity: SnapshotIdentity,
    /// The last verified page sequence.
    pub page_sequence: PageSequence,
    /// The digest of the verified page.
    pub verified_digest: ContentDigest,
}

impl HydrationCursor {
    /// Creates a hydration cursor.
    #[must_use]
    pub const fn new(
        snapshot_identity: SnapshotIdentity,
        page_sequence: PageSequence,
        verified_digest: ContentDigest,
    ) -> Self {
        Self {
            snapshot_identity,
            page_sequence,
            verified_digest,
        }
    }

    /// Returns true when the snapshot is still retained.
    #[must_use]
    pub const fn is_readable_at(&self, oldest_readable: SnapshotVersion) -> bool {
        self.snapshot_identity.commit_version.get() >= oldest_readable.get()
    }
}

impl CursorToken for HydrationCursor {
    const CURSOR_TYPE: CursorType = CursorType::Hydration;

    fn encode_payload(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.bytes(self.snapshot_identity.shard_id.as_bytes());
        encoder.u64(self.snapshot_identity.commit_version.get());
        encoder.hash(self.snapshot_identity.content_digest.as_bytes());
        encoder.u64(self.page_sequence.get());
        encoder.hash(self.verified_digest.as_bytes());
        encoder.finish()
    }

    fn decode_payload(payload: &[u8]) -> Result<Self, CursorError> {
        let mut decoder = Decoder::new(payload);
        let shard_id = ShardId::new(decoder.bytes()?.to_vec());
        let commit_version = CommitVersion::new(decoder.u64()?);
        let content_digest = ContentDigest::from_bytes(decoder.hash()?);
        let page_sequence = PageSequence::new(decoder.u64()?);
        let verified_digest = ContentDigest::from_bytes(decoder.hash()?);
        decoder.finish()?;
        Ok(Self::new(
            SnapshotIdentity::new(shard_id, commit_version, content_digest),
            page_sequence,
            verified_digest,
        ))
    }

    fn expiry_rule(&self) -> CursorExpiry {
        CursorExpiry::SnapshotRetention
    }
}

/// A receipt list cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReceiptCursor {
    /// The last receipt identity.
    pub last_receipt_id: crate::receipt::ReceiptId,
    /// The last receipt sequence.
    pub sequence: ReceiptSequence,
}

impl ReceiptCursor {
    /// Creates a receipt cursor.
    #[must_use]
    pub const fn new(
        last_receipt_id: crate::receipt::ReceiptId,
        sequence: ReceiptSequence,
    ) -> Self {
        Self {
            last_receipt_id,
            sequence,
        }
    }

    /// Returns true when the receipt is still retained.
    #[must_use]
    pub const fn is_readable_at(&self, oldest_sequence: ReceiptSequence) -> bool {
        self.sequence.get() >= oldest_sequence.get()
    }
}

impl CursorToken for ReceiptCursor {
    const CURSOR_TYPE: CursorType = CursorType::Receipt;

    fn encode_payload(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.hash(self.last_receipt_id.as_bytes());
        encoder.u64(self.sequence.get());
        encoder.finish()
    }

    fn decode_payload(payload: &[u8]) -> Result<Self, CursorError> {
        let mut decoder = Decoder::new(payload);
        let last_receipt_id = crate::receipt::ReceiptId::from_bytes(decoder.hash()?);
        let sequence = ReceiptSequence::new(decoder.u64()?);
        decoder.finish()?;
        Ok(Self::new(last_receipt_id, sequence))
    }

    fn expiry_rule(&self) -> CursorExpiry {
        CursorExpiry::ReceiptRetention
    }
}

/// A per-key event cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EventCursor {
    /// The per-key position or the compact watermark.
    pub position: EventPosition,
}

impl EventCursor {
    /// Creates an event cursor.
    #[must_use]
    pub const fn new(position: EventPosition) -> Self {
        Self { position }
    }

    /// Returns true when every position is at or above the retention floor.
    #[must_use]
    pub fn is_readable_at(&self, floor: &Self) -> bool {
        self.position.minimum() >= floor.position.minimum()
    }
}

impl CursorToken for EventCursor {
    const CURSOR_TYPE: CursorType = CursorType::Event;

    fn encode_payload(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        self.position.encode(&mut encoder);
        encoder.finish()
    }

    fn decode_payload(payload: &[u8]) -> Result<Self, CursorError> {
        let mut decoder = Decoder::new(payload);
        let position = EventPosition::decode(&mut decoder)?;
        decoder.finish()?;
        Ok(Self::new(position))
    }

    fn expiry_rule(&self) -> CursorExpiry {
        CursorExpiry::EventRetention
    }
}

/// A search pagination cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SearchCursor {
    /// The pinned version vector.
    pub pin: ScopeVersionVector,
    /// The query fingerprint.
    pub query_fingerprint: QueryFingerprint,
    /// The last sort tuple.
    pub sort_tuple: SortTuple,
}

impl SearchCursor {
    /// Creates a search cursor.
    #[must_use]
    pub const fn new(
        pin: ScopeVersionVector,
        query_fingerprint: QueryFingerprint,
        sort_tuple: SortTuple,
    ) -> Self {
        Self {
            pin,
            query_fingerprint,
            sort_tuple,
        }
    }

    /// Returns true when the pin is still readable.
    #[must_use]
    pub fn is_readable_at(&self, oldest_readable: &ScopeVersionVector) -> bool {
        self.pin.as_vector().entails(oldest_readable.as_vector())
    }
}

impl CursorToken for SearchCursor {
    const CURSOR_TYPE: CursorType = CursorType::Search;

    fn encode_payload(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.vector(self.pin.as_vector());
        encoder.hash(self.query_fingerprint.as_bytes());
        encoder.u64(u64::try_from(self.sort_tuple.0.len()).unwrap_or(u64::MAX));
        for value in &self.sort_tuple.0 {
            value.encode(&mut encoder);
        }
        encoder.finish()
    }

    fn decode_payload(payload: &[u8]) -> Result<Self, CursorError> {
        let mut decoder = Decoder::new(payload);
        let pin = ScopeVersionVector::from_vector(decoder.vector()?);
        let query_fingerprint = QueryFingerprint::from_bytes(decoder.hash()?);
        let count = decoder.count()?;
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(SortValue::decode(&mut decoder)?);
        }
        decoder.finish()?;
        Ok(Self::new(pin, query_fingerprint, SortTuple::new(values)))
    }

    fn expiry_rule(&self) -> CursorExpiry {
        CursorExpiry::PinLifetime
    }
}

/// A projection watermark cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectionCursor {
    /// The watermarks per fact source.
    pub source_watermarks: ProjectionWatermarks,
    /// The projection epoch.
    pub epoch: ProjectionEpoch,
}

impl ProjectionCursor {
    /// Creates a projection cursor.
    #[must_use]
    pub const fn new(source_watermarks: ProjectionWatermarks, epoch: ProjectionEpoch) -> Self {
        Self {
            source_watermarks,
            epoch,
        }
    }

    /// Returns true when the epoch is still retained.
    #[must_use]
    pub const fn is_readable_at(&self, oldest_epoch: ProjectionEpoch) -> bool {
        self.epoch.get() >= oldest_epoch.get()
    }
}

impl CursorToken for ProjectionCursor {
    const CURSOR_TYPE: CursorType = CursorType::Projection;

    fn encode_payload(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u64(u64::try_from(self.source_watermarks.iter().count()).unwrap_or(u64::MAX));
        for (source, version) in self.source_watermarks.iter() {
            encoder.text(source.as_str());
            encoder.u64(version.get());
        }
        encoder.u64(self.epoch.get());
        encoder.finish()
    }

    fn decode_payload(payload: &[u8]) -> Result<Self, CursorError> {
        let mut decoder = Decoder::new(payload);
        let count = decoder.count()?;
        let mut watermarks = ProjectionWatermarks::new();
        for _ in 0..count {
            let source = crate::version::FactSourceId::new(decoder.text()?);
            let version = crate::version::FactVersion::new(decoder.u64()?);
            watermarks.set(source, version);
        }
        let epoch = ProjectionEpoch::new(decoder.u64()?);
        decoder.finish()?;
        Ok(Self::new(watermarks, epoch))
    }

    fn expiry_rule(&self) -> CursorExpiry {
        CursorExpiry::ProjectionRetention
    }
}

/// A cost frontier cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CostCursor {
    /// The frontier per shard.
    pub frontiers: BTreeMap<ShardId, CostFrontier>,
    /// The version set.
    pub versions: ScopeVersionVector,
    /// The remaining query budget.
    pub remaining_budget: u64,
}

impl CostCursor {
    /// Creates a cost frontier cursor.
    #[must_use]
    pub const fn new(
        frontiers: BTreeMap<ShardId, CostFrontier>,
        versions: ScopeVersionVector,
        remaining_budget: u64,
    ) -> Self {
        Self {
            frontiers,
            versions,
            remaining_budget,
        }
    }

    /// Returns true when the cursor still has query budget.
    #[must_use]
    pub const fn has_budget(&self) -> bool {
        self.remaining_budget > 0
    }
}

impl CursorToken for CostCursor {
    const CURSOR_TYPE: CursorType = CursorType::Cost;

    fn encode_payload(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.u64(u64::try_from(self.frontiers.len()).unwrap_or(u64::MAX));
        for (shard_id, frontier) in &self.frontiers {
            encoder.bytes(shard_id.as_bytes());
            encoder.u64(frontier.keys_examined);
        }
        encoder.vector(self.versions.as_vector());
        encoder.u64(self.remaining_budget);
        encoder.finish()
    }

    fn decode_payload(payload: &[u8]) -> Result<Self, CursorError> {
        let mut decoder = Decoder::new(payload);
        let count = decoder.count()?;
        let mut frontiers = BTreeMap::new();
        for _ in 0..count {
            let shard_id = ShardId::new(decoder.bytes()?.to_vec());
            let keys_examined = decoder.u64()?;
            frontiers.insert(shard_id, CostFrontier::new(keys_examined));
        }
        let versions = ScopeVersionVector::from_vector(decoder.vector()?);
        let remaining_budget = decoder.u64()?;
        decoder.finish()?;
        Ok(Self::new(frontiers, versions, remaining_budget))
    }

    fn expiry_rule(&self) -> CursorExpiry {
        CursorExpiry::QueryBudget
    }
}

/// A cursor token codec.
pub trait CursorToken: Sized {
    /// The cursor type of this token.
    const CURSOR_TYPE: CursorType;

    /// The token version of this cursor type.
    const TOKEN_VERSION: u16 = DEFAULT_TOKEN_VERSION;

    /// Encodes the cursor fields. The encoder is canonical.
    fn encode_payload(&self) -> Vec<u8>;

    /// Decodes the cursor fields. The decoder must consume every byte.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError::Invalid`] when the payload is malformed.
    fn decode_payload(payload: &[u8]) -> Result<Self, CursorError>;

    /// Returns the expiry rule of this cursor.
    #[must_use]
    fn expiry_rule(&self) -> CursorExpiry;

    /// Returns the named expiry error of this cursor type.
    #[must_use]
    fn expiry_error() -> &'static str {
        Self::CURSOR_TYPE.expiry_error()
    }

    /// Encodes the cursor as one versioned opaque token.
    #[must_use]
    fn to_token(&self) -> Vec<u8> {
        let payload = self.encode_payload();
        let mut token = Vec::with_capacity(HEADER_LENGTH + payload.len() + DIGEST_LENGTH);
        token.extend_from_slice(&MAGIC);
        token.push(Self::CURSOR_TYPE.tag());
        token.extend_from_slice(&Self::TOKEN_VERSION.to_be_bytes());
        token.extend_from_slice(&payload);
        let digest = Hash32::of_domain(DigestDomain::CursorToken, &token);
        token.extend_from_slice(digest.as_bytes());
        token
    }

    /// Decodes a cursor from one versioned opaque token.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError::Invalid`] for a malformed or tampered token.
    /// Returns [`CursorError::TypeMismatch`] for a token from another cursor
    /// type. Returns [`CursorError::UnsupportedVersion`] for an unknown token
    /// version.
    fn from_token(token: &[u8]) -> Result<Self, CursorError> {
        if token.len() < HEADER_LENGTH + DIGEST_LENGTH {
            return Err(CursorError::Invalid);
        }
        if token[..4] != MAGIC {
            return Err(CursorError::Invalid);
        }
        let found = CursorType::from_tag(token[4]).ok_or(CursorError::Invalid)?;
        if found != Self::CURSOR_TYPE {
            return Err(CursorError::TypeMismatch {
                expected: Self::CURSOR_TYPE,
                found,
            });
        }
        let found_version = u16::from_be_bytes([token[5], token[6]]);
        if found_version != Self::TOKEN_VERSION {
            return Err(CursorError::UnsupportedVersion {
                expected: Self::TOKEN_VERSION,
                found: found_version,
            });
        }
        let body_end = token.len() - DIGEST_LENGTH;
        let expected = Hash32::of_domain(DigestDomain::CursorToken, &token[..body_end]);
        if expected.as_bytes().as_slice() != &token[body_end..] {
            return Err(CursorError::Invalid);
        }
        Self::decode_payload(&token[HEADER_LENGTH..body_end])
    }
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn hash(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    fn bytes(&mut self, value: &[u8]) {
        self.u64(u64::try_from(value.len()).unwrap_or(u64::MAX));
        self.bytes.extend_from_slice(value);
    }

    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn vector(&mut self, vector: &VersionVector) {
        self.u64(u64::try_from(vector.len()).unwrap_or(u64::MAX));
        for (shard_id, commit_version) in vector.iter() {
            self.bytes(shard_id.as_bytes());
            self.u64(commit_version.get());
        }
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn finish(&self) -> Result<(), CursorError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(CursorError::Invalid)
        }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], CursorError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(CursorError::Invalid)?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(CursorError::Invalid)?;
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, CursorError> {
        let slice = self.take(1)?;
        Ok(slice[0])
    }

    fn u64(&mut self) -> Result<u64, CursorError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(u64::from_be_bytes(bytes))
    }

    fn i64(&mut self) -> Result<i64, CursorError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(i64::from_be_bytes(bytes))
    }

    fn hash(&mut self) -> Result<[u8; 32], CursorError> {
        let slice = self.take(32)?;
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(slice);
        Ok(bytes)
    }

    fn bytes(&mut self) -> Result<&'a [u8], CursorError> {
        let length = self.u64()?;
        let length = usize::try_from(length).map_err(|_| CursorError::Invalid)?;
        self.take(length)
    }

    fn text(&mut self) -> Result<String, CursorError> {
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| CursorError::Invalid)
    }

    fn count(&mut self) -> Result<usize, CursorError> {
        let count = self.u64()?;
        let count = usize::try_from(count).map_err(|_| CursorError::Invalid)?;
        // Every entry needs at least one byte. This rejects a huge count before
        // an allocation.
        if count > self.bytes.len() - self.position {
            return Err(CursorError::Invalid);
        }
        Ok(count)
    }

    fn vector(&mut self) -> Result<VersionVector, CursorError> {
        let count = self.count()?;
        let mut vector = VersionVector::new();
        for _ in 0..count {
            let shard_id = ShardId::new(self.bytes()?.to_vec());
            let commit_version = CommitVersion::new(self.u64()?);
            vector.set(shard_id, commit_version);
        }
        Ok(vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::ReceiptId;
    use crate::version::{FactSourceId, FactVersion};

    fn scope_vector(entries: &[(&[u8], u64)]) -> ScopeVersionVector {
        ScopeVersionVector::from_vector(VersionVector::from_entries(
            entries.iter().map(|(shard, version)| {
                (ShardId::new(shard.to_vec()), CommitVersion::new(*version))
            }),
        ))
    }

    fn sample_key() -> KeyCursor {
        KeyCursor::new(
            TableName::parse_user("accounts").expect("the table name is valid"),
            SnapshotVersion::new(18),
            vec![b'k', 1],
            CursorDirection::Ascending,
        )
    }

    fn sample_sync() -> SyncCursor {
        SyncCursor::new(
            ScopeGeneration::new(3),
            scope_vector(&[(&[1], 18), (&[2], 9)]),
            scope_vector(&[(&[1], 4)]),
            Hash32::from_bytes([7; 32]),
        )
    }

    fn sample_hydration() -> HydrationCursor {
        HydrationCursor::new(
            SnapshotIdentity::new(
                ShardId::new(vec![1]),
                CommitVersion::new(18),
                ContentDigest::from_bytes([8; 32]),
            ),
            PageSequence::new(2),
            ContentDigest::from_bytes([9; 32]),
        )
    }

    fn sample_receipt() -> ReceiptCursor {
        ReceiptCursor::new(ReceiptId::from_bytes([10; 32]), ReceiptSequence::new(5))
    }

    fn sample_event() -> EventCursor {
        let mut entries = BTreeMap::new();
        entries.insert(vec![b'a'], EventSequence::new(4));
        entries.insert(vec![b'b'], EventSequence::new(7));
        EventCursor::new(EventPosition::PerKey(entries))
    }

    fn sample_search() -> SearchCursor {
        SearchCursor::new(
            scope_vector(&[(&[1], 18)]),
            QueryFingerprint::from_bytes([11; 32]),
            SortTuple::new(vec![
                SortValue::Integer(42),
                SortValue::Text("alpha".to_owned()),
                SortValue::Bytes(vec![1, 2, 3]),
            ]),
        )
    }

    fn sample_projection() -> ProjectionCursor {
        let mut watermarks = ProjectionWatermarks::new();
        watermarks.set(FactSourceId::new("accounts"), FactVersion::new(12));
        ProjectionCursor::new(watermarks, ProjectionEpoch::new(41))
    }

    fn sample_cost() -> CostCursor {
        let mut frontiers = BTreeMap::new();
        frontiers.insert(ShardId::new(vec![1]), CostFrontier::new(10));
        frontiers.insert(ShardId::new(vec![2]), CostFrontier::new(20));
        CostCursor::new(frontiers, scope_vector(&[(&[1], 18), (&[2], 9)]), 5)
    }

    fn decode_for(cursor_type: CursorType, token: &[u8]) -> Result<(), CursorError> {
        match cursor_type {
            CursorType::Key => KeyCursor::from_token(token).map(|_| ()),
            CursorType::Sync => SyncCursor::from_token(token).map(|_| ()),
            CursorType::Hydration => HydrationCursor::from_token(token).map(|_| ()),
            CursorType::Receipt => ReceiptCursor::from_token(token).map(|_| ()),
            CursorType::Event => EventCursor::from_token(token).map(|_| ()),
            CursorType::Search => SearchCursor::from_token(token).map(|_| ()),
            CursorType::Projection => ProjectionCursor::from_token(token).map(|_| ()),
            CursorType::Cost => CostCursor::from_token(token).map(|_| ()),
        }
    }

    fn sample_tokens() -> [(CursorType, Vec<u8>); 8] {
        [
            (CursorType::Key, sample_key().to_token()),
            (CursorType::Sync, sample_sync().to_token()),
            (CursorType::Hydration, sample_hydration().to_token()),
            (CursorType::Receipt, sample_receipt().to_token()),
            (CursorType::Event, sample_event().to_token()),
            (CursorType::Search, sample_search().to_token()),
            (CursorType::Projection, sample_projection().to_token()),
            (CursorType::Cost, sample_cost().to_token()),
        ]
    }

    #[test]
    fn there_are_eight_cursor_types() {
        assert_eq!(CursorType::ALL.len(), 8);
        for (index, cursor_type) in CursorType::ALL.iter().enumerate() {
            assert_eq!(CursorType::from_tag(cursor_type.tag()), Some(*cursor_type));
            assert_eq!(usize::from(cursor_type.tag()), index + 1);
        }
        assert!(CursorType::from_tag(0).is_none());
        assert!(CursorType::from_tag(9).is_none());
    }

    #[test]
    fn every_cursor_type_names_its_version_fields() {
        for cursor_type in CursorType::ALL {
            assert_ne!(cursor_type.version_fields().len(), 0);
        }
    }

    #[test]
    fn every_cursor_type_round_trips() {
        assert_eq!(
            KeyCursor::from_token(&sample_key().to_token()),
            Ok(sample_key())
        );
        assert_eq!(
            SyncCursor::from_token(&sample_sync().to_token()),
            Ok(sample_sync())
        );
        assert_eq!(
            HydrationCursor::from_token(&sample_hydration().to_token()),
            Ok(sample_hydration())
        );
        assert_eq!(
            ReceiptCursor::from_token(&sample_receipt().to_token()),
            Ok(sample_receipt())
        );
        assert_eq!(
            EventCursor::from_token(&sample_event().to_token()),
            Ok(sample_event())
        );
        assert_eq!(
            SearchCursor::from_token(&sample_search().to_token()),
            Ok(sample_search())
        );
        assert_eq!(
            ProjectionCursor::from_token(&sample_projection().to_token()),
            Ok(sample_projection())
        );
        assert_eq!(
            CostCursor::from_token(&sample_cost().to_token()),
            Ok(sample_cost())
        );
    }

    #[test]
    fn a_token_from_one_type_fails_in_another_type() {
        for (source_type, token) in sample_tokens() {
            for target_type in CursorType::ALL {
                if *target_type == source_type {
                    continue;
                }
                assert_eq!(
                    decode_for(*target_type, &token),
                    Err(CursorError::TypeMismatch {
                        expected: *target_type,
                        found: source_type
                    }),
                    "{source_type} passed as {target_type}"
                );
            }
        }
    }

    #[test]
    fn a_tampered_token_fails() {
        let mut token = sample_key().to_token();
        let last = token.len() - 1;
        token[last] ^= 0x01;
        assert_eq!(KeyCursor::from_token(&token), Err(CursorError::Invalid));
        assert_eq!(
            KeyCursor::from_token(&token).unwrap_err().code(),
            "sync.cursor_invalid"
        );
    }

    #[test]
    fn a_truncated_token_fails() {
        let token = sample_key().to_token();
        for length in 0..token.len() {
            assert_eq!(
                KeyCursor::from_token(&token[..length]),
                Err(CursorError::Invalid)
            );
        }
        assert!(KeyCursor::from_token(&[]).is_err());
    }

    #[test]
    fn an_unknown_token_version_fails() {
        let mut token = sample_key().to_token();
        token[5..7].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(
            KeyCursor::from_token(&token),
            Err(CursorError::UnsupportedVersion {
                expected: 1,
                found: 2
            })
        );
    }

    #[test]
    fn every_type_has_its_own_expiry_rule_and_error() {
        assert_eq!(sample_key().expiry_rule(), CursorExpiry::SnapshotRetention);
        assert_eq!(sample_sync().expiry_rule(), CursorExpiry::DiffRetention);
        assert_eq!(
            sample_hydration().expiry_rule(),
            CursorExpiry::SnapshotRetention
        );
        assert_eq!(
            sample_receipt().expiry_rule(),
            CursorExpiry::ReceiptRetention
        );
        assert_eq!(sample_event().expiry_rule(), CursorExpiry::EventRetention);
        assert_eq!(sample_search().expiry_rule(), CursorExpiry::PinLifetime);
        assert_eq!(
            sample_projection().expiry_rule(),
            CursorExpiry::ProjectionRetention
        );
        assert_eq!(sample_cost().expiry_rule(), CursorExpiry::QueryBudget);

        assert_eq!(KeyCursor::expiry_error(), "sync.cursor_expired");
        assert_eq!(ReceiptCursor::expiry_error(), "receipt.expired");
        assert_eq!(EventCursor::expiry_error(), "event.retention_gap");
        assert_eq!(SearchCursor::expiry_error(), "search.cursor_invalid");
        assert_eq!(ProjectionCursor::expiry_error(), "projection.epoch_invalid");
        assert_eq!(CostCursor::expiry_error(), "query.budget_exceeded");
    }

    #[test]
    fn a_cursor_past_retention_is_not_readable() {
        assert!(sample_key().is_readable_at(SnapshotVersion::new(18)));
        assert!(!sample_key().is_readable_at(SnapshotVersion::new(19)));
        assert!(sample_sync().is_readable_at(&scope_vector(&[(&[1], 4)])));
        assert!(!sample_sync().is_readable_at(&scope_vector(&[(&[1], 19)])));
        assert!(sample_hydration().is_readable_at(SnapshotVersion::new(18)));
        assert!(!sample_hydration().is_readable_at(SnapshotVersion::new(19)));
        assert!(sample_receipt().is_readable_at(ReceiptSequence::new(5)));
        assert!(!sample_receipt().is_readable_at(ReceiptSequence::new(6)));
        assert!(
            sample_event().is_readable_at(&EventCursor::new(EventPosition::Compact(
                EventSequence::new(4)
            )))
        );
        assert!(
            !sample_event().is_readable_at(&EventCursor::new(EventPosition::Compact(
                EventSequence::new(5)
            )))
        );
        assert!(sample_search().is_readable_at(&scope_vector(&[(&[1], 18)])));
        assert!(!sample_search().is_readable_at(&scope_vector(&[(&[1], 19)])));
        assert!(sample_projection().is_readable_at(ProjectionEpoch::new(41)));
        assert!(!sample_projection().is_readable_at(ProjectionEpoch::new(42)));
        assert!(sample_cost().has_budget());
        assert!(!CostCursor::new(BTreeMap::new(), ScopeVersionVector::new(), 0).has_budget());
    }
}
