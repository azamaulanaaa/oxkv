//! Snapshot construction and verified reading.
//!
//! [`ShardSnapshot`] covers one shard at one commit version. [`ScopeSnapshot`]
//! covers one scope at one scope version vector. Both use the same text header,
//! binary pages, directory, content digest, and content seal.

use core::cmp::Ordering;

use oxkv_contracts::version::{
    CommitVersion, ContentDigest, ContentSeal, KeyEncodingVersion, LeaseTerm, ManifestDigest,
    MapDigest, MapVersion, PageId, PageSequence, PolicyVersion, PrefixBounds, ProjectionEpoch,
    ProjectionWatermarks, ScopeIdentity, ScopeVersionVector, ShardId, Signature,
    SnapshotPageVersion,
};

use crate::codec::CodecId;
use crate::directory::{Directory, DirectoryEntry};
use crate::error::SnapshotError;
use crate::header::{Header, HeaderIdentity, KNOWN_REQUIRED_FEATURES, ScopeHeader, ShardHeader};
use crate::page::{Page, Record};
use crate::seal::{content_digest_for, verify_content_digest, verify_seal};
use crate::{
    DEFAULT_PAGE_SIZE, FORMAT_MAJOR, FORMAT_MINOR, MAX_KEY_BYTES, MAX_PAGE_SIZE, MAX_VALUE_BYTES,
};

/// The parameters of one `ShardSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardSnapshotParams {
    /// The opaque shard identifier.
    pub shard_id: ShardId,
    /// The inclusive lower bound and the exclusive upper bound.
    pub prefix_bounds: PrefixBounds,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The shard map digest.
    pub map_digest: MapDigest,
    /// The key encoding annex version.
    pub key_encoding_version: KeyEncodingVersion,
    /// The target page payload size in bytes.
    pub page_size: u64,
    /// The page codec identifier.
    pub codec: CodecId,
    /// The authority lease term.
    pub lease_term: LeaseTerm,
    /// An optional `Ed25519` signature value.
    pub signature: Option<Signature>,
}

impl ShardSnapshotParams {
    /// Creates parameters with the default page size and the identity codec.
    #[must_use]
    pub fn new(
        shard_id: ShardId,
        prefix_bounds: PrefixBounds,
        commit_version: CommitVersion,
        manifest_digest: ManifestDigest,
        map_version: MapVersion,
        map_digest: MapDigest,
        lease_term: LeaseTerm,
    ) -> Self {
        Self {
            shard_id,
            prefix_bounds,
            commit_version,
            manifest_digest,
            map_version,
            map_digest,
            key_encoding_version: KeyEncodingVersion::new(1),
            page_size: DEFAULT_PAGE_SIZE,
            codec: CodecId::IDENTITY,
            lease_term,
            signature: None,
        }
    }
}

/// The parameters of one `ScopeSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSnapshotParams {
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
    /// The policy version that produced the scope.
    pub policy_version: PolicyVersion,
    /// One commit version per shard in the scope.
    pub version_vector: ScopeVersionVector,
    /// The projection epoch of the needed facts.
    pub projection_epoch: ProjectionEpoch,
    /// One watermark per needed fact source.
    pub projection_watermarks: ProjectionWatermarks,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The shard map digest.
    pub map_digest: MapDigest,
    /// The key encoding annex version.
    pub key_encoding_version: KeyEncodingVersion,
    /// The target page payload size in bytes.
    pub page_size: u64,
    /// The page codec identifier.
    pub codec: CodecId,
    /// The authority lease term.
    pub lease_term: LeaseTerm,
    /// An optional `Ed25519` signature value.
    pub signature: Option<Signature>,
}

impl ScopeSnapshotParams {
    /// Creates parameters with the default page size and the identity codec.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn with_defaults(
        scope_identity: ScopeIdentity,
        policy_version: PolicyVersion,
        version_vector: ScopeVersionVector,
        projection_epoch: ProjectionEpoch,
        projection_watermarks: ProjectionWatermarks,
        manifest_digest: ManifestDigest,
        map_version: MapVersion,
        map_digest: MapDigest,
        lease_term: LeaseTerm,
    ) -> Self {
        Self {
            scope_identity,
            policy_version,
            version_vector,
            projection_epoch,
            projection_watermarks,
            manifest_digest,
            map_version,
            map_digest,
            key_encoding_version: KeyEncodingVersion::new(1),
            page_size: DEFAULT_PAGE_SIZE,
            codec: CodecId::IDENTITY,
            lease_term,
            signature: None,
        }
    }
}

