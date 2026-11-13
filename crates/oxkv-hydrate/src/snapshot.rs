//! The sealed snapshot artifact and its descriptor.
//!
//! The content digest covers the directory and every page digest. The content
//! seal covers the content digest and the lease term. The lease term does not
//! enter the content digest. A sealed snapshot stays readable after a lease
//! change.

use oxkv_contracts::version::{
    CommitVersion, ContentDigest, ContentSeal, KeyEncodingVersion, LeaseTerm, ManifestDigest,
    MapDigest, MapVersion, PageId, PageSequence, PolicyVersion, PrefixBounds, ScopeIdentity,
    ScopeVersionVector, ShardId, SnapshotIdentity, SnapshotPageVersion, VersionVector,
};

use crate::encoding::{DecodeError, Reader, Writer};
use crate::error::HydrationError;
use crate::page::{PAGE_FORMAT_MAJOR, SnapshotPage, check_features};

/// The snapshot magic bytes.
const SNAPSHOT_MAGIC: [u8; 4] = *b"OXSN";

/// The provenance presence bit for the key encoding version.
const PROVENANCE_KEY_ENCODING: u8 = 0b001;
/// The provenance presence bit for the shard map version.
const PROVENANCE_MAP_VERSION: u8 = 0b010;
/// The provenance presence bit for the shard map digest.
const PROVENANCE_MAP_DIGEST: u8 = 0b100;
/// Every provenance bit that the reader requires.
const PROVENANCE_REQUIRED: u8 =
    PROVENANCE_KEY_ENCODING | PROVENANCE_MAP_VERSION | PROVENANCE_MAP_DIGEST;

/// The identity and budget fields of one snapshot.
///
/// The descriptor carries the required provenance block of section 8.1. A
/// reader must not accept a descriptor that omits a required field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDescriptor {
    /// The snapshot identity: shard, commit version, and content digest.
    pub identity: SnapshotIdentity,
    /// The key encoding annex version.
    pub key_encoding_version: KeyEncodingVersion,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The shard map digest.
    pub map_digest: MapDigest,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The policy version.
    pub policy_version: PolicyVersion,
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
    /// The scope version vector.
    pub version_vector: ScopeVersionVector,
    /// The number of pages.
    pub page_count: u64,
    /// The logical snapshot byte count.
    pub snapshot_bytes: u64,
    /// The authority lease term.
    pub lease_term: LeaseTerm,
    /// The snapshot and page format version.
    pub format_version: SnapshotPageVersion,
    /// The required feature bits.
    pub required_features: u64,
    /// The shard key bounds.
    pub shard_bounds: PrefixBounds,
}

impl SnapshotDescriptor {
    /// Returns true when the descriptor has the same full identity.
    #[must_use]
    pub fn same_identity(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.key_encoding_version == other.key_encoding_version
            && self.map_version == other.map_version
            && self.map_digest == other.map_digest
            && self.manifest_digest == other.manifest_digest
            && self.policy_version == other.policy_version
            && self.scope_identity == other.scope_identity
            && self.version_vector == other.version_vector
            && self.page_count == other.page_count
            && self.format_version == other.format_version
            && self.required_features == other.required_features
    }

    /// Returns true when the descriptor names the same manifest, policy, and scope.
    #[must_use]
    pub fn same_scope(&self, other: &Self) -> bool {
        self.manifest_digest == other.manifest_digest
            && self.policy_version == other.policy_version
            && self.scope_identity == other.scope_identity
    }
}

/// One directory row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDirectoryEntry {
    /// The deterministic page identity.
    pub page_id: PageId,
    /// The page sequence.
    pub page_sequence: PageSequence,
    /// The absolute byte offset of the page in the snapshot.
    pub offset: u64,
    /// The encoded page length.
    pub length: u64,
    /// The first key in the page.
    pub first_key: Vec<u8>,
    /// The last key in the page.
    pub last_key: Vec<u8>,
    /// The BLAKE3 digest of the encoded page.
    pub page_digest: ContentDigest,
}

