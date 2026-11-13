//! Qualified version axes and the provenance block.
//!
//! The 1.0 contract uses qualified version names. A bare "version" is not a
//! public name. [`VersionAxis`] lists every axis and its group.
//!
//! [`ContentDigest`] covers artifact bytes only. [`ContentSeal`] covers the
//! content digest and the authority [`LeaseTerm`]. The content digest has no
//! field for a lease term.

use core::fmt;
use core::str::FromStr;
use std::collections::BTreeMap;

/// The group of one version axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VersionGroup {
    /// A build or package axis.
    Product,
    /// A frozen contract or annex axis.
    Contract,
    /// A data axis.
    Data,
    /// A content identity axis.
    Identity,
    /// A fencing axis.
    Fence,
    /// A client session or replica axis.
    Session,
}

impl VersionGroup {
    /// Returns the lowercase name of the group.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Contract => "contract",
            Self::Data => "data",
            Self::Identity => "identity",
            Self::Fence => "fence",
            Self::Session => "session",
        }
    }
}

impl fmt::Display for VersionGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One qualified version axis from the 1.0 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VersionAxis {
    /// The crate version.
    CrateVersion,
    /// The key encoding annex version.
    KeyEncodingVersion,
    /// The snapshot and page contract version.
    SnapshotPageVersion,
    /// The sync wire contract version.
    SyncWireVersion,
    /// The manifest schema version.
    ManifestSchemaVersion,
    /// The policy AST version.
    PolicyAstVersion,
    /// The receipt and error contract version.
    ReceiptErrorVersion,
    /// The RPC descriptor version.
    DescriptorVersion,
    /// The shard map schema version.
    MapSchemaVersion,
    /// The tokenizer annex version.
    TokenizerVersion,
    /// The event envelope version.
    EventVersion,
    /// The audit envelope version.
    AuditVersion,
    /// The scope annex version.
    ScopeVersion,
    /// The projection annex version.
    ProjectionVersion,
    /// The cost and explain annex version.
    CostVersion,
    /// The shard commit version.
    CommitVersion,
    /// The version of one key record.
    KeyVersion,
    /// The version of one shard snapshot.
    SnapshotVersion,
    /// The scope version vector.
    ScopeVersionVector,
    /// The projection epoch.
    ProjectionEpoch,
    /// The version of one fact.
    FactVersion,
    /// The search statistics version.
    StatisticsVersion,
    /// The shard map version.
    MapVersion,
    /// The manifest revision.
    ManifestRevision,
    /// The manifest digest.
    ManifestDigest,
    /// The policy version.
    PolicyVersion,
    /// The shard map digest.
    MapDigest,
    /// The RPC descriptor digest.
    DescriptorDigest,
    /// The authority lease term.
    LeaseTerm,
    /// The session version vector minimum.
    SessionMinimum,
    /// The replica high-water version vector.
    ReplicaHighWater,
}

impl VersionAxis {
    /// Every version axis in the 1.0 contract.
    pub const ALL: &'static [Self] = &[
        Self::CrateVersion,
        Self::KeyEncodingVersion,
        Self::SnapshotPageVersion,
        Self::SyncWireVersion,
        Self::ManifestSchemaVersion,
        Self::PolicyAstVersion,
        Self::ReceiptErrorVersion,
        Self::DescriptorVersion,
        Self::MapSchemaVersion,
        Self::TokenizerVersion,
        Self::EventVersion,
        Self::AuditVersion,
        Self::ScopeVersion,
        Self::ProjectionVersion,
        Self::CostVersion,
        Self::CommitVersion,
        Self::KeyVersion,
        Self::SnapshotVersion,
        Self::ScopeVersionVector,
        Self::ProjectionEpoch,
        Self::FactVersion,
        Self::StatisticsVersion,
        Self::MapVersion,
        Self::ManifestRevision,
        Self::ManifestDigest,
        Self::PolicyVersion,
        Self::MapDigest,
        Self::DescriptorDigest,
        Self::LeaseTerm,
        Self::SessionMinimum,
        Self::ReplicaHighWater,
    ];

    /// Returns the qualified name of the axis.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CrateVersion => "crate_version",
            Self::KeyEncodingVersion => "key_encoding_version",
            Self::SnapshotPageVersion => "snapshot_page_version",
            Self::SyncWireVersion => "sync_wire_version",
            Self::ManifestSchemaVersion => "manifest_schema_version",
            Self::PolicyAstVersion => "policy_ast_version",
            Self::ReceiptErrorVersion => "receipt_error_version",
            Self::DescriptorVersion => "descriptor_version",
            Self::MapSchemaVersion => "map_schema_version",
            Self::TokenizerVersion => "tokenizer_version",
            Self::EventVersion => "event_version",
            Self::AuditVersion => "audit_version",
            Self::ScopeVersion => "scope_version",
            Self::ProjectionVersion => "projection_version",
            Self::CostVersion => "cost_version",
            Self::CommitVersion => "commit_version",
            Self::KeyVersion => "key_version",
            Self::SnapshotVersion => "snapshot_version",
            Self::ScopeVersionVector => "scope_version_vector",
            Self::ProjectionEpoch => "projection_epoch",
            Self::FactVersion => "fact_version",
            Self::StatisticsVersion => "statistics_version",
            Self::MapVersion => "map_version",
            Self::ManifestRevision => "manifest_revision",
            Self::ManifestDigest => "manifest_digest",
            Self::PolicyVersion => "policy_version",
            Self::MapDigest => "map_digest",
            Self::DescriptorDigest => "descriptor_digest",
            Self::LeaseTerm => "lease_term",
            Self::SessionMinimum => "session_minimum",
            Self::ReplicaHighWater => "replica_high_water",
        }
    }

    /// Returns the group of the axis.
    #[must_use]
    pub const fn group(self) -> VersionGroup {
        match self {
            Self::CrateVersion => VersionGroup::Product,
            Self::KeyEncodingVersion
            | Self::SnapshotPageVersion
            | Self::SyncWireVersion
            | Self::ManifestSchemaVersion
            | Self::PolicyAstVersion
            | Self::ReceiptErrorVersion
            | Self::DescriptorVersion
            | Self::MapSchemaVersion
            | Self::TokenizerVersion
            | Self::EventVersion
            | Self::AuditVersion
            | Self::ScopeVersion
            | Self::ProjectionVersion
            | Self::CostVersion => VersionGroup::Contract,
            Self::CommitVersion
            | Self::KeyVersion
            | Self::SnapshotVersion
            | Self::ScopeVersionVector
            | Self::ProjectionEpoch
            | Self::FactVersion
            | Self::StatisticsVersion
            | Self::MapVersion => VersionGroup::Data,
            Self::ManifestRevision
            | Self::ManifestDigest
            | Self::PolicyVersion
            | Self::MapDigest
            | Self::DescriptorDigest => VersionGroup::Identity,
            Self::LeaseTerm => VersionGroup::Fence,
            Self::SessionMinimum | Self::ReplicaHighWater => VersionGroup::Session,
        }
    }
}