/// The records of one shard inside a `ScopeSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeShard {
    /// The shard identifier.
    pub shard_id: ShardId,
    /// The commit version of this shard.
    pub commit_version: CommitVersion,
    /// The admitted user rows and needed projection facts.
    pub records: Vec<Record>,
}

impl ScopeShard {
    /// Creates one scope shard input.
    #[must_use]
    pub fn new(shard_id: ShardId, commit_version: CommitVersion, records: Vec<Record>) -> Self {
        Self {
            shard_id,
            commit_version,
            records,
        }
    }
}

/// The verified content of one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotView {
    /// The parsed header.
    pub header: Header,
    /// The verified directory.
    pub directory: Directory,
    /// The verified pages in `page_sequence` order.
    pub pages: Vec<Page>,
}

impl SnapshotView {
    /// Returns every record in page order.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.pages.iter().flat_map(|page| page.records.iter())
    }

    /// Returns the content digest.
    #[must_use]
    pub const fn content_digest(&self) -> ContentDigest {
        self.header.content_digest
    }
}

/// A verified `ShardSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardSnapshot {
    view: SnapshotView,
}

impl ShardSnapshot {
    /// Builds one `ShardSnapshot` from ordered records.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::PageOutOfBounds`] when a key is outside the
    /// prefix bounds. Returns [`SnapshotError::CorruptPage`] for an unsorted
    /// key or a duplicate key. Returns [`SnapshotError::PageTooLarge`] for a
    /// length over its limit.
    pub fn build(
        params: &ShardSnapshotParams,
        records: &[Record],
    ) -> Result<Vec<u8>, SnapshotError> {
        for record in records {
            if !key_in_bounds(&record.key, &params.prefix_bounds) {
                return Err(SnapshotError::PageOutOfBounds {
                    page_id: PageId::from_bytes([0; 32]),
                    offset: 0,
                });
            }
        }
        let draft = HeaderDraft {
            key_encoding_version: params.key_encoding_version,
            identity: HeaderIdentity::Shard(ShardHeader {
                shard_id: params.shard_id.clone(),
                prefix_bounds: params.prefix_bounds.clone(),
                commit_version: params.commit_version,
            }),
            manifest_digest: params.manifest_digest,
            map_version: params.map_version,
            map_digest: params.map_digest,
            page_size: params.page_size,
            codec: params.codec,
            lease_term: params.lease_term,
            signature: params.signature,
        };
        build_snapshot(
            draft,
            vec![PageSource {
                shard_id: params.shard_id.clone(),
                commit_version: params.commit_version,
                records: records.to_vec(),
            }],
        )
    }

    /// Reads and verifies one `ShardSnapshot`.
    ///
    /// # Errors
    ///
    /// Returns a named [`SnapshotError`] from the header, directory, page,
    /// content digest, or content seal checks.
    pub fn read(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let view = read_snapshot(bytes)?;
        if !matches!(&view.header.identity, HeaderIdentity::Shard(_)) {
            return Err(SnapshotError::MalformedHeader {
                offset: 0,
                reason: "the artifact is not a ShardSnapshot",
            });
        }
        Ok(Self { view })
    }

    /// Returns the verified view.
    #[must_use]
    pub const fn view(&self) -> &SnapshotView {
        &self.view
    }

    /// Returns the parsed header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.view.header
    }

    /// Returns the verified pages.
    #[must_use]
    pub fn pages(&self) -> &[Page] {
        &self.view.pages
    }

    /// Returns the content digest.
    #[must_use]
    pub const fn content_digest(&self) -> ContentDigest {
        self.view.header.content_digest
    }

    /// Returns every record in page order.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.view.records()
    }
}

/// A verified `ScopeSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSnapshot {
    view: SnapshotView,
}