/// The header of one sealed snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotHeader {
    /// The snapshot and page format version.
    pub format_version: SnapshotPageVersion,
    /// The required feature bits.
    pub required_features: u64,
    /// The key encoding annex version.
    pub key_encoding_version: KeyEncodingVersion,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The shard map digest.
    pub map_digest: MapDigest,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The policy version.
    pub policy_version: PolicyVersion,
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
    /// The scope version vector.
    pub version_vector: ScopeVersionVector,
    /// The number of pages.
    pub page_count: u64,
    /// The shard identifier.
    pub shard_id: ShardId,
    /// The shard key bounds.
    pub shard_bounds: PrefixBounds,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The authority lease term.
    pub lease_term: LeaseTerm,
    /// The content digest.
    pub content_digest: ContentDigest,
    /// The BLAKE3 integrity value of the directory bytes.
    pub directory_integrity: ContentDigest,
    /// The content seal.
    pub content_seal: ContentSeal,
}

/// A snapshot builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDraft {
    /// The snapshot and page format version.
    pub format_version: SnapshotPageVersion,
    /// The required feature bits.
    pub required_features: u64,
    /// The key encoding annex version.
    pub key_encoding_version: KeyEncodingVersion,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The shard map digest.
    pub map_digest: MapDigest,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The policy version.
    pub policy_version: PolicyVersion,
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
    /// The scope version vector.
    pub version_vector: ScopeVersionVector,
    /// The shard identifier.
    pub shard_id: ShardId,
    /// The shard key bounds.
    pub shard_bounds: PrefixBounds,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The authority lease term.
    pub lease_term: LeaseTerm,
}

impl SnapshotDraft {
    /// Creates a deterministic sample draft for tests.
    #[must_use]
    pub fn sample(shard_bounds: PrefixBounds, lease_term: LeaseTerm) -> Self {
        let shard_id = ShardId::new(vec![1]);
        let commit_version = CommitVersion::new(1);
        Self {
            format_version: SnapshotPageVersion::new(PAGE_FORMAT_MAJOR, 0),
            required_features: 0,
            key_encoding_version: KeyEncodingVersion::new(1),
            map_version: MapVersion::new(1),
            map_digest: MapDigest::from_bytes([4; 32]),
            manifest_digest: ManifestDigest::from_bytes([1; 32]),
            policy_version: PolicyVersion::from_bytes([2; 32]),
            scope_identity: ScopeIdentity::from_bytes([3; 32]),
            version_vector: ScopeVersionVector::from_vector(VersionVector::from_entries([(
                shard_id.clone(),
                commit_version,
            )])),
            shard_id,
            shard_bounds,
            commit_version,
            lease_term,
        }
    }

