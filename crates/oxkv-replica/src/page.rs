//! Scope snapshot pages and records.
//!
//! A [`ScopeSnapshotPage`] holds authority page bytes that the policy already
//! filtered. The replica verifies the page digest and the scope membership. It
//! stores the page unchanged. It never re-encodes or re-blocks a page.
//!
//! The page layout here is the in-memory form that the replica receives. The
//! snapshot and page byte contract belongs to the snapshot format work. The
//! digest covers the page descriptor and every record in canonical order.

use oxkv_contracts::version::{
    CommitVersion, ContentDigest, FactSourceId, FactVersion, KeyVersion, ManifestDigest, PageId,
    PageSequence, PolicyVersion, PrefixBounds, ScopeIdentity, ScopeVersionVector, ShardId,
    SnapshotPageVersion,
};

use crate::codec::{Encoder, as_u64};
use crate::error::ReplicaError;

/// The only supported snapshot page major version in 1.0.
pub const SUPPORTED_PAGE_VERSION: SnapshotPageVersion = SnapshotPageVersion::new(1, 0);

/// The body of one mirrored record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordBody {
    /// A user row.
    Row {
        /// The version of the key record.
        key_version: KeyVersion,
    },
    /// A projection fact that the scope needs.
    Fact {
        /// The fact source.
        source: FactSourceId,
        /// The version of the fact.
        fact_version: FactVersion,
    },
}

impl RecordBody {
    /// Returns true when the record is a projection fact.
    #[must_use]
    pub const fn is_fact(&self) -> bool {
        matches!(self, Self::Fact { .. })
    }

    /// Returns the key version of a row record.
    #[must_use]
    pub const fn key_version(&self) -> Option<KeyVersion> {
        match self {
            Self::Row { key_version } => Some(*key_version),
            Self::Fact { .. } => None,
        }
    }

    /// Returns the fact source of a fact record.
    #[must_use]
    pub fn fact_source(&self) -> Option<&FactSourceId> {
        match self {
            Self::Row { .. } => None,
            Self::Fact { source, .. } => Some(source),
        }
    }

    /// Returns the fact version of a fact record.
    #[must_use]
    pub const fn fact_version(&self) -> Option<FactVersion> {
        match self {
            Self::Row { .. } => None,
            Self::Fact { fact_version, .. } => Some(*fact_version),
        }
    }

    fn encode(&self, encoder: &mut Encoder) {
        match self {
            Self::Row { key_version } => {
                encoder.u8(0);
                encoder.u64(key_version.get());
            }
            Self::Fact {
                source,
                fact_version,
            } => {
                encoder.u8(1);
                encoder.text(source.as_str());
                encoder.u64(fact_version.get());
            }
        }
    }
}

/// One record inside a scope snapshot page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirroredRecord {
    /// The canonical key.
    pub key: Vec<u8>,
    /// The record body.
    pub body: RecordBody,
    /// The value. `None` is a tombstone.
    pub value: Option<Vec<u8>>,
}

impl MirroredRecord {
    /// Creates a row record.
    #[must_use]
    pub fn row(key: Vec<u8>, value: Vec<u8>, key_version: KeyVersion) -> Self {
        Self {
            key,
            body: RecordBody::Row { key_version },
            value: Some(value),
        }
    }

    /// Creates a row tombstone.
    #[must_use]
    pub fn tombstone(key: Vec<u8>, key_version: KeyVersion) -> Self {
        Self {
            key,
            body: RecordBody::Row { key_version },
            value: None,
        }
    }

    /// Creates a projection fact record.
    #[must_use]
    pub fn fact(
        source: FactSourceId,
        key: Vec<u8>,
        value: Vec<u8>,
        fact_version: FactVersion,
    ) -> Self {
        Self {
            key,
            body: RecordBody::Fact {
                source,
                fact_version,
            },
            value: Some(value),
        }
    }

    /// Returns true when the record is a projection fact.
    #[must_use]
    pub const fn is_fact(&self) -> bool {
        self.body.is_fact()
    }

    fn encode(&self, encoder: &mut Encoder) {
        self.body.encode(encoder);
        encoder.bytes(&self.key);
        match &self.value {
            Some(value) => {
                encoder.u8(1);
                encoder.bytes(value);
            }
            None => encoder.u8(0),
        }
    }
}

