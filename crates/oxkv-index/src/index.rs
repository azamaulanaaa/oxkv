//! The per-shard inverted index and its write transaction.
//!
//! A write stages every posting in one transaction. The transaction validates
//! the token budgets, the flags, the key bounds, and the overhead budget. It
//! applies a delta segment only when every check passes. A failed commit leaves
//! the index unchanged.
//!
//! The index stores immutable segments. A search at a pin reads the segments at
//! or below the pin and applies the tombstones at or below the pin. Compaction
//! merges the visible documents into one segment and retains the compacted
//! version only.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::version::{CommitVersion, ContentDigest, Hash32, PrefixBounds};

use crate::canonical::{self, Reader};
use crate::error::IndexError;
use crate::manifest::IndexManifest;
use crate::posting::{
    DocumentField, IndexDocument, Posting, PostingFlags, StoredDocument, StoredField, TermFrequency,
};
use crate::segment::Segment;
use crate::tokenizer::tokenize_field;

/// The index magic bytes.
const MAGIC: &[u8; 4] = b"OXI1";

/// The index format version.
const FORMAT_VERSION: u16 = 1;

/// The result of a committed index write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitOutcome {
    /// The commit version of the new segment.
    pub commit_version: CommitVersion,
    /// The number of documents in the new segment.
    pub document_count: u32,
    /// The number of tombstones in the new segment.
    pub delete_count: u32,
    /// The number of postings in the new segment.
    pub posting_count: u32,
    /// The index size after the commit.
    pub overhead_bytes: u64,
    /// The index digest after the commit.
    pub digest: ContentDigest,
    /// True when the index changed.
    pub changed: bool,
}

/// A per-table inverted index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardIndex {
    manifest: IndexManifest,
    bounds: PrefixBounds,
    segments: Vec<Segment>,
    commit_version: CommitVersion,
    oldest_retained: CommitVersion,
    digest: ContentDigest,
}

impl ShardIndex {
    /// Creates an empty index for one shard prefix range.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidManifest`] when the shard bounds are empty.
    pub fn new(manifest: IndexManifest, bounds: PrefixBounds) -> Result<Self, IndexError> {
        if bounds.is_empty() {
            return Err(IndexError::InvalidManifest {
                reason: "the shard bounds are empty".to_owned(),
            });
        }
        Self::from_parts(
            manifest,
            bounds,
            Vec::new(),
            CommitVersion::new(0),
            CommitVersion::new(0),
        )
    }

    /// Creates an empty index at one commit version.
    ///
    /// Use this constructor when a replica starts from a scope snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidManifest`] when the shard bounds are empty
    /// or the version is not greater than zero.
    pub fn empty_at(
        manifest: IndexManifest,
        bounds: PrefixBounds,
        commit_version: CommitVersion,
    ) -> Result<Self, IndexError> {
        if bounds.is_empty() {
            return Err(IndexError::InvalidManifest {
                reason: "the shard bounds are empty".to_owned(),
            });
        }
        Self::from_parts(manifest, bounds, Vec::new(), commit_version, commit_version)
    }

    pub(crate) fn from_parts(
        manifest: IndexManifest,
        bounds: PrefixBounds,
        segments: Vec<Segment>,
        commit_version: CommitVersion,
        oldest_retained: CommitVersion,
    ) -> Result<Self, IndexError> {
        if oldest_retained > commit_version {
            return Err(IndexError::InvalidManifest {
                reason: "the oldest retained version is above the commit version".to_owned(),
            });
        }
        if bounds.is_empty() {
            return Err(IndexError::InvalidManifest {
                reason: "the shard bounds are empty".to_owned(),
            });
        }
        let mut previous: Option<CommitVersion> = None;
        for segment in &segments {
            if segment.commit_version() < oldest_retained
                || segment.commit_version() > commit_version
            {
                return Err(IndexError::IndexMismatch);
            }
            if previous.is_some_and(|version| version >= segment.commit_version()) {
                return Err(IndexError::IndexMismatch);
            }
            previous = Some(segment.commit_version());
        }
        let mut index = Self {
            manifest,
            bounds,
            segments,
            commit_version,
            oldest_retained,
            digest: ContentDigest::from_bytes([0; 32]),
        };
        index.recompute_digest();
        Ok(index)
    }