    /// Seals the pages into one deterministic snapshot.
    ///
    /// # Errors
    ///
    /// Returns a format error when the pages do not have contiguous sequences,
    /// do not share the draft provenance, or do not fit the draft bounds.
    #[allow(clippy::too_many_lines)]
    pub fn seal(self, mut pages: Vec<SnapshotPage>) -> Result<SealedSnapshot, HydrationError> {
        if self.format_version.major() != PAGE_FORMAT_MAJOR {
            return Err(HydrationError::UnsupportedVersion {
                found: self.format_version,
                supported_major: PAGE_FORMAT_MAJOR,
            });
        }
        check_features(self.required_features)?;
        pages.sort_by_key(|page| page.page_sequence);
        for (index, page) in pages.iter().enumerate() {
            let expected = u64::try_from(index).unwrap_or(u64::MAX);
            if page.page_sequence.get() != expected {
                return Err(HydrationError::TruncatedSnapshot {
                    expected: pages.len(),
                    actual: index,
                });
            }
            if page.format_version.major() != PAGE_FORMAT_MAJOR {
                return Err(HydrationError::UnsupportedVersion {
                    found: page.format_version,
                    supported_major: PAGE_FORMAT_MAJOR,
                });
            }
            check_features(page.required_features)?;
            if page.shard_id != self.shard_id || page.commit_version != self.commit_version {
                return Err(HydrationError::IdentityMismatch);
            }
            page.check_bounds(&self.shard_bounds)?;
        }
        let page_count = u64::try_from(pages.len()).unwrap_or(u64::MAX);
        let mut entries = directory_entries(&pages, 0);
        let zero_directory = encode_directory(&entries);
        let mut header = SnapshotHeader {
            format_version: self.format_version,
            required_features: self.required_features,
            key_encoding_version: self.key_encoding_version,
            map_version: self.map_version,
            map_digest: self.map_digest,
            manifest_digest: self.manifest_digest,
            policy_version: self.policy_version,
            scope_identity: self.scope_identity,
            version_vector: self.version_vector,
            page_count,
            shard_id: self.shard_id,
            shard_bounds: self.shard_bounds,
            commit_version: self.commit_version,
            lease_term: self.lease_term,
            content_digest: ContentDigest::from_bytes([0; 32]),
            directory_integrity: ContentDigest::from_bytes([0; 32]),
            content_seal: ContentSeal::new(
                ContentDigest::from_bytes([0; 32]),
                self.lease_term,
                None,
            ),
        };
        let placeholder = encode_header(&header, &zero_directory, 0);
        let body_offset = u64::try_from(placeholder.len()).unwrap_or(u64::MAX);
        entries = directory_entries(&pages, body_offset);
        let directory_bytes = encode_directory(&entries);
        header.directory_integrity = ContentDigest::of_bytes(&directory_bytes);
        let content_digest = compute_content_digest(&directory_bytes, &entries);
        header.content_digest = content_digest;
        header.content_seal = ContentSeal::new(content_digest, self.lease_term, None);
        let body = encode_pages(&pages);
        let header_bytes = encode_header(&header, &directory_bytes, body.len());
        let mut bytes = header_bytes;
        bytes.extend_from_slice(&body);
        Ok(SealedSnapshot {
            header,
            directory: entries,
            pages,
            bytes,
        })
    }
}

/// One sealed snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSnapshot {
    /// The parsed header.
    pub header: SnapshotHeader,
    /// The parsed directory.
    pub directory: Vec<SnapshotDirectoryEntry>,
    /// The parsed pages in sequence order.
    pub pages: Vec<SnapshotPage>,
    bytes: Vec<u8>,
}

impl SealedSnapshot {
    /// Returns the canonical snapshot bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    /// Returns a borrow of the canonical snapshot bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the snapshot descriptor.
    #[must_use]
    pub fn descriptor(&self) -> SnapshotDescriptor {
        let snapshot_bytes = self.pages.iter().map(|page| page.uncompressed_length).sum();
        SnapshotDescriptor {
            identity: SnapshotIdentity::new(
                self.header.shard_id.clone(),
                self.header.commit_version,
                self.header.content_digest,
            ),
            key_encoding_version: self.header.key_encoding_version,
            map_version: self.header.map_version,
            map_digest: self.header.map_digest,
            manifest_digest: self.header.manifest_digest,
            policy_version: self.header.policy_version,
            scope_identity: self.header.scope_identity,
            version_vector: self.header.version_vector.clone(),
            page_count: self.header.page_count,
            snapshot_bytes,
            lease_term: self.header.lease_term,
            format_version: self.header.format_version,
            required_features: self.header.required_features,
            shard_bounds: self.header.shard_bounds.clone(),
        }
    }

    /// Replaces the lease term and recomputes the seal only.
    ///
    /// The content digest does not change. Use this method to model a lease
    /// change over immutable snapshot bytes.
    #[must_use]
    pub fn reseal(&self, lease_term: LeaseTerm) -> Self {
        let mut header = self.header.clone();
        header.lease_term = lease_term;
        header.content_seal = ContentSeal::new(self.header.content_digest, lease_term, None);
        let body = encode_pages(&self.pages);
        let directory_bytes = encode_directory(&self.directory);
        let header_bytes = encode_header(&header, &directory_bytes, body.len());
        let mut bytes = header_bytes;
        bytes.extend_from_slice(&body);
        Self {
            header,
            directory: self.directory.clone(),
            pages: self.pages.clone(),
            bytes,
        }
    }