impl fmt::Display for VersionAxis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// An error from qualified version text parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VersionTextError {
    /// The text does not match the version shape.
    #[error("The version text has an invalid format.")]
    InvalidFormat,
    /// A numeric part is out of range.
    #[error("The version number is out of range.")]
    NumberOutOfRange,
}

/// A 32-byte hash value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash32([u8; 32]);

/// The digest domain separates one hash use from another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DigestDomain {
    /// Artifact content bytes.
    Content,
    /// Shard snapshot bytes.
    Snapshot,
    /// Page bytes.
    Page,
    /// Canonical manifest bytes.
    Manifest,
    /// Canonical policy AST bytes.
    Policy,
    /// Canonical shard map bytes.
    Map,
    /// Canonical descriptor bytes.
    Descriptor,
    /// Canonical scope declaration bytes.
    Scope,
    /// An audit record body and previous hash.
    AuditRecord,
    /// A business event identity input.
    EventId,
    /// A receipt identity input.
    ReceiptId,
    /// A cursor token header and payload.
    CursorToken,
}

impl DigestDomain {
    /// Returns the domain separation tag.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Content => "oxkv/content/v1",
            Self::Snapshot => "oxkv/snapshot/v1",
            Self::Page => "oxkv/page/v1",
            Self::Manifest => "oxkv/manifest/v1",
            Self::Policy => "oxkv/policy/v1",
            Self::Map => "oxkv/map/v1",
            Self::Descriptor => "oxkv/descriptor/v1",
            Self::Scope => "oxkv/scope/v1",
            Self::AuditRecord => "oxkv/audit-record/v1",
            Self::EventId => "oxkv/event-id/v1",
            Self::ReceiptId => "oxkv/receipt-id/v1",
            Self::CursorToken => "oxkv/cursor-token/v1",
        }
    }
}

impl Hash32 {
    /// Computes a domain-separated `BLAKE3` hash.
    #[must_use]
    pub fn of(domain: DigestDomain, bytes: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(domain.tag().as_bytes());
        hasher.update(&[0]);
        hasher.update(bytes);
        Self(*hasher.finalize().as_bytes())
    }

    /// Creates a hash from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw hash bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(64);
        for byte in self.0 {
            text.push(char::from(HEX[usize::from(byte >> 4)]));
            text.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        text
    }
}

impl fmt::Display for Hash32 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// A digest of artifact content bytes.
///
/// The type has no field for a lease term. [`ContentDigest::of_bytes`] accepts
/// artifact bytes only. [`ContentSeal`] adds the lease term.
///
/// A lease term cannot enter the digest through this API:
///
/// ```compile_fail
/// # use oxkv_contracts::version::{ContentDigest, LeaseTerm};
/// let _ = ContentDigest::of_lease_term(LeaseTerm::new(9));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentDigest(Hash32);

impl ContentDigest {
    /// Computes the content digest of artifact bytes.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(Hash32::of(DigestDomain::Content, bytes))
    }

    /// Creates a content digest from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Hash32::from_bytes(bytes))
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Returns the lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// An opaque `Ed25519` signature value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature([u8; 64]);

impl Signature {
    /// Creates a signature from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// Returns the raw signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

/// A content seal.
///
/// The seal covers the content digest and the authority lease term. The
/// content digest never covers the lease term. A sealed artifact stays valid
/// when the lease term changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentSeal {
    content_digest: ContentDigest,
    lease_term: LeaseTerm,
    signature: Option<Signature>,
}

impl ContentSeal {
    /// Creates a content seal.
    #[must_use]
    pub const fn new(
        content_digest: ContentDigest,
        lease_term: LeaseTerm,
        signature: Option<Signature>,
    ) -> Self {
        Self {
            content_digest,
            lease_term,
            signature,
        }
    }