    /// Returns the index manifest.
    #[must_use]
    pub const fn manifest(&self) -> &IndexManifest {
        &self.manifest
    }

    /// Returns the shard prefix range.
    #[must_use]
    pub const fn bounds(&self) -> &PrefixBounds {
        &self.bounds
    }

    /// Returns the current commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the oldest retained commit version.
    #[must_use]
    pub const fn oldest_retained(&self) -> CommitVersion {
        self.oldest_retained
    }

    /// Returns the immutable segments in version order.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Returns the index digest.
    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }

    /// Returns the index size in bytes.
    #[must_use]
    pub fn overhead_bytes(&self) -> u64 {
        u64::try_from(self.encode().len()).unwrap_or(u64::MAX)
    }

    /// Starts one write transaction at a commit version.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::VersionNotMonotonic`] when the version does not
    /// advance the current version.
    pub fn begin_write(
        &mut self,
        commit_version: CommitVersion,
    ) -> Result<WriteTransaction<'_>, IndexError> {
        if commit_version <= self.commit_version {
            return Err(IndexError::VersionNotMonotonic {
                current: self.commit_version,
                requested: commit_version,
            });
        }
        Ok(WriteTransaction {
            base: self,
            commit_version,
            operations: BTreeMap::new(),
        })
    }

    /// Returns the visible document map at one pin.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::PinUnavailable`] when the pin is outside the
    /// retained range.
    pub fn documents_at(
        &self,
        pin: CommitVersion,
    ) -> Result<BTreeMap<Vec<u8>, &StoredDocument>, IndexError> {
        self.check_pin(pin)?;
        let mut visible: BTreeMap<Vec<u8>, &StoredDocument> = BTreeMap::new();
        for segment in &self.segments {
            if segment.commit_version() > pin {
                continue;
            }
            for (key, document) in segment.documents() {
                visible.insert(key.clone(), document);
            }
            for key in segment.deletes() {
                visible.remove(key);
            }
        }
        Ok(visible)
    }

    pub(crate) fn visible_state(
        &self,
        pin: CommitVersion,
    ) -> Result<BTreeMap<Vec<u8>, (CommitVersion, &StoredDocument)>, IndexError> {
        self.check_pin(pin)?;
        let mut visible: BTreeMap<Vec<u8>, (CommitVersion, &StoredDocument)> = BTreeMap::new();
        for segment in &self.segments {
            if segment.commit_version() > pin {
                continue;
            }
            for (key, document) in segment.documents() {
                visible.insert(key.clone(), (segment.commit_version(), document));
            }
            for key in segment.deletes() {
                visible.remove(key);
            }
        }
        Ok(visible)
    }

    fn check_pin(&self, pin: CommitVersion) -> Result<(), IndexError> {
        if pin < self.oldest_retained || pin > self.commit_version {
            return Err(IndexError::PinUnavailable {
                requested: pin,
                current: self.commit_version,
                oldest: self.oldest_retained,
            });
        }
        Ok(())
    }

    /// Returns every posting of one field and term, in segment order.
    #[must_use]
    pub fn postings_for(&self, field: &str, term: &str) -> Vec<&Posting> {
        let key = (field.to_owned(), term.to_owned());
        let mut postings = Vec::new();
        for segment in &self.segments {
            if let Some(list) = segment.postings().get(&key) {
                postings.extend(list);
            }
        }
        postings
    }

    /// Returns the number of visible documents that contain a term at one pin.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::PinUnavailable`] when the pin is outside the
    /// retained range.
    pub fn term_document_frequency(
        &self,
        field: &str,
        term: &str,
        pin: CommitVersion,
    ) -> Result<u32, IndexError> {
        let visible = self.visible_state(pin)?;
        let postings = self.postings_for(field, term);
        let mut frequency = 0_u32;
        for posting in postings {
            if visible
                .get(posting.document())
                .is_some_and(|(version, _)| *version == posting.commit_version())
            {
                frequency = frequency.saturating_add(1);
            }
        }
        Ok(frequency)
    }

    /// Merges every segment into one segment at the current version.
    ///
    /// The compacted index retains the current version only. A pin below the
    /// current version returns [`IndexError::PinUnavailable`].
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::OverheadExceeded`] when the merged segment would
    /// exceed the manifest budget. The index is unchanged on error.
    pub fn compact(&mut self) -> Result<CommitOutcome, IndexError> {
        let visible = self.visible_state(self.commit_version)?;
        let documents: Vec<StoredDocument> = visible
            .values()
            .map(|(_, document)| (*document).clone())
            .collect();
        let segment = Segment::new(
            self.commit_version,
            &self.manifest,
            documents,
            BTreeSet::new(),
        )?;
        let document_count = u32::try_from(segment.documents().len()).unwrap_or(u32::MAX);
        let posting_count = segment
            .postings()
            .values()
            .try_fold(0_u32, |total, list| {
                u32::try_from(list.len())
                    .ok()
                    .and_then(|count| total.checked_add(count))
            })
            .ok_or(IndexError::IndexMismatch)?;
        let prospective = Self::from_parts(
            self.manifest.clone(),
            self.bounds.clone(),
            vec![segment],
            self.commit_version,
            self.commit_version,
        )?;
        self.check_overhead(&prospective)?;
        let digest = prospective.digest;
        *self = prospective;
        Ok(CommitOutcome {
            commit_version: self.commit_version,
            document_count,
            delete_count: 0,
            posting_count,
            overhead_bytes: self.overhead_bytes(),
            digest,
            changed: true,
        })
    }

    /// Encodes the index, its segments, and the index digest.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.encode_body();
        bytes.extend_from_slice(self.digest.as_bytes());
        bytes
    }

    fn encode_body(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        canonical::u16(&mut output, FORMAT_VERSION);
        output.extend_from_slice(self.manifest.fingerprint().as_bytes());
        canonical::u64(&mut output, self.commit_version.get());
        canonical::u64(&mut output, self.oldest_retained.get());
        canonical::u64(
            &mut output,
            u64::try_from(self.segments.len()).unwrap_or(u64::MAX),
        );
        for segment in &self.segments {
            let bytes = segment.encode();
            canonical::bytes(&mut output, &bytes);
        }
        output
    }

    fn recompute_digest(&mut self) {
        self.digest = ContentDigest::of_bytes(&self.encode_body());
    }

    /// Decodes an index and verifies the index digest and every checksum.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::IndexMismatch`] when a byte does not match the
    /// canonical encoding or a checksum. Returns
    /// [`IndexError::TokenizerMismatch`] when a segment tokenizer does not
    /// match the manifest. Returns [`IndexError::ManifestDrift`] when the
    /// manifest fingerprint changed.
    pub fn decode(
        bytes: &[u8],
        manifest: IndexManifest,
        bounds: PrefixBounds,
    ) -> Result<Self, IndexError> {
        if bytes.len() < 32 {
            return Err(IndexError::IndexMismatch);
        }
        let (body, digest) = bytes.split_at(bytes.len() - 32);
        if ContentDigest::of_bytes(body).as_bytes().as_slice() != digest {
            return Err(IndexError::IndexMismatch);
        }
        let mut reader = Reader::new(body);
        if reader.take(4)? != MAGIC {
            return Err(IndexError::IndexMismatch);
        }
        let version = reader.u16()?;
        if version != FORMAT_VERSION {
            return Err(IndexError::UnsupportedVersion {
                expected: FORMAT_VERSION,
                found: version,
            });
        }
        let schema_digest = Hash32::from_bytes(
            reader
                .take(32)?
                .try_into()
                .map_err(|_| IndexError::IndexMismatch)?,
        );
        if schema_digest != manifest.fingerprint() {
            return Err(IndexError::ManifestDrift);
        }
        let commit_version = CommitVersion::new(reader.u64()?);
        let oldest_retained = CommitVersion::new(reader.u64()?);
        let segment_count = reader.u64()?;
        let mut segments = Vec::new();
        for _ in 0..segment_count {
            let segment_bytes = reader.bytes()?;
            segments.push(Segment::decode(segment_bytes, &manifest)?);
        }
        reader.finish()?;
        let index = Self::from_parts(manifest, bounds, segments, commit_version, oldest_retained)?;
        index.verify()?;
        if index.encode().as_slice() != bytes {
            return Err(IndexError::IndexMismatch);
        }
        Ok(index)
    }

    /// Verifies every segment checksum and the index digest.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::IndexMismatch`] when a checksum or the digest does
    /// not match the current bytes.
    pub fn verify(&self) -> Result<(), IndexError> {
        for segment in &self.segments {
            segment.verify()?;
        }
        if ContentDigest::of_bytes(&self.encode_body()) == self.digest {
            Ok(())
        } else {
            Err(IndexError::IndexMismatch)
        }
    }

    fn check_overhead(&self, prospective: &Self) -> Result<(), IndexError> {
        let used = prospective.overhead_bytes();
        let budget = self.manifest.overhead_budget();
        if used > budget {
            return Err(IndexError::OverheadExceeded { used, budget });
        }
        Ok(())
    }

    pub(crate) fn set_oldest_retained(
        &mut self,
        oldest_retained: CommitVersion,
    ) -> Result<(), IndexError> {
        if oldest_retained > self.commit_version {
            return Err(IndexError::InvalidManifest {
                reason: "the oldest retained version is above the commit version".to_owned(),
            });
        }
        self.oldest_retained = oldest_retained;
        self.recompute_digest();
        Ok(())
    }
}