    /// Decodes and verifies one sealed snapshot.
    ///
    /// The method checks the content digest before the content seal. It then
    /// checks every page checksum and the directory coverage.
    ///
    /// # Errors
    ///
    /// Returns a named format error for an unknown version, an unknown required
    /// feature, a corrupt page, an out-of-bounds page, or a truncated
    /// snapshot.
    #[allow(clippy::too_many_lines)]
    pub fn decode(bytes: &[u8]) -> Result<Self, HydrationError> {
        let mut reader = Reader::new(bytes);
        let magic = reader.exact(4).map_err(decode_error)?;
        if magic != SNAPSHOT_MAGIC {
            return Err(HydrationError::CorruptPage {
                page_id: None,
                byte_offset: 0,
            });
        }
        let major = reader.u16().map_err(decode_error)?;
        let minor = reader.u16().map_err(decode_error)?;
        let format_version = SnapshotPageVersion::new(major, minor);
        if major != PAGE_FORMAT_MAJOR {
            return Err(HydrationError::UnsupportedVersion {
                found: format_version,
                supported_major: PAGE_FORMAT_MAJOR,
            });
        }
        let required_features = reader.u64().map_err(decode_error)?;
        check_features(required_features)?;
        let key_encoding_version = KeyEncodingVersion::new(reader.u16().map_err(decode_error)?);
        let map_version = MapVersion::new(reader.u64().map_err(decode_error)?);
        let map_digest = MapDigest::from_bytes(reader.hash32().map_err(decode_error)?);
        let provenance = reader.u8().map_err(decode_error)?;
        check_provenance(provenance)?;
        let manifest_digest = ManifestDigest::from_bytes(reader.hash32().map_err(decode_error)?);
        let policy_version = PolicyVersion::from_bytes(reader.hash32().map_err(decode_error)?);
        let scope_identity = ScopeIdentity::from_bytes(reader.hash32().map_err(decode_error)?);
        let vector_count = reader.u32().map_err(decode_error)?;
        let mut vector = VersionVector::new();
        for _ in 0..vector_count {
            let shard_id = ShardId::new(reader.bytes().map_err(decode_error)?.to_vec());
            let commit_version = CommitVersion::new(reader.u64().map_err(decode_error)?);
            vector.set(shard_id, commit_version);
        }
        let page_count = reader.u64().map_err(decode_error)?;
        let shard_id = ShardId::new(reader.bytes().map_err(decode_error)?.to_vec());
        let shard_bounds = PrefixBounds::new(
            reader.bytes().map_err(decode_error)?.to_vec(),
            reader.bytes().map_err(decode_error)?.to_vec(),
        );
        let commit_version = CommitVersion::new(reader.u64().map_err(decode_error)?);
        let lease_term = LeaseTerm::new(reader.u64().map_err(decode_error)?);
        let content_digest = ContentDigest::from_bytes(reader.hash32().map_err(decode_error)?);
        let seal_digest = ContentDigest::from_bytes(reader.hash32().map_err(decode_error)?);
        let seal_lease = LeaseTerm::new(reader.u64().map_err(decode_error)?);
        let signature_flag = reader.u8().map_err(decode_error)?;
        let directory_integrity = ContentDigest::from_bytes(reader.hash32().map_err(decode_error)?);
        let directory_length = reader.u64().map_err(decode_error)?;
        let directory_length =
            usize::try_from(directory_length).map_err(|_| HydrationError::TruncatedSnapshot {
                expected: bytes.len(),
                actual: bytes.len(),
            })?;
        let directory_bytes = reader.exact(directory_length).map_err(decode_error)?;
        if ContentDigest::of_bytes(directory_bytes) != directory_integrity {
            return Err(HydrationError::CorruptPage {
                page_id: None,
                byte_offset: 0,
            });
        }
        let directory = decode_directory(directory_bytes)?;
        let computed_digest = compute_content_digest(directory_bytes, &directory);
        if computed_digest != content_digest {
            return Err(HydrationError::CorruptPage {
                page_id: None,
                byte_offset: 0,
            });
        }
        if seal_lease != lease_term {
            return Err(HydrationError::StaleEpoch {
                expected: lease_term,
                found: seal_lease,
            });
        }
        if signature_flag != 0 {
            return Err(HydrationError::UnsupportedSignature);
        }
        let seal = ContentSeal::new(seal_digest, seal_lease, None);
        if !seal.covers(computed_digest) {
            return Err(HydrationError::CorruptPage {
                page_id: None,
                byte_offset: 0,
            });
        }
        let body_length = reader.u64().map_err(decode_error)?;
        let body_length =
            usize::try_from(body_length).map_err(|_| HydrationError::TruncatedSnapshot {
                expected: bytes.len(),
                actual: bytes.len(),
            })?;
        let body_offset = reader.offset();
        let body = reader.exact(body_length).map_err(decode_error)?;
        reader.finish().map_err(decode_error)?;
        if u64::try_from(directory.len()).unwrap_or(u64::MAX) != page_count {
            return Err(HydrationError::TruncatedSnapshot {
                expected: usize::try_from(page_count).unwrap_or(usize::MAX),
                actual: directory.len(),
            });
        }
        let mut pages = Vec::with_capacity(directory.len());
        for (index, entry) in directory.iter().enumerate() {
            if entry.page_sequence.get() != u64::try_from(index).unwrap_or(u64::MAX) {
                return Err(HydrationError::TruncatedSnapshot {
                    expected: directory.len(),
                    actual: index,
                });
            }
            let start = entry
                .offset
                .checked_sub(u64::try_from(body_offset).unwrap_or(u64::MAX))
                .and_then(|value| usize::try_from(value).ok())
                .ok_or(HydrationError::TruncatedSnapshot {
                    expected: body.len(),
                    actual: 0,
                })?;
            let length =
                usize::try_from(entry.length).map_err(|_| HydrationError::TruncatedSnapshot {
                    expected: body.len(),
                    actual: 0,
                })?;
            let end = start
                .checked_add(length)
                .ok_or(HydrationError::TruncatedSnapshot {
                    expected: body.len(),
                    actual: 0,
                })?;
            let frame = body
                .get(start..end)
                .ok_or(HydrationError::TruncatedSnapshot {
                    expected: end,
                    actual: body.len(),
                })?;
            let page = SnapshotPage::decode(frame)?;
            if page.page_sequence != entry.page_sequence || page.page_id != entry.page_id {
                return Err(HydrationError::CorruptPage {
                    page_id: Some(entry.page_id),
                    byte_offset: start,
                });
            }
            if page.digest() != entry.page_digest {
                return Err(HydrationError::CorruptPage {
                    page_id: Some(page.page_id),
                    byte_offset: start,
                });
            }
            if page.shard_id != shard_id
                || page.commit_version != commit_version
                || page.check_bounds(&shard_bounds).is_err()
            {
                return Err(HydrationError::PageOutOfBounds {
                    page_sequence: page.page_sequence,
                });
            }
            pages.push(page);
        }
        let header = SnapshotHeader {
            format_version,
            required_features,
            key_encoding_version,
            map_version,
            map_digest,
            manifest_digest,
            policy_version,
            scope_identity,
            version_vector: ScopeVersionVector::from_vector(vector),
            page_count,
            shard_id,
            shard_bounds,
            commit_version,
            lease_term,
            content_digest,
            directory_integrity,
            content_seal: ContentSeal::new(seal_digest, seal_lease, None),
        };
        Ok(Self {
            header,
            directory,
            pages,
            bytes: bytes.to_vec(),
        })
    }
}