impl ScopeSnapshot {
    /// Builds one `ScopeSnapshot` from ordered shard inputs.
    ///
    /// The function sorts the shard inputs by shard identifier. It does not
    /// sort the records inside one shard. It rejects every system key except a
    /// projection fact.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::ForbiddenNamespace`] for a forbidden system
    /// namespace. Returns [`SnapshotError::CorruptPage`] for a shard outside
    /// the version vector or an unsorted key. Returns
    /// [`SnapshotError::PageTooLarge`] for a length over its limit.
    pub fn build(
        params: &ScopeSnapshotParams,
        shards: &[ScopeShard],
    ) -> Result<Vec<u8>, SnapshotError> {
        let mut sources = shards.to_vec();
        sources.sort_by(|left, right| left.shard_id.cmp(&right.shard_id));
        for pair in sources.windows(2) {
            if pair[0].shard_id == pair[1].shard_id {
                return Err(SnapshotError::DuplicateKey { offset: 0 });
            }
        }
        for source in &sources {
            match params.version_vector.get(&source.shard_id) {
                Some(commit_version) if commit_version == source.commit_version => {}
                _ => {
                    return Err(SnapshotError::CorruptPage {
                        page_id: PageId::from_bytes([0; 32]),
                        offset: 0,
                        reason: "the scope shard is not in the version vector",
                    });
                }
            }
            for record in &source.records {
                if let Some(id) = crate::scope::forbidden_namespace(&record.key) {
                    return Err(SnapshotError::ForbiddenNamespace { id });
                }
            }
        }
        let draft = HeaderDraft {
            key_encoding_version: params.key_encoding_version,
            identity: HeaderIdentity::Scope(ScopeHeader {
                scope_identity: params.scope_identity,
                policy_version: params.policy_version,
                version_vector: params.version_vector.clone(),
                projection_epoch: params.projection_epoch,
                projection_watermarks: params.projection_watermarks.clone(),
            }),
            manifest_digest: params.manifest_digest,
            map_version: params.map_version,
            map_digest: params.map_digest,
            page_size: params.page_size,
            codec: params.codec,
            lease_term: params.lease_term,
            signature: params.signature,
        };
        let page_sources = sources
            .into_iter()
            .map(|source| PageSource {
                shard_id: source.shard_id,
                commit_version: source.commit_version,
                records: source.records,
            })
            .collect();
        build_snapshot(draft, page_sources)
    }

    /// Reads and verifies one `ScopeSnapshot`.
    ///
    /// # Errors
    ///
    /// Returns a named [`SnapshotError`] from the header, directory, page,
    /// content digest, or content seal checks.
    pub fn read(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let view = read_snapshot(bytes)?;
        if !matches!(&view.header.identity, HeaderIdentity::Scope(_)) {
            return Err(SnapshotError::MalformedHeader {
                offset: 0,
                reason: "the artifact is not a ScopeSnapshot",
            });
        }
        Ok(Self { view })
    }

    /// Returns the verified view.
    #[must_use]
    pub const fn view(&self) -> &SnapshotView {
        &self.view
    }

    /// Returns the parsed header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.view.header
    }

    /// Returns the verified pages.
    #[must_use]
    pub fn pages(&self) -> &[Page] {
        &self.view.pages
    }

    /// Returns the content digest.
    #[must_use]
    pub const fn content_digest(&self) -> ContentDigest {
        self.view.header.content_digest
    }

    /// Returns every record in page order.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.view.records()
    }
}

/// Reads and fully verifies one snapshot of either artifact kind.
///
/// The function verifies the directory, then the content digest, then the
/// content seal, and then every page.
///
/// # Errors
///
/// Returns a named [`SnapshotError`] from the header, directory, page,
/// content digest, or content seal checks.
pub fn read_snapshot(bytes: &[u8]) -> Result<SnapshotView, SnapshotError> {
    let header = Header::parse(bytes)?;
    let directory_bytes = directory_slice(bytes, &header)?;
    let directory = Directory::decode(directory_bytes, header.directory_offset)?;
    let page_count =
        u64::try_from(directory.entries.len()).map_err(|_| SnapshotError::PageTooLarge {
            page_id: PageId::from_bytes([0; 32]),
            offset: header.directory_offset,
        })?;
    if page_count != header.page_count {
        return Err(SnapshotError::CorruptPage {
            page_id: PageId::from_bytes([0; 32]),
            offset: header.directory_offset,
            reason: "the directory page count does not match the header",
        });
    }
    let computed = content_digest_for(&directory);
    verify_content_digest(header.content_digest, computed, header.directory_offset)?;
    verify_seal(
        header.content_seal,
        computed,
        header.lease_term,
        header.directory_offset,
    )?;
    let pages = read_pages(bytes, &header, &directory)?;
    Ok(SnapshotView {
        header,
        directory,
        pages,
    })
}