/// One staged write operation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Operation {
    Put(StoredDocument),
    Delete(Vec<u8>),
}

/// One atomic index write.
///
/// A transaction borrows the index mutably. Dropping the transaction discards
/// every staged operation. [`WriteTransaction::commit`] applies the operations
/// only after every check passes.
pub struct WriteTransaction<'a> {
    base: &'a mut ShardIndex,
    commit_version: CommitVersion,
    operations: BTreeMap<Vec<u8>, Operation>,
}

impl WriteTransaction<'_> {
    /// Returns the commit version of this transaction.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the number of staged keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Returns true when no key is staged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Stages one document.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::OutOfScope`] for a key outside the shard bounds.
    /// Returns [`IndexError::UnknownField`] for an undeclared field. Returns
    /// [`IndexError::UnknownFlag`] for an undeclared flag. Returns
    /// [`IndexError::TokenCountExceeded`] or
    /// [`IndexError::TokenLengthExceeded`] when a field exceeds the token
    /// budget.
    pub fn put(&mut self, document: &IndexDocument) -> Result<(), IndexError> {
        let stored = prepare_document(&self.base.manifest, &self.base.bounds, document)?;
        self.operations
            .insert(stored.key().to_vec(), Operation::Put(stored));
        Ok(())
    }

    /// Stages one delete.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::OutOfScope`] for a key outside the shard bounds.
    pub fn delete(&mut self, key: &[u8]) -> Result<(), IndexError> {
        if !key_in_bounds(&self.base.bounds, key) {
            return Err(IndexError::OutOfScope { key: key.to_vec() });
        }
        self.operations
            .insert(key.to_vec(), Operation::Delete(key.to_vec()));
        Ok(())
    }

    /// Commits the staged operations.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::EmptyWrite`] when no key is staged. Returns
    /// [`IndexError::OverheadExceeded`] when the new index would exceed the
    /// manifest budget. The index is unchanged on error.
    pub fn commit(self) -> Result<CommitOutcome, IndexError> {
        if self.operations.is_empty() {
            return Err(IndexError::EmptyWrite);
        }
        let mut documents = Vec::new();
        let mut deletes = BTreeSet::new();
        for operation in self.operations.into_values() {
            match operation {
                Operation::Put(document) => documents.push(document),
                Operation::Delete(key) => {
                    deletes.insert(key);
                }
            }
        }
        let segment = Segment::new(self.commit_version, &self.base.manifest, documents, deletes)?;
        let document_count = u32::try_from(segment.documents().len()).unwrap_or(u32::MAX);
        let delete_count = u32::try_from(segment.deletes().len()).unwrap_or(u32::MAX);
        let posting_count = segment
            .postings()
            .values()
            .try_fold(0_u32, |total, list| {
                u32::try_from(list.len())
                    .ok()
                    .and_then(|count| total.checked_add(count))
            })
            .ok_or(IndexError::IndexMismatch)?;
        let mut segments = self.base.segments.clone();
        segments.push(segment);
        let prospective = ShardIndex::from_parts(
            self.base.manifest.clone(),
            self.base.bounds.clone(),
            segments,
            self.commit_version,
            self.base.oldest_retained,
        )?;
        self.base.check_overhead(&prospective)?;
        let digest = prospective.digest;
        *self.base = prospective;
        Ok(CommitOutcome {
            commit_version: self.commit_version,
            document_count,
            delete_count,
            posting_count,
            overhead_bytes: self.base.overhead_bytes(),
            digest,
            changed: true,
        })
    }
}