/// Rebuilds the directory and content digest for a set of verified pages.
///
/// The function uses the descriptor header fields. The result matches the
/// content digest of the sealed snapshot for the same pages.
pub(crate) fn directory_and_digest(
    descriptor: &SnapshotDescriptor,
    pages: &[SnapshotPage],
) -> (Vec<SnapshotDirectoryEntry>, ContentDigest) {
    let header = SnapshotHeader {
        format_version: descriptor.format_version,
        required_features: descriptor.required_features,
        key_encoding_version: descriptor.key_encoding_version,
        map_version: descriptor.map_version,
        map_digest: descriptor.map_digest,
        manifest_digest: descriptor.manifest_digest,
        policy_version: descriptor.policy_version,
        scope_identity: descriptor.scope_identity,
        version_vector: descriptor.version_vector.clone(),
        page_count: descriptor.page_count,
        shard_id: descriptor.identity.shard_id.clone(),
        shard_bounds: descriptor.shard_bounds.clone(),
        commit_version: descriptor.identity.commit_version,
        lease_term: descriptor.lease_term,
        content_digest: ContentDigest::from_bytes([0; 32]),
        directory_integrity: ContentDigest::from_bytes([0; 32]),
        content_seal: ContentSeal::new(
            ContentDigest::from_bytes([0; 32]),
            descriptor.lease_term,
            None,
        ),
    };
    let zero_entries = directory_entries(pages, 0);
    let zero_directory = encode_directory(&zero_entries);
    let placeholder = encode_header(&header, &zero_directory, 0);
    let body_offset = u64::try_from(placeholder.len()).unwrap_or(u64::MAX);
    let entries = directory_entries(pages, body_offset);
    let directory = encode_directory(&entries);
    let digest = compute_content_digest(&directory, &entries);
    (entries, digest)
}