/// The page fields that the digest covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageDescriptor {
    /// The snapshot page format version.
    pub format_version: SnapshotPageVersion,
    /// The deterministic page identity.
    pub page_id: PageId,
    /// The page sequence in one snapshot.
    pub page_sequence: PageSequence,
    /// The shard that owns the page.
    pub shard_id: ShardId,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The inclusive lower key and the exclusive upper key.
    pub bounds: PrefixBounds,
    /// The scope identity.
    pub scope_identity: ScopeIdentity,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The policy version.
    pub policy_version: PolicyVersion,
}

impl PageDescriptor {
    fn encode(&self, encoder: &mut Encoder) {
        encoder.u16(self.format_version.major());
        encoder.u16(self.format_version.minor());
        encoder.bytes(self.page_id.as_bytes());
        encoder.u64(self.page_sequence.get());
        encoder.bytes(self.shard_id.as_bytes());
        encoder.u64(self.commit_version.get());
        encoder.bytes(self.bounds.lower());
        encoder.bytes(self.bounds.upper());
        encoder.bytes(self.scope_identity.as_bytes());
        encoder.bytes(self.manifest_digest.as_bytes());
        encoder.bytes(self.policy_version.as_bytes());
    }
}

/// The verified header of one scope snapshot page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHeader {
    /// The snapshot page format version.
    pub format_version: SnapshotPageVersion,
    /// The deterministic page identity.
    pub page_id: PageId,
    /// The page sequence in one snapshot.
    pub page_sequence: PageSequence,
    /// The shard that owns the page.
    pub shard_id: ShardId,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The inclusive lower key and the exclusive upper key.
    pub bounds: PrefixBounds,
    /// The record count.
    pub record_count: u64,
    /// The digest of the descriptor and records.
    pub content_digest: ContentDigest,
    /// The scope identity.
    pub scope_identity: ScopeIdentity,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The policy version.
    pub policy_version: PolicyVersion,
}

impl PageHeader {
    /// Returns the digest descriptor of this header.
    #[must_use]
    pub fn descriptor(&self) -> PageDescriptor {
        PageDescriptor {
            format_version: self.format_version,
            page_id: self.page_id,
            page_sequence: self.page_sequence,
            shard_id: self.shard_id.clone(),
            commit_version: self.commit_version,
            bounds: self.bounds.clone(),
            scope_identity: self.scope_identity,
            manifest_digest: self.manifest_digest,
            policy_version: self.policy_version,
        }
    }

    /// Returns true when the key is inside the page bounds.
    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        key >= self.bounds.lower() && key < self.bounds.upper()
    }
}

/// One scope snapshot page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSnapshotPage {
    /// The page header.
    pub header: PageHeader,
    /// The page records in canonical order.
    pub records: Vec<MirroredRecord>,
}

impl ScopeSnapshotPage {
    /// Creates a page and computes its content digest.
    #[must_use]
    pub fn new(descriptor: PageDescriptor, records: Vec<MirroredRecord>) -> Self {
        let content_digest = compute_digest(&descriptor, &records);
        let record_count = as_u64(records.len());
        Self {
            header: PageHeader {
                format_version: descriptor.format_version,
                page_id: descriptor.page_id,
                page_sequence: descriptor.page_sequence,
                shard_id: descriptor.shard_id,
                commit_version: descriptor.commit_version,
                bounds: descriptor.bounds,
                record_count,
                content_digest,
                scope_identity: descriptor.scope_identity,
                manifest_digest: descriptor.manifest_digest,
                policy_version: descriptor.policy_version,
            },
            records,
        }
    }

    /// Returns the page identity.
    #[must_use]
    pub const fn page_id(&self) -> PageId {
        self.header.page_id
    }

    /// Returns the shard identifier.
    #[must_use]
    pub fn shard_id(&self) -> &ShardId {
        &self.header.shard_id
    }

    /// Returns the shard commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.header.commit_version
    }

    /// Returns true when the page digest matches the page bytes.
    #[must_use]
    pub fn verify_digest(&self) -> bool {
        compute_digest(&self.header.descriptor(), &self.records) == self.header.content_digest
    }

    /// Returns the resident byte count of the page.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        let mut encoder = Encoder::new();
        self.header.descriptor().encode(&mut encoder);
        encoder.u64(self.header.record_count);
        encoder.bytes(self.header.content_digest.as_bytes());
        for record in &self.records {
            record.encode(&mut encoder);
        }
        as_u64(encoder.len())
    }

    /// Checks the format major version and the page digest.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::UnsupportedVersion`] for another major version.
    /// Returns [`ReplicaError::CorruptPage`] when the digest does not match.
    pub fn verify(&self) -> Result<(), ReplicaError> {
        if self.header.format_version.major() != SUPPORTED_PAGE_VERSION.major() {
            return Err(ReplicaError::UnsupportedVersion {
                supported: SUPPORTED_PAGE_VERSION,
                found: self.header.format_version,
            });
        }
        if !self.verify_digest() {
            return Err(ReplicaError::CorruptPage {
                page_id: self.header.page_id,
                offset: 0,
            });
        }
        if self.header.record_count != as_u64(self.records.len()) {
            return Err(ReplicaError::CorruptPage {
                page_id: self.header.page_id,
                offset: 0,
            });
        }
        Ok(())
    }
}