    /// Returns the sealed content digest.
    #[must_use]
    pub const fn content_digest(self) -> ContentDigest {
        self.content_digest
    }

    /// Returns the lease term that the seal covers.
    #[must_use]
    pub const fn lease_term(self) -> LeaseTerm {
        self.lease_term
    }

    /// Returns the signature value.
    #[must_use]
    pub const fn signature(self) -> Option<Signature> {
        self.signature
    }

    /// Returns true when the seal covers the given content digest.
    #[must_use]
    pub fn covers(self, content_digest: ContentDigest) -> bool {
        self.content_digest == content_digest
    }
}

macro_rules! integer_version {
    ($(#[$meta:meta])* $name:ident, $axis:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u64);

        impl $name {
            /// The version axis for this type.
            pub const AXIS: VersionAxis = VersionAxis::$axis;

            /// Creates the version value.
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

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }
    };
}

integer_version!(
    /// The shard commit version.
    CommitVersion,
    CommitVersion
);
integer_version!(
    /// The version of one key record.
    KeyVersion,
    KeyVersion
);
integer_version!(
    /// The version of one shard snapshot.
    SnapshotVersion,
    SnapshotVersion
);
integer_version!(
    /// The projection epoch.
    ProjectionEpoch,
    ProjectionEpoch
);
integer_version!(
    /// The version of one fact.
    FactVersion,
    FactVersion
);
integer_version!(
    /// The search statistics version.
    StatisticsVersion,
    StatisticsVersion
);
integer_version!(
    /// The shard map version.
    MapVersion,
    MapVersion
);
integer_version!(
    /// The manifest revision.
    ManifestRevision,
    ManifestRevision
);
integer_version!(
    /// The authority lease term.
    LeaseTerm,
    LeaseTerm
);

/// A page sequence inside one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageSequence(u64);

impl PageSequence {
    /// Creates a page sequence value.
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

impl fmt::Display for PageSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

macro_rules! prefixed_version {
    ($(#[$meta:meta])* $name:ident, $axis:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u16);

        impl $name {
            /// The version axis for this type.
            pub const AXIS: VersionAxis = VersionAxis::$axis;

            /// The text prefix of the version.
            pub const PREFIX: &'static str = $prefix;

            /// Creates the version value.
            #[must_use]
            pub const fn new(revision: u16) -> Self {
                Self(revision)
            }

            /// Returns the revision number.
            #[must_use]
            pub const fn revision(self) -> u16 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}/v{}", $prefix, self.0)
            }
        }

        impl FromStr for $name {
            type Err = VersionTextError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                let prefix = concat!($prefix, "/v");
                let revision = text
                    .strip_prefix(prefix)
                    .ok_or(VersionTextError::InvalidFormat)?;
                let revision = revision
                    .parse::<u16>()
                    .map_err(|_| VersionTextError::NumberOutOfRange)?;
                Ok(Self(revision))
            }
        }
    };
}

prefixed_version!(
    /// The manifest schema version.
    ManifestSchemaVersion,
    ManifestSchemaVersion,
    "oxkv/manifest"
);
prefixed_version!(
    /// The policy AST version.
    PolicyAstVersion,
    PolicyAstVersion,
    "policy-ast"
);
prefixed_version!(
    /// The receipt and error contract version.
    ReceiptErrorVersion,
    ReceiptErrorVersion,
    "receipt-error"
);
prefixed_version!(
    /// The RPC descriptor version.
    DescriptorVersion,
    DescriptorVersion,
    "descriptor"
);
prefixed_version!(
    /// The shard map schema version.
    MapSchemaVersion,
    MapSchemaVersion,
    "oxkv/map"
);
prefixed_version!(
    /// The tokenizer annex version.
    TokenizerVersion,
    TokenizerVersion,
    "tokenizer"
);
prefixed_version!(
    /// The event envelope version.
    EventVersion,
    EventVersion,
    "oxkv/event"
);
prefixed_version!(
    /// The audit envelope version.
    AuditVersion,
    AuditVersion,
    "audit"
);
prefixed_version!(
    /// The scope annex version.
    ScopeVersion,
    ScopeVersion,
    "scope"
);
prefixed_version!(
    /// The projection annex version.
    ProjectionVersion,
    ProjectionVersion,
    "projection"
);
prefixed_version!(
    /// The cost and explain annex version.
    CostVersion,
    CostVersion,
    "cost"
);

/// The key encoding annex version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyEncodingVersion(u16);

impl KeyEncodingVersion {
    /// The version axis for this type.
    pub const AXIS: VersionAxis = VersionAxis::KeyEncodingVersion;

    /// Creates the key encoding version.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for KeyEncodingVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// The sync wire contract version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SyncWireVersion(u16);

impl SyncWireVersion {
    /// The version axis for this type.
    pub const AXIS: VersionAxis = VersionAxis::SyncWireVersion;

    /// Creates the sync wire version.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for SyncWireVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// A major and minor contract version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapshotPageVersion {
    major: u16,
    minor: u16,
}

impl SnapshotPageVersion {
    /// The version axis for this type.
    pub const AXIS: VersionAxis = VersionAxis::SnapshotPageVersion;