fn directory_entries(pages: &[SnapshotPage], body_offset: u64) -> Vec<SnapshotDirectoryEntry> {
    let mut offset = body_offset;
    let mut entries = Vec::with_capacity(pages.len());
    for page in pages {
        let encoded = page.encode();
        let length = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        entries.push(SnapshotDirectoryEntry {
            page_id: page.page_id,
            page_sequence: page.page_sequence,
            offset,
            length,
            first_key: page.bounds.lower().to_vec(),
            last_key: page.bounds.upper().to_vec(),
            page_digest: ContentDigest::of_bytes(&encoded),
        });
        offset = offset.saturating_add(length);
    }
    entries
}

fn encode_directory(entries: &[SnapshotDirectoryEntry]) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.u64(u64::try_from(entries.len()).unwrap_or(u64::MAX));
    for entry in entries {
        writer.hash32(entry.page_id.as_bytes());
        writer.u64(entry.page_sequence.get());
        writer.u64(entry.offset);
        writer.u64(entry.length);
        writer.bytes(&entry.first_key);
        writer.bytes(&entry.last_key);
        writer.hash32(entry.page_digest.as_bytes());
    }
    writer.into_bytes()
}

fn decode_directory(bytes: &[u8]) -> Result<Vec<SnapshotDirectoryEntry>, HydrationError> {
    let mut reader = Reader::new(bytes);
    let count = reader.u64().map_err(decode_error)?;
    let count = usize::try_from(count).map_err(|_| HydrationError::CorruptPage {
        page_id: None,
        byte_offset: 0,
    })?;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(SnapshotDirectoryEntry {
            page_id: PageId::from_bytes(reader.hash32().map_err(decode_error)?),
            page_sequence: PageSequence::new(reader.u64().map_err(decode_error)?),
            offset: reader.u64().map_err(decode_error)?,
            length: reader.u64().map_err(decode_error)?,
            first_key: reader.bytes().map_err(decode_error)?.to_vec(),
            last_key: reader.bytes().map_err(decode_error)?.to_vec(),
            page_digest: ContentDigest::from_bytes(reader.hash32().map_err(decode_error)?),
        });
    }
    reader.finish().map_err(decode_error)?;
    Ok(entries)
}

fn compute_content_digest(directory: &[u8], entries: &[SnapshotDirectoryEntry]) -> ContentDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(directory);
    for entry in entries {
        hasher.update(entry.page_digest.as_bytes());
    }
    ContentDigest::from_bytes(*hasher.finalize().as_bytes())
}

fn encode_pages(pages: &[SnapshotPage]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for page in pages {
        bytes.extend_from_slice(&page.encode());
    }
    bytes
}