/// Rewrites the content seal with a new lease term.
///
/// The content digest, the directory, and every page stay byte-identical. The
/// function verifies the content digest before it writes the new seal.
///
/// # Errors
///
/// Returns a named [`SnapshotError`] when the input is invalid or when the
/// header length changes.
pub fn reseal(
    bytes: &[u8],
    lease_term: LeaseTerm,
    signature: Option<Signature>,
) -> Result<Vec<u8>, SnapshotError> {
    let header = Header::parse(bytes)?;
    let directory_bytes = directory_slice(bytes, &header)?;
    let directory = Directory::decode(directory_bytes, header.directory_offset)?;
    let computed = content_digest_for(&directory);
    verify_content_digest(header.content_digest, computed, header.directory_offset)?;
    let mut resealed = header.clone();
    resealed.lease_term = lease_term;
    resealed.content_seal = ContentSeal::new(computed, lease_term, signature);
    let header_bytes = resealed.encode();
    if header_bytes.len() != header.text_length {
        return Err(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "the resealed header length changed",
        });
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&bytes[header.text_length..]);
    Ok(out)
}

#[allow(clippy::too_many_lines)]
fn read_pages(
    bytes: &[u8],
    header: &Header,
    directory: &Directory,
) -> Result<Vec<Page>, SnapshotError> {
    let header_len = u64::try_from(header.text_length).unwrap_or(u64::MAX);
    let directory_offset = header.directory_offset;
    let mut pages = Vec::with_capacity(directory.entries.len());
    let mut previous_sequence: Option<PageSequence> = None;
    for entry in &directory.entries {
        if let Some(previous) = previous_sequence
            && entry.page_sequence <= previous
        {
            return Err(SnapshotError::CorruptPage {
                page_id: entry.page_id,
                offset: entry.offset,
                reason: "the page sequence is not strictly increasing",
            });
        }
        previous_sequence = Some(entry.page_sequence);
        let start = usize::try_from(entry.offset).map_err(|_| SnapshotError::CorruptPage {
            page_id: entry.page_id,
            offset: entry.offset,
            reason: "the page offset is too large",
        })?;
        let length = usize::try_from(entry.length).map_err(|_| SnapshotError::PageTooLarge {
            page_id: entry.page_id,
            offset: entry.offset,
        })?;
        let end = start
            .checked_add(length)
            .ok_or(SnapshotError::CorruptPage {
                page_id: entry.page_id,
                offset: entry.offset,
                reason: "the page range overflows",
            })?;
        if end > bytes.len() {
            return Err(SnapshotError::TruncatedSnapshot {
                offset: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            });
        }
        if entry.offset < header_len || u64::try_from(end).unwrap_or(u64::MAX) > directory_offset {
            return Err(SnapshotError::CorruptPage {
                page_id: entry.page_id,
                offset: entry.offset,
                reason: "the page is outside the body",
            });
        }
        let page = Page::decode(&bytes[start..end], entry.offset)?;
        if page.page_id != entry.page_id
            || page.page_sequence != entry.page_sequence
            || page.first_key != entry.first_key
            || page.last_key != entry.last_key
            || page.integrity != entry.integrity
        {
            return Err(SnapshotError::CorruptPage {
                page_id: entry.page_id,
                offset: entry.offset,
                reason: "the directory entry does not match the page",
            });
        }
        if page.codec != header.codec {
            return Err(SnapshotError::CorruptPage {
                page_id: entry.page_id,
                offset: entry.offset,
                reason: "the page codec does not match the header",
            });
        }
        match &header.identity {
            HeaderIdentity::Shard(shard) => {
                if page.shard_id != shard.shard_id || page.commit_version != shard.commit_version {
                    return Err(SnapshotError::CorruptPage {
                        page_id: entry.page_id,
                        offset: entry.offset,
                        reason: "the page identity does not match the shard",
                    });
                }
                for record in &page.records {
                    if !key_in_bounds(&record.key, &shard.prefix_bounds) {
                        return Err(SnapshotError::PageOutOfBounds {
                            page_id: entry.page_id,
                            offset: entry.offset,
                        });
                    }
                }
            }
            HeaderIdentity::Scope(scope) => {
                if scope.version_vector.get(&page.shard_id) != Some(page.commit_version) {
                    return Err(SnapshotError::CorruptPage {
                        page_id: entry.page_id,
                        offset: entry.offset,
                        reason: "the page commit version is not in the scope version vector",
                    });
                }
                for record in &page.records {
                    if let Some(id) = crate::scope::forbidden_namespace(&record.key) {
                        return Err(SnapshotError::ForbiddenNamespace { id });
                    }
                }
            }
        }
        pages.push(page);
    }
    Ok(pages)
}