    /// Creates a major and minor version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Returns the major part.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the minor part.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }
}

impl fmt::Display for SnapshotPageVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

impl FromStr for SnapshotPageVersion {
    type Err = VersionTextError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts = text.split('.');
        let major = parse_u16(parts.next())?;
        let minor = parse_u16(parts.next())?;
        if parts.next().is_some() {
            return Err(VersionTextError::InvalidFormat);
        }
        Ok(Self::new(major, minor))
    }
}

/// The crate version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CrateVersion {
    major: u16,
    minor: u16,
    patch: u16,
}

impl CrateVersion {
    /// The version axis for this type.
    pub const AXIS: VersionAxis = VersionAxis::CrateVersion;

    /// Creates a crate version.
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Returns the major part.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the minor part.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Returns the patch part.
    #[must_use]
    pub const fn patch(self) -> u16 {
        self.patch
    }
}

impl fmt::Display for CrateVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for CrateVersion {
    type Err = VersionTextError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts = text.split('.');
        let major = parse_u16(parts.next())?;
        let minor = parse_u16(parts.next())?;
        let patch = parse_u16(parts.next())?;
        if parts.next().is_some() {
            return Err(VersionTextError::InvalidFormat);
        }
        Ok(Self::new(major, minor, patch))
    }
}

fn parse_u16(part: Option<&str>) -> Result<u16, VersionTextError> {
    part.ok_or(VersionTextError::InvalidFormat)?
        .parse::<u16>()
        .map_err(|_| VersionTextError::NumberOutOfRange)
}

macro_rules! hash_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Hash32);

        impl $name {
            /// Creates the value from raw hash bytes.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(Hash32::from_bytes(bytes))
            }

            /// Returns the raw hash bytes.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                self.0.as_bytes()
            }

            /// Returns the lowercase hexadecimal form.
            #[must_use]
            pub fn to_hex(self) -> String {
                self.0.to_hex()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.to_hex())
                    .finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.to_hex())
            }
        }
    };
}

hash_newtype!(
    /// The manifest digest.
    ManifestDigest
);
hash_newtype!(
    /// The policy version. It is a content address.
    PolicyVersion
);
hash_newtype!(
    /// The shard map digest.
    MapDigest
);
hash_newtype!(
    /// The RPC descriptor digest.
    DescriptorDigest
);
hash_newtype!(
    /// The canonical scope identity.
    ScopeIdentity
);
hash_newtype!(
    /// The deterministic page identity.
    PageId
);

/// An opaque and stable shard identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShardId(Vec<u8>);

impl ShardId {
    /// Creates a shard identifier.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Returns the raw identifier bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for ShardId {
    fn from(bytes: Vec<u8>) -> Self {
        Self::new(bytes)
    }
}

/// An inclusive lower bound and an exclusive upper bound.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrefixBounds {
    lower: Vec<u8>,
    upper: Vec<u8>,
}

impl PrefixBounds {
    /// Creates a prefix bound pair.
    #[must_use]
    pub fn new(lower: Vec<u8>, upper: Vec<u8>) -> Self {
        Self { lower, upper }
    }

    /// Returns the inclusive lower bound.
    #[must_use]
    pub fn lower(&self) -> &[u8] {
        &self.lower
    }

    /// Returns the exclusive upper bound.
    #[must_use]
    pub fn upper(&self) -> &[u8] {
        &self.upper
    }

    /// Returns true when the lower bound is not less than the upper bound.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lower >= self.upper
    }
}

/// A version vector with one commit version per shard.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct VersionVector {
    entries: BTreeMap<ShardId, CommitVersion>,
}

impl VersionVector {
    /// Creates an empty version vector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a version vector from entries.
    #[must_use]
    pub fn from_entries(entries: impl IntoIterator<Item = (ShardId, CommitVersion)>) -> Self {
        let mut vector = Self::new();
        for (shard_id, commit_version) in entries {
            vector.set(shard_id, commit_version);
        }
        vector
    }

    /// Sets one shard entry.
    pub fn set(&mut self, shard_id: ShardId, commit_version: CommitVersion) {
        self.entries.insert(shard_id, commit_version);
    }

    /// Returns one shard entry.
    #[must_use]
    pub fn get(&self, shard_id: &ShardId) -> Option<CommitVersion> {
        self.entries.get(shard_id).copied()
    }

    /// Returns the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the vector has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates over the entries in canonical shard order.
    pub fn iter(&self) -> impl Iterator<Item = (&ShardId, CommitVersion)> {
        self.entries
            .iter()
            .map(|(shard_id, commit_version)| (shard_id, *commit_version))
    }

    /// Returns true when this vector is at or above every entry of another.
    #[must_use]
    pub fn entails(&self, other: &Self) -> bool {
        other.iter().all(|(shard_id, other_version)| {
            self.get(shard_id)
                .is_some_and(|version| version >= other_version)
        })
    }
}