/// One deterministic `ScopeSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSnapshot {
    /// The scope identity.
    pub scope_identity: ScopeIdentity,
    /// The scope version vector.
    pub scope_version: ScopeVersionVector,
    /// The authority watermark at the snapshot.
    pub authority_watermark: ScopeVersionVector,
    /// The pages in page sequence order.
    pub pages: Vec<ScopeSnapshotPage>,
}

impl ScopeSnapshot {
    /// Creates a `ScopeSnapshot`.
    #[must_use]
    pub fn new(
        scope_identity: ScopeIdentity,
        scope_version: ScopeVersionVector,
        authority_watermark: ScopeVersionVector,
        pages: Vec<ScopeSnapshotPage>,
    ) -> Self {
        Self {
            scope_identity,
            scope_version,
            authority_watermark,
            pages,
        }
    }

    /// Returns the resident byte count of every page.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.pages.iter().fold(0_u64, |total, page| {
            total.saturating_add(page.resident_bytes())
        })
    }
}

fn compute_digest(descriptor: &PageDescriptor, records: &[MirroredRecord]) -> ContentDigest {
    let mut encoder = Encoder::with_capacity(64 + records.len().saturating_mul(32));
    descriptor.encode(&mut encoder);
    encoder.u64(as_u64(records.len()));
    for record in records {
        record.encode(&mut encoder);
    }
    ContentDigest::of_bytes(&encoder.into_bytes())
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{
        CommitVersion, FactSourceId, FactVersion, KeyVersion, ManifestDigest, PageId, PageSequence,
        PolicyVersion, PrefixBounds, ScopeIdentity, ShardId, SnapshotPageVersion,
    };

    use super::{MirroredRecord, PageDescriptor, ScopeSnapshotPage};

    fn descriptor() -> PageDescriptor {
        PageDescriptor {
            format_version: SnapshotPageVersion::new(1, 0),
            page_id: PageId::from_bytes([1; 32]),
            page_sequence: PageSequence::new(1),
            shard_id: ShardId::new(vec![1]),
            commit_version: CommitVersion::new(7),
            bounds: PrefixBounds::new(b"\x01accounts\x00".to_vec(), b"\x01accounts\x01".to_vec()),
            scope_identity: ScopeIdentity::from_bytes([2; 32]),
            manifest_digest: ManifestDigest::from_bytes([3; 32]),
            policy_version: PolicyVersion::from_bytes([4; 32]),
        }
    }

    fn page() -> ScopeSnapshotPage {
        ScopeSnapshotPage::new(
            descriptor(),
            vec![
                MirroredRecord::row(
                    b"\x01accounts\x00\x10alice".to_vec(),
                    b"admin".to_vec(),
                    KeyVersion::new(1),
                ),
                MirroredRecord::fact(
                    FactSourceId::new("roles"),
                    b"role:admin".to_vec(),
                    b"true".to_vec(),
                    FactVersion::new(2),
                ),
            ],
        )
    }

    #[test]
    fn a_page_verifies_its_digest() {
        let page = page();
        assert!(page.verify_digest());
        assert!(page.verify().is_ok());
        assert_eq!(page.header.record_count, 2);
    }

    #[test]
    fn a_changed_record_fails_the_digest() {
        let mut page = page();
        page.records[0].value = Some(b"owner".to_vec());
        assert!(!page.verify_digest());
        assert_eq!(
            page.verify().expect_err("the page is corrupt").code(),
            "format.corrupt_page"
        );
    }

    #[test]
    fn another_major_version_is_unsupported() {
        let mut page = page();
        page.header.format_version = SnapshotPageVersion::new(2, 0);
        assert_eq!(
            page.verify()
                .expect_err("the version is unsupported")
                .code(),
            "format.unsupported_version"
        );
    }

    #[test]
    fn the_bounds_contain_only_page_keys() {
        let page = page();
        assert!(page.header.contains(b"\x01accounts\x00\x10alice"));
        assert!(!page.header.contains(b"\x01accounts\x01\x10alice"));
        assert!(!page.header.contains(b"\x01orders\x00\x10alice"));
    }
}