fn directory_slice<'a>(bytes: &'a [u8], header: &Header) -> Result<&'a [u8], SnapshotError> {
    let start =
        usize::try_from(header.directory_offset).map_err(|_| SnapshotError::MalformedHeader {
            offset: header.directory_offset,
            reason: "the directory offset is too large",
        })?;
    let length =
        usize::try_from(header.directory_length).map_err(|_| SnapshotError::PageTooLarge {
            page_id: PageId::from_bytes([0; 32]),
            offset: header.directory_offset,
        })?;
    let end = start
        .checked_add(length)
        .ok_or(SnapshotError::MalformedHeader {
            offset: header.directory_offset,
            reason: "the directory range overflows",
        })?;
    if end > bytes.len() {
        return Err(SnapshotError::TruncatedSnapshot {
            offset: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        });
    }
    Ok(&bytes[start..end])
}

fn key_in_bounds(key: &[u8], bounds: &PrefixBounds) -> bool {
    let lower = bounds.lower();
    let upper = bounds.upper();
    let above_lower = lower.is_empty() || key >= lower;
    let below_upper = upper.is_empty() || key < upper;
    above_lower && below_upper
}

struct HeaderDraft {
    key_encoding_version: KeyEncodingVersion,
    identity: HeaderIdentity,
    manifest_digest: ManifestDigest,
    map_version: MapVersion,
    map_digest: MapDigest,
    page_size: u64,
    codec: CodecId,
    lease_term: LeaseTerm,
    signature: Option<Signature>,
}

struct PageSource {
    shard_id: ShardId,
    commit_version: CommitVersion,
    records: Vec<Record>,
}