macro_rules! vector_newtype {
    ($(#[$meta:meta])* $name:ident, $axis:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
        pub struct $name(VersionVector);

        impl $name {
            /// The version axis for this type.
            pub const AXIS: VersionAxis = VersionAxis::$axis;

            /// Creates an empty vector.
            #[must_use]
            pub fn new() -> Self {
                Self(VersionVector::new())
            }

            /// Creates the value from a version vector.
            #[must_use]
            pub const fn from_vector(vector: VersionVector) -> Self {
                Self(vector)
            }

            /// Returns the inner version vector.
            #[must_use]
            pub const fn as_vector(&self) -> &VersionVector {
                &self.0
            }

            /// Returns one shard entry.
            #[must_use]
            pub fn get(&self, shard_id: &ShardId) -> Option<CommitVersion> {
                self.0.get(shard_id)
            }

            /// Returns true when the vector has no entries.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }
    };
}

vector_newtype!(
    /// The scope version vector.
    ScopeVersionVector,
    ScopeVersionVector
);
vector_newtype!(
    /// The session version vector minimum.
    SessionMinimum,
    SessionMinimum
);
vector_newtype!(
    /// The replica high-water version vector.
    ReplicaHighWater,
    ReplicaHighWater
);

/// A fact source identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FactSourceId(String);

impl FactSourceId {
    /// Creates a fact source identifier.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One tokenizer version per field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TokenizerVersions(BTreeMap<String, TokenizerVersion>);

impl TokenizerVersions {
    /// Creates an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets one field tokenizer version.
    pub fn set(&mut self, field: impl Into<String>, tokenizer_version: TokenizerVersion) {
        self.0.insert(field.into(), tokenizer_version);
    }

    /// Returns one field tokenizer version.
    #[must_use]
    pub fn get(&self, field: &str) -> Option<TokenizerVersion> {
        self.0.get(field).copied()
    }

    /// Iterates over the entries in canonical field order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, TokenizerVersion)> {
        self.0
            .iter()
            .map(|(field, version)| (field.as_str(), *version))
    }

    /// Returns true when the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One projection watermark per fact source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ProjectionWatermarks(BTreeMap<FactSourceId, FactVersion>);

impl ProjectionWatermarks {
    /// Creates an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets one fact source watermark.
    pub fn set(&mut self, source: FactSourceId, fact_version: FactVersion) {
        self.0.insert(source, fact_version);
    }

    /// Returns one fact source watermark.
    #[must_use]
    pub fn get(&self, source: &FactSourceId) -> Option<FactVersion> {
        self.0.get(source).copied()
    }

    /// Iterates over the entries in canonical source order.
    pub fn iter(&self) -> impl Iterator<Item = (&FactSourceId, FactVersion)> {
        self.0.iter().map(|(source, version)| (source, *version))
    }

    /// Returns true when the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The identity of one shard snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SnapshotIdentity {
    /// The shard that owns the snapshot.
    pub shard_id: ShardId,
    /// The commit version of the snapshot.
    pub commit_version: CommitVersion,
    /// The content digest of the snapshot.
    pub content_digest: ContentDigest,
}

impl SnapshotIdentity {
    /// Creates a snapshot identity.
    #[must_use]
    pub const fn new(
        shard_id: ShardId,
        commit_version: CommitVersion,
        content_digest: ContentDigest,
    ) -> Self {
        Self {
            shard_id,
            commit_version,
            content_digest,
        }
    }
}

/// One field in the provenance block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProvenanceField {
    /// The snapshot and page contract version.
    FormatVersion,
    /// The key encoding annex version.
    KeyEncodingVersion,
    /// The manifest digest.
    ManifestDigest,
    /// The shard map version.
    MapVersion,
    /// The shard map digest.
    MapDigest,
    /// The policy version.
    PolicyVersion,
    /// The shard identifier.
    ShardId,
    /// The shard prefix bounds.
    PrefixBounds,
    /// The shard commit version.
    CommitVersion,
    /// The scope version vector.
    VersionVector,
    /// The canonical scope identity.
    ScopeIdentity,
    /// The tokenizer versions per field.
    TokenizerVersions,
    /// The search statistics version.
    StatisticsVersion,
    /// The projection epoch.
    ProjectionEpoch,
    /// The projection watermarks per fact source.
    ProjectionWatermarks,
    /// The content digest.
    ContentDigest,
    /// The content seal.
    ContentSeal,
    /// The deterministic page identity.
    PageId,
    /// The page sequence.
    PageSequence,
}

impl ProvenanceField {
    /// Every provenance field.
    pub const ALL: &'static [Self] = &[
        Self::FormatVersion,
        Self::KeyEncodingVersion,
        Self::ManifestDigest,
        Self::MapVersion,
        Self::MapDigest,
        Self::PolicyVersion,
        Self::ShardId,
        Self::PrefixBounds,
        Self::CommitVersion,
        Self::VersionVector,
        Self::ScopeIdentity,
        Self::TokenizerVersions,
        Self::StatisticsVersion,
        Self::ProjectionEpoch,
        Self::ProjectionWatermarks,
        Self::ContentDigest,
        Self::ContentSeal,
        Self::PageId,
        Self::PageSequence,
    ];

    /// Returns the qualified field name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::FormatVersion => "format_version",
            Self::KeyEncodingVersion => "key_encoding_version",
            Self::ManifestDigest => "manifest_digest",
            Self::MapVersion => "map_version",
            Self::MapDigest => "map_digest",
            Self::PolicyVersion => "policy_version",
            Self::ShardId => "shard_id",
            Self::PrefixBounds => "prefix_bounds",
            Self::CommitVersion => "commit_version",
            Self::VersionVector => "version_vector",
            Self::ScopeIdentity => "scope_identity",
            Self::TokenizerVersions => "tokenizer_versions",
            Self::StatisticsVersion => "statistics_version",
            Self::ProjectionEpoch => "projection_epoch",
            Self::ProjectionWatermarks => "projection_watermarks",
            Self::ContentDigest => "content_digest",
            Self::ContentSeal => "content_seal",
            Self::PageId => "page_id",
            Self::PageSequence => "page_sequence",
        }
    }
}