fn encode_header(header: &SnapshotHeader, directory: &[u8], body_length: usize) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.raw(&SNAPSHOT_MAGIC);
    writer.u16(header.format_version.major());
    writer.u16(header.format_version.minor());
    writer.u64(header.required_features);
    writer.u16(header.key_encoding_version.get());
    writer.u64(header.map_version.get());
    writer.hash32(header.map_digest.as_bytes());
    writer.u8(PROVENANCE_REQUIRED);
    writer.hash32(header.manifest_digest.as_bytes());
    writer.hash32(header.policy_version.as_bytes());
    writer.hash32(header.scope_identity.as_bytes());
    let vector = header.version_vector.as_vector();
    writer.u32(u32::try_from(vector.len()).unwrap_or(u32::MAX));
    for (shard_id, commit_version) in vector.iter() {
        writer.bytes(shard_id.as_bytes());
        writer.u64(commit_version.get());
    }
    writer.u64(header.page_count);
    writer.bytes(header.shard_id.as_bytes());
    writer.bytes(header.shard_bounds.lower());
    writer.bytes(header.shard_bounds.upper());
    writer.u64(header.commit_version.get());
    writer.u64(header.lease_term.get());
    writer.hash32(header.content_digest.as_bytes());
    writer.hash32(header.content_seal.content_digest().as_bytes());
    writer.u64(header.content_seal.lease_term().get());
    writer.u8(u8::from(header.content_seal.signature().is_some()));
    writer.hash32(ContentDigest::of_bytes(directory).as_bytes());
    writer.u64(u64::try_from(directory.len()).unwrap_or(u64::MAX));
    writer.raw(directory);
    writer.u64(u64::try_from(body_length).unwrap_or(u64::MAX));
    writer.into_bytes()
}

fn check_provenance(mask: u8) -> Result<(), HydrationError> {
    if mask & PROVENANCE_KEY_ENCODING == 0 {
        return Err(HydrationError::MissingProvenance {
            field: "key_encoding_version",
        });
    }
    if mask & PROVENANCE_MAP_VERSION == 0 {
        return Err(HydrationError::MissingProvenance {
            field: "map_version",
        });
    }
    if mask & PROVENANCE_MAP_DIGEST == 0 {
        return Err(HydrationError::MissingProvenance {
            field: "map_digest",
        });
    }
    Ok(())
}