#[allow(clippy::too_many_lines)]
fn build_snapshot(draft: HeaderDraft, sources: Vec<PageSource>) -> Result<Vec<u8>, SnapshotError> {
    if draft.page_size == 0 || draft.page_size > MAX_PAGE_SIZE {
        return Err(SnapshotError::PageTooLarge {
            page_id: PageId::from_bytes([0; 32]),
            offset: 0,
        });
    }
    if !draft.codec.is_identity() {
        return Err(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "the page codec is unknown",
        });
    }
    let mut pages = Vec::new();
    let mut sequence = 1_u64;
    for source in sources {
        validate_records(&source.records)?;
        for range in split_ranges(&source.records, draft.page_size)? {
            let page = Page::new(
                PageSequence::new(sequence),
                source.shard_id.clone(),
                source.commit_version,
                source.records[range].to_vec(),
                draft.codec,
            )?;
            pages.push(page);
            sequence = sequence.checked_add(1).ok_or(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            })?;
        }
    }
    let page_count = u64::try_from(pages.len()).map_err(|_| SnapshotError::PageTooLarge {
        page_id: PageId::from_bytes([0; 32]),
        offset: 0,
    })?;
    let mut header = Header {
        format_version: SnapshotPageVersion::new(FORMAT_MAJOR, FORMAT_MINOR),
        key_encoding_version: draft.key_encoding_version,
        required_features: KNOWN_REQUIRED_FEATURES,
        manifest_digest: draft.manifest_digest,
        map_version: draft.map_version,
        map_digest: draft.map_digest,
        identity: draft.identity,
        page_size: draft.page_size,
        page_count,
        codec: draft.codec,
        directory_codec: CodecId::IDENTITY,
        directory_offset: 0,
        directory_length: 0,
        content_digest: ContentDigest::from_bytes([0; 32]),
        content_seal: ContentSeal::new(
            ContentDigest::from_bytes([0; 32]),
            draft.lease_term,
            draft.signature,
        ),
        lease_term: draft.lease_term,
        text_length: 0,
    };
    let header_len = header.encoded_len();
    let header_len_u64 = u64::try_from(header_len).map_err(|_| SnapshotError::PageTooLarge {
        page_id: PageId::from_bytes([0; 32]),
        offset: 0,
    })?;
    let mut body = Vec::new();
    let mut entries = Vec::with_capacity(pages.len());
    let mut offset = header_len_u64;
    for page in &pages {
        let page_bytes = page.encode()?;
        let length = u64::try_from(page_bytes.len()).map_err(|_| SnapshotError::PageTooLarge {
            page_id: page.page_id,
            offset,
        })?;
        entries.push(DirectoryEntry {
            page_id: page.page_id,
            page_sequence: page.page_sequence,
            offset,
            length,
            first_key: page.first_key.clone(),
            last_key: page.last_key.clone(),
            integrity: page.integrity,
        });
        body.extend_from_slice(&page_bytes);
        offset = offset
            .checked_add(length)
            .ok_or(SnapshotError::PageTooLarge {
                page_id: page.page_id,
                offset,
            })?;
    }
    let directory = Directory::new(entries)?;
    let directory_bytes = directory.encode()?;
    let directory_length =
        u64::try_from(directory_bytes.len()).map_err(|_| SnapshotError::PageTooLarge {
            page_id: PageId::from_bytes([0; 32]),
            offset,
        })?;
    let content_digest = content_digest_for(&directory);
    header.directory_offset = offset;
    header.directory_length = directory_length;
    header.content_digest = content_digest;
    header.content_seal = ContentSeal::new(content_digest, draft.lease_term, draft.signature);
    let header_bytes = header.encode();
    if header_bytes.len() != header_len {
        return Err(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "the header length is not stable",
        });
    }
    let mut out = Vec::with_capacity(header_bytes.len() + body.len() + directory_bytes.len());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&body);
    out.extend_from_slice(&directory_bytes);
    Ok(out)
}

fn validate_records(records: &[Record]) -> Result<(), SnapshotError> {
    let mut previous: Option<&[u8]> = None;
    for record in records {
        if record.key.len() > MAX_KEY_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        if record.value.len() > MAX_VALUE_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        if let Some(previous) = previous {
            match record.key.as_slice().cmp(previous) {
                Ordering::Less => {
                    return Err(SnapshotError::CorruptPage {
                        page_id: PageId::from_bytes([0; 32]),
                        offset: 0,
                        reason: "the page keys are not sorted",
                    });
                }
                Ordering::Equal => return Err(SnapshotError::DuplicateKey { offset: 0 }),
                Ordering::Greater => {}
            }
        }
        previous = Some(&record.key);
    }
    Ok(())
}

fn split_ranges(
    records: &[Record],
    page_size: u64,
) -> Result<Vec<core::ops::Range<usize>>, SnapshotError> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut size = 0_u64;
    for (index, record) in records.iter().enumerate() {
        let key_len = u64::try_from(record.key.len()).unwrap_or(u64::MAX);
        let value_len = u64::try_from(record.value.len()).unwrap_or(u64::MAX);
        let record_size = key_len.saturating_add(value_len).saturating_add(8);
        if record_size > page_size {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        if index > start && size.saturating_add(record_size) > page_size {
            ranges.push(start..index);
            start = index;
            size = 0;
        }
        size = size.saturating_add(record_size);
    }
    if start < records.len() {
        ranges.push(start..records.len());
    }
    Ok(ranges)
}