impl fmt::Display for ProvenanceField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The artifact kinds that carry a provenance block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArtifactKind {
    /// A shard snapshot.
    ShardSnapshot,
    /// A snapshot page.
    Page,
    /// A canonical manifest.
    Manifest,
    /// A canonical shard map.
    ShardMap,
    /// A canonical policy AST.
    Policy,
    /// A scope snapshot.
    ScopeSnapshot,
    /// A read pin.
    ReadPin,
    /// A cursor token.
    Cursor,
    /// A write enforcement verdict.
    Verdict,
    /// A business event.
    Event,
    /// A search index artifact.
    Index,
    /// A search result artifact.
    SearchArtifact,
}

const SNAPSHOT_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::FormatVersion,
    ProvenanceField::KeyEncodingVersion,
    ProvenanceField::ManifestDigest,
    ProvenanceField::ShardId,
    ProvenanceField::PrefixBounds,
    ProvenanceField::CommitVersion,
    ProvenanceField::ContentDigest,
    ProvenanceField::ContentSeal,
];

const PAGE_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::FormatVersion,
    ProvenanceField::KeyEncodingVersion,
    ProvenanceField::ManifestDigest,
    ProvenanceField::ShardId,
    ProvenanceField::PrefixBounds,
    ProvenanceField::CommitVersion,
    ProvenanceField::ContentDigest,
    ProvenanceField::ContentSeal,
    ProvenanceField::PageId,
    ProvenanceField::PageSequence,
];

const MANIFEST_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::ContentDigest,
];

const MAP_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::MapVersion,
    ProvenanceField::MapDigest,
    ProvenanceField::ContentDigest,
];

const POLICY_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::PolicyVersion,
    ProvenanceField::ContentDigest,
];

const SCOPE_SNAPSHOT_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::PolicyVersion,
    ProvenanceField::VersionVector,
    ProvenanceField::ScopeIdentity,
    ProvenanceField::ProjectionEpoch,
    ProvenanceField::ProjectionWatermarks,
    ProvenanceField::ContentDigest,
];

const READ_PIN_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::VersionVector,
    ProvenanceField::ScopeIdentity,
];

const CURSOR_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::KeyEncodingVersion,
    ProvenanceField::ScopeIdentity,
];

const VERDICT_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::PolicyVersion,
    ProvenanceField::ProjectionEpoch,
    ProvenanceField::ProjectionWatermarks,
];

const EVENT_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::PolicyVersion,
];

const INDEX_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::TokenizerVersions,
];

const SEARCH_FIELDS: &[ProvenanceField] = &[
    ProvenanceField::ManifestDigest,
    ProvenanceField::TokenizerVersions,
];

impl ArtifactKind {
    /// Every artifact kind.
    pub const ALL: &'static [Self] = &[
        Self::ShardSnapshot,
        Self::Page,
        Self::Manifest,
        Self::ShardMap,
        Self::Policy,
        Self::ScopeSnapshot,
        Self::ReadPin,
        Self::Cursor,
        Self::Verdict,
        Self::Event,
        Self::Index,
        Self::SearchArtifact,
    ];

    /// Returns the canonical artifact name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ShardSnapshot => "ShardSnapshot",
            Self::Page => "Page",
            Self::Manifest => "Manifest",
            Self::ShardMap => "ShardMap",
            Self::Policy => "Policy",
            Self::ScopeSnapshot => "ScopeSnapshot",
            Self::ReadPin => "ReadPin",
            Self::Cursor => "Cursor",
            Self::Verdict => "Verdict",
            Self::Event => "Event",
            Self::Index => "Index",
            Self::SearchArtifact => "SearchArtifact",
        }
    }

    /// Returns the required provenance fields for this artifact.
    #[must_use]
    pub const fn required_fields(self) -> &'static [ProvenanceField] {
        match self {
            Self::ShardSnapshot => SNAPSHOT_FIELDS,
            Self::Page => PAGE_FIELDS,
            Self::Manifest => MANIFEST_FIELDS,
            Self::ShardMap => MAP_FIELDS,
            Self::Policy => POLICY_FIELDS,
            Self::ScopeSnapshot => SCOPE_SNAPSHOT_FIELDS,
            Self::ReadPin => READ_PIN_FIELDS,
            Self::Cursor => CURSOR_FIELDS,
            Self::Verdict => VERDICT_FIELDS,
            Self::Event => EVENT_FIELDS,
            Self::Index => INDEX_FIELDS,
            Self::SearchArtifact => SEARCH_FIELDS,
        }
    }

    /// Returns true when the artifact is a routing artifact.
    #[must_use]
    pub const fn is_routing(self) -> bool {
        matches!(self, Self::ShardMap)
    }

    /// Returns true when the artifact is policy filtered.
    #[must_use]
    pub const fn is_policy_filtered(self) -> bool {
        matches!(self, Self::ScopeSnapshot | Self::Verdict)
    }
}

/// An error from provenance validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProvenanceError {
    /// A required provenance field is absent.
    #[error("The artifact is missing the required provenance field {field}.")]
    MissingField {
        /// The missing field.
        field: ProvenanceField,
    },
}

impl ProvenanceError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingField { .. } => "format.missing_provenance",
        }
    }
}