fn decode_error(error: DecodeError) -> HydrationError {
    match error {
        DecodeError::Truncated { expected, actual } => {
            HydrationError::TruncatedSnapshot { expected, actual }
        }
        DecodeError::Invalid => HydrationError::CorruptPage {
            page_id: None,
            byte_offset: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::SnapshotPage;

    fn sample_snapshot(lease_term: LeaseTerm) -> SealedSnapshot {
        let bounds = PrefixBounds::new(vec![b'a'], vec![b'z']);
        let page = SnapshotPage::with_records(
            0,
            vec![(b"alpha".to_vec(), b"one".to_vec())],
            bounds.clone(),
        );
        SnapshotDraft::sample(bounds, lease_term)
            .seal(vec![page])
            .expect("the sample snapshot seals")
    }

    #[test]
    fn a_sealed_snapshot_round_trips() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let decoded = SealedSnapshot::decode(&snapshot.to_bytes()).expect("the snapshot decodes");
        assert_eq!(decoded.header, snapshot.header);
        assert_eq!(decoded.directory, snapshot.directory);
        assert_eq!(decoded.pages, snapshot.pages);
    }

    #[test]
    fn a_sealed_snapshot_survives_a_lease_change() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let resealed = snapshot.reseal(LeaseTerm::new(9));
        let decoded = SealedSnapshot::decode(&resealed.to_bytes()).expect("the snapshot decodes");
        assert_eq!(
            decoded.header.content_digest,
            snapshot.header.content_digest
        );
        assert_eq!(decoded.header.lease_term, LeaseTerm::new(9));
        assert_ne!(
            decoded.header.content_seal.lease_term(),
            snapshot.header.content_seal.lease_term()
        );
        assert_eq!(decoded.pages, snapshot.pages);
    }

    #[test]
    fn an_unknown_major_version_fails() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let mut bytes = snapshot.to_bytes();
        bytes[4] = 2;
        assert_eq!(
            SealedSnapshot::decode(&bytes)
                .expect_err("the version fails")
                .code(),
            "format.unsupported_version"
        );
    }

    #[test]
    fn an_unknown_required_feature_fails() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let mut bytes = snapshot.to_bytes();
        bytes[8] = 0b10;
        assert_eq!(
            SealedSnapshot::decode(&bytes)
                .expect_err("the feature fails")
                .code(),
            "format.missing_required_feature"
        );
    }

    #[test]
    fn a_short_snapshot_fails() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let bytes = snapshot.to_bytes();
        assert_eq!(
            SealedSnapshot::decode(&bytes[..bytes.len() - 3])
                .expect_err("the short snapshot fails")
                .code(),
            "format.truncated_snapshot"
        );
    }

    #[test]
    fn a_missing_provenance_field_is_rejected() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let mut bytes = snapshot.to_bytes();
        bytes[58] = 0;
        assert_eq!(
            SealedSnapshot::decode(&bytes)
                .expect_err("the provenance is missing")
                .code(),
            "format.missing_provenance"
        );
    }

    #[test]
    fn a_changed_directory_byte_fails() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let mut bytes = snapshot.to_bytes();
        let body_length = snapshot
            .pages
            .iter()
            .map(|page| page.encode().len())
            .sum::<usize>();
        let directory_length = encode_directory(&snapshot.directory).len();
        let directory_start = bytes.len() - body_length - 8 - directory_length;
        bytes[directory_start] ^= 0x01;
        assert_eq!(
            SealedSnapshot::decode(&bytes)
                .expect_err("the directory integrity fails")
                .code(),
            "format.corrupt_page"
        );
    }

    #[test]
    fn a_mismatched_seal_lease_is_rejected() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let mut header = snapshot.header.clone();
        header.content_seal = ContentSeal::new(header.content_digest, LeaseTerm::new(2), None);
        let directory = encode_directory(&snapshot.directory);
        let body = encode_pages(&snapshot.pages);
        let mut bytes = encode_header(&header, &directory, body.len());
        bytes.extend_from_slice(&body);
        assert_eq!(
            SealedSnapshot::decode(&bytes)
                .expect_err("the seal lease differs")
                .code(),
            "replication.stale_epoch"
        );
    }

    #[test]
    fn a_changed_page_fails_before_the_seal() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let mut bytes = snapshot.to_bytes();
        let last = bytes.len() - 2;
        bytes[last] ^= 0xff;
        let error = SealedSnapshot::decode(&bytes).expect_err("the page change fails");
        assert_eq!(error.code(), "format.corrupt_page");
        assert!(matches!(
            error,
            HydrationError::CorruptPage {
                page_id: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn an_out_of_bounds_page_fails() {
        let bounds = PrefixBounds::new(vec![b'a'], vec![b'z']);
        let page = SnapshotPage::with_records(
            0,
            vec![(b"alpha".to_vec(), b"one".to_vec())],
            PrefixBounds::new(vec![b'A'], vec![b'{']),
        );
        let error = SnapshotDraft::sample(bounds, LeaseTerm::new(1))
            .seal(vec![page])
            .expect_err("the page is outside the snapshot");
        assert_eq!(error.code(), "format.page_out_of_bounds");
    }

    #[test]
    fn a_descriptor_carries_the_required_provenance() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let descriptor = snapshot.descriptor();
        assert_eq!(
            descriptor.key_encoding_version,
            snapshot.header.key_encoding_version
        );
        assert_eq!(descriptor.map_version, snapshot.header.map_version);
        assert_eq!(descriptor.map_digest, snapshot.header.map_digest);
    }

    #[test]
    fn a_descriptor_identity_includes_the_map_provenance() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let descriptor = snapshot.descriptor();
        assert!(descriptor.same_identity(&snapshot.descriptor()));

        let mut other_key_encoding = descriptor.clone();
        other_key_encoding.key_encoding_version = KeyEncodingVersion::new(2);
        assert!(!descriptor.same_identity(&other_key_encoding));

        let mut other_map_version = descriptor.clone();
        other_map_version.map_version = MapVersion::new(2);
        assert!(!descriptor.same_identity(&other_map_version));

        let mut other_map_digest = descriptor.clone();
        other_map_digest.map_digest = MapDigest::from_bytes([9; 32]);
        assert!(!descriptor.same_identity(&other_map_digest));
    }

    #[test]
    fn the_descriptor_provenance_rebuilds_the_snapshot_digest() {
        let snapshot = sample_snapshot(LeaseTerm::new(1));
        let descriptor = snapshot.descriptor();
        let (entries, digest) = directory_and_digest(&descriptor, &snapshot.pages);
        assert_eq!(entries, snapshot.directory);
        assert_eq!(digest, snapshot.header.content_digest);
    }
}