pub(crate) fn prepare_document(
    manifest: &IndexManifest,
    bounds: &PrefixBounds,
    document: &IndexDocument,
) -> Result<StoredDocument, IndexError> {
    let key = document.key();
    if key.is_empty() {
        return Err(IndexError::MalformedDocument {
            reason: "a document key is empty".to_owned(),
        });
    }
    if !key_in_bounds(bounds, key) {
        return Err(IndexError::OutOfScope { key: key.to_vec() });
    }
    let mut fields: BTreeMap<String, StoredField> = BTreeMap::new();
    for field in document.fields() {
        let stored = prepare_field(manifest, field)?;
        if fields.insert(field.field().to_owned(), stored).is_some() {
            return Err(IndexError::MalformedDocument {
                reason: format!("the field {} repeats", field.field()),
            });
        }
    }
    Ok(StoredDocument::new(key.to_vec(), fields))
}

fn prepare_field(
    manifest: &IndexManifest,
    field: &DocumentField,
) -> Result<StoredField, IndexError> {
    let name = field.field();
    let Some(spec) = manifest.field(name) else {
        return Err(IndexError::UnknownField {
            field: name.to_owned(),
        });
    };
    if !spec.indexed() {
        return Err(IndexError::UnknownField {
            field: name.to_owned(),
        });
    }
    let filter_flags = PostingFlags::new(field.filter_flags());
    spec.filter_flags().validate(filter_flags, name)?;
    let search_flags = PostingFlags::new(field.search_flags());
    spec.search_flags().validate(search_flags, name)?;
    let tokens = tokenize_field(
        name,
        spec.tokenizer(),
        field.text(),
        manifest.token_budget(),
    )?;
    let token_count = u32::try_from(tokens.len()).map_err(|_| IndexError::TokenCountExceeded {
        field: name.to_owned(),
        count: u64::try_from(tokens.len()).unwrap_or(u64::MAX),
        limit: u64::from(manifest.token_budget().max_tokens_per_field),
    })?;
    let mut terms: BTreeMap<String, TermFrequency> = BTreeMap::new();
    for token in &tokens {
        let entry = terms
            .entry(token.text().to_owned())
            .or_insert_with(|| TermFrequency::new(0, Vec::new()));
        let frequency = entry.frequency().saturating_add(1);
        let mut positions = entry.positions().to_vec();
        if spec.positions() {
            positions.push(token.position());
        }
        *entry = TermFrequency::new(frequency, positions);
    }
    Ok(StoredField::new(
        token_count,
        filter_flags,
        search_flags,
        terms,
    ))
}

pub(crate) fn key_in_bounds(bounds: &PrefixBounds, key: &[u8]) -> bool {
    key >= bounds.lower() && key < bounds.upper()
}