/// The provenance block of one artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    /// The snapshot and page contract version.
    pub format_version: Option<SnapshotPageVersion>,
    /// The key encoding annex version.
    pub key_encoding_version: Option<KeyEncodingVersion>,
    /// The manifest digest.
    pub manifest_digest: Option<ManifestDigest>,
    /// The shard map version.
    pub map_version: Option<MapVersion>,
    /// The shard map digest.
    pub map_digest: Option<MapDigest>,
    /// The policy version.
    pub policy_version: Option<PolicyVersion>,
    /// The shard identifier.
    pub shard_id: Option<ShardId>,
    /// The shard prefix bounds.
    pub prefix_bounds: Option<PrefixBounds>,
    /// The shard commit version.
    pub commit_version: Option<CommitVersion>,
    /// The scope version vector.
    pub version_vector: Option<ScopeVersionVector>,
    /// The canonical scope identity.
    pub scope_identity: Option<ScopeIdentity>,
    /// The tokenizer versions per field.
    pub tokenizer_versions: Option<TokenizerVersions>,
    /// The search statistics version.
    pub statistics_version: Option<StatisticsVersion>,
    /// The projection epoch.
    pub projection_epoch: Option<ProjectionEpoch>,
    /// The projection watermarks per fact source.
    pub projection_watermarks: Option<ProjectionWatermarks>,
    /// The content digest.
    pub content_digest: Option<ContentDigest>,
    /// The content seal.
    pub content_seal: Option<ContentSeal>,
    /// The deterministic page identity.
    pub page_id: Option<PageId>,
    /// The page sequence.
    pub page_sequence: Option<PageSequence>,
}

impl Provenance {
    /// Creates an empty provenance block.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true when the field is present.
    #[must_use]
    pub fn has(&self, field: ProvenanceField) -> bool {
        match field {
            ProvenanceField::FormatVersion => self.format_version.is_some(),
            ProvenanceField::KeyEncodingVersion => self.key_encoding_version.is_some(),
            ProvenanceField::ManifestDigest => self.manifest_digest.is_some(),
            ProvenanceField::MapVersion => self.map_version.is_some(),
            ProvenanceField::MapDigest => self.map_digest.is_some(),
            ProvenanceField::PolicyVersion => self.policy_version.is_some(),
            ProvenanceField::ShardId => self.shard_id.is_some(),
            ProvenanceField::PrefixBounds => self.prefix_bounds.is_some(),
            ProvenanceField::CommitVersion => self.commit_version.is_some(),
            ProvenanceField::VersionVector => self.version_vector.is_some(),
            ProvenanceField::ScopeIdentity => self.scope_identity.is_some(),
            ProvenanceField::TokenizerVersions => self.tokenizer_versions.is_some(),
            ProvenanceField::StatisticsVersion => self.statistics_version.is_some(),
            ProvenanceField::ProjectionEpoch => self.projection_epoch.is_some(),
            ProvenanceField::ProjectionWatermarks => self.projection_watermarks.is_some(),
            ProvenanceField::ContentDigest => self.content_digest.is_some(),
            ProvenanceField::ContentSeal => self.content_seal.is_some(),
            ProvenanceField::PageId => self.page_id.is_some(),
            ProvenanceField::PageSequence => self.page_sequence.is_some(),
        }
    }

    /// Returns every required field that is absent.
    #[must_use]
    pub fn missing(&self, kind: ArtifactKind) -> Vec<ProvenanceField> {
        kind.required_fields()
            .iter()
            .copied()
            .filter(|field| !self.has(*field))
            .collect()
    }

    /// Checks the required field subset for one artifact.
    ///
    /// # Errors
    ///
    /// Returns [`ProvenanceError::MissingField`] when a required field is
    /// absent.
    pub fn validate(&self, kind: ArtifactKind) -> Result<(), ProvenanceError> {
        if let Some(field) = self.missing(kind).first() {
            return Err(ProvenanceError::MissingField { field: *field });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_provenance() -> Provenance {
        let mut vector = ScopeVersionVector::new();
        vector.0.set(ShardId::new(vec![1]), CommitVersion::new(7));
        let mut tokenizers = TokenizerVersions::new();
        tokenizers.set("title", TokenizerVersion::new(1));
        let mut watermarks = ProjectionWatermarks::new();
        watermarks.set(FactSourceId::new("accounts"), FactVersion::new(3));
        let digest = ContentDigest::from_bytes([2; 32]);
        Provenance {
            format_version: Some(SnapshotPageVersion::new(1, 0)),
            key_encoding_version: Some(KeyEncodingVersion::new(1)),
            manifest_digest: Some(ManifestDigest::from_bytes([1; 32])),
            map_version: Some(MapVersion::new(4)),
            map_digest: Some(MapDigest::from_bytes([3; 32])),
            policy_version: Some(PolicyVersion::from_bytes([4; 32])),
            shard_id: Some(ShardId::new(vec![1, 2])),
            prefix_bounds: Some(PrefixBounds::new(vec![1], vec![2])),
            commit_version: Some(CommitVersion::new(9)),
            version_vector: Some(vector),
            scope_identity: Some(ScopeIdentity::from_bytes([5; 32])),
            tokenizer_versions: Some(tokenizers),
            statistics_version: Some(StatisticsVersion::new(2)),
            projection_epoch: Some(ProjectionEpoch::new(6)),
            projection_watermarks: Some(watermarks),
            content_digest: Some(digest),
            content_seal: Some(ContentSeal::new(digest, LeaseTerm::new(3), None)),
            page_id: Some(PageId::from_bytes([6; 32])),
            page_sequence: Some(PageSequence::new(1)),
        }
    }

    fn clear_field(provenance: &mut Provenance, field: ProvenanceField) {
        match field {
            ProvenanceField::FormatVersion => provenance.format_version = None,
            ProvenanceField::KeyEncodingVersion => provenance.key_encoding_version = None,
            ProvenanceField::ManifestDigest => provenance.manifest_digest = None,
            ProvenanceField::MapVersion => provenance.map_version = None,
            ProvenanceField::MapDigest => provenance.map_digest = None,
            ProvenanceField::PolicyVersion => provenance.policy_version = None,
            ProvenanceField::ShardId => provenance.shard_id = None,
            ProvenanceField::PrefixBounds => provenance.prefix_bounds = None,
            ProvenanceField::CommitVersion => provenance.commit_version = None,
            ProvenanceField::VersionVector => provenance.version_vector = None,
            ProvenanceField::ScopeIdentity => provenance.scope_identity = None,
            ProvenanceField::TokenizerVersions => provenance.tokenizer_versions = None,
            ProvenanceField::StatisticsVersion => provenance.statistics_version = None,
            ProvenanceField::ProjectionEpoch => provenance.projection_epoch = None,
            ProvenanceField::ProjectionWatermarks => provenance.projection_watermarks = None,
            ProvenanceField::ContentDigest => provenance.content_digest = None,
            ProvenanceField::ContentSeal => provenance.content_seal = None,
            ProvenanceField::PageId => provenance.page_id = None,
            ProvenanceField::PageSequence => provenance.page_sequence = None,
        }
    }

    #[test]
    fn every_artifact_kind_accepts_a_complete_provenance() {
        let provenance = sample_provenance();
        for kind in ArtifactKind::ALL {
            provenance
                .validate(*kind)
                .unwrap_or_else(|error| panic!("{kind:?} rejected a complete block: {error}"));
        }
    }

    #[test]
    fn every_required_field_is_checked() {
        for kind in ArtifactKind::ALL {
            for field in kind.required_fields() {
                let mut provenance = sample_provenance();
                clear_field(&mut provenance, *field);
                assert_eq!(
                    provenance.missing(*kind),
                    vec![*field],
                    "{kind:?} did not report the missing field {field}"
                );
                assert_eq!(
                    provenance.validate(*kind),
                    Err(ProvenanceError::MissingField { field: *field })
                );
            }
        }
    }

    #[test]
    fn all_artifacts_require_the_manifest_digest() {
        for kind in ArtifactKind::ALL {
            assert!(
                kind.required_fields()
                    .contains(&ProvenanceField::ManifestDigest),
                "{kind:?} did not require the manifest digest"
            );
        }
    }

    #[test]
    fn statistics_version_is_optional_in_1_0() {
        let mut provenance = sample_provenance();
        provenance.statistics_version = None;
        assert!(provenance.validate(ArtifactKind::SearchArtifact).is_ok());
    }

    #[test]
    fn content_digest_covers_bytes_only() {
        let digest = ContentDigest::of_bytes(b"page bytes");
        assert_eq!(digest.as_bytes().len(), 32);
        assert_ne!(digest, ContentDigest::of_bytes(b"other bytes"));
    }

    #[test]
    fn a_seal_stays_valid_across_a_lease_change() {
        let digest = ContentDigest::of_bytes(b"immutable bytes");
        let old_seal = ContentSeal::new(digest, LeaseTerm::new(4), None);
        let new_seal = ContentSeal::new(digest, LeaseTerm::new(9), None);
        assert!(old_seal.covers(digest));
        assert!(new_seal.covers(digest));
        assert_eq!(old_seal.content_digest(), new_seal.content_digest());
        assert_ne!(old_seal.lease_term(), new_seal.lease_term());
    }

    #[test]
    fn version_axes_have_one_group() {
        for axis in VersionAxis::ALL {
            let group = axis.group();
            assert_ne!(group.name(), "");
            assert_ne!(axis.name(), "");
        }
    }

    #[test]
    fn version_text_round_trips() {
        let version = CrateVersion::new(1, 2, 3);
        assert_eq!(version.to_string(), "1.2.3");
        assert_eq!("1.2.3".parse::<CrateVersion>(), Ok(version));

        let page = SnapshotPageVersion::new(1, 0);
        assert_eq!(page.to_string(), "1.0");
        assert_eq!("1.0".parse::<SnapshotPageVersion>(), Ok(page));

        let manifest = ManifestSchemaVersion::new(1);
        assert_eq!(manifest.to_string(), "oxkv/manifest/v1");
        assert_eq!(
            "oxkv/manifest/v1".parse::<ManifestSchemaVersion>(),
            Ok(manifest)
        );
        assert_eq!(
            "oxkv/manifest/v2".parse::<ManifestSchemaVersion>(),
            Ok(ManifestSchemaVersion::new(2))
        );
        assert!("manifest/v1".parse::<ManifestSchemaVersion>().is_err());
    }

    #[test]
    fn a_version_vector_compares_per_shard() {
        let shard = ShardId::new(vec![1]);
        let low = VersionVector::from_entries([(shard.clone(), CommitVersion::new(3))]);
        let high = VersionVector::from_entries([(shard, CommitVersion::new(5))]);
        assert!(high.entails(&low));
        assert!(!low.entails(&high));
    }
}
