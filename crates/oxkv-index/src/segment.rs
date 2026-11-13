//! Immutable index segments.
//!
//! A segment holds per-field postings, a document map, a tombstone set, and the
//! tokenizer identity of every indexed field. The segment checksum covers the
//! canonical body. A reader verifies the checksum before it uses the segment.
//!
//! A delta segment carries the puts and deletes of one commit. A compacted
//! segment carries only the latest document per key.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::version::{CommitVersion, ContentDigest, Hash32};

use crate::canonical::{self, Reader};
use crate::error::IndexError;
use crate::manifest::IndexManifest;
use crate::posting::{Posting, PostingFlags, StoredDocument, StoredField, TermFrequency};
use crate::tokenizer::TokenizerIdentity;

/// The segment magic bytes.
const MAGIC: &[u8; 4] = b"OXS1";

/// The segment format version.
const FORMAT_VERSION: u16 = 1;

/// One immutable index segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    commit_version: CommitVersion,
    schema_digest: Hash32,
    tokenizers: BTreeMap<String, TokenizerIdentity>,
    positions: BTreeSet<String>,
    postings: BTreeMap<(String, String), Vec<Posting>>,
    documents: BTreeMap<Vec<u8>, StoredDocument>,
    deletes: BTreeSet<Vec<u8>>,
    checksum: ContentDigest,
}

impl Segment {
    /// Creates a segment from documents and deletes.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::MalformedDocument`] when a document repeats a key,
    /// uses an undeclared field, or has an invalid field value. Returns
    /// [`IndexError::InvalidManifest`] for a bad manifest.
    pub fn new(
        commit_version: CommitVersion,
        manifest: &IndexManifest,
        documents: Vec<StoredDocument>,
        deletes: BTreeSet<Vec<u8>>,
    ) -> Result<Self, IndexError> {
        let tokenizers = manifest
            .fields()
            .iter()
            .filter(|field| field.indexed())
            .map(|field| (field.name().to_owned(), field.tokenizer()))
            .collect();
        let positions = manifest
            .fields()
            .iter()
            .filter(|field| field.indexed() && field.positions())
            .map(|field| field.name().to_owned())
            .collect();
        let mut document_map: BTreeMap<Vec<u8>, StoredDocument> = BTreeMap::new();
        for document in documents {
            let key = document.key().to_vec();
            if key.is_empty() {
                return Err(IndexError::MalformedDocument {
                    reason: "a document key is empty".to_owned(),
                });
            }
            if document_map.insert(key.clone(), document).is_some() {
                return Err(IndexError::MalformedDocument {
                    reason: format!("the document key {} repeats", display_key(&key)),
                });
            }
        }
        if deletes.iter().any(|key| document_map.contains_key(key)) {
            return Err(IndexError::MalformedDocument {
                reason: "a segment cannot put and delete the same key".to_owned(),
            });
        }
        let mut postings: BTreeMap<(String, String), Vec<Posting>> = BTreeMap::new();
        for (key, document) in &document_map {
            for (field_name, field) in document.fields() {
                let Some(spec) = manifest.field(field_name) else {
                    return Err(IndexError::MalformedDocument {
                        reason: format!("the field {field_name} is not declared"),
                    });
                };
                if !spec.indexed() {
                    return Err(IndexError::MalformedDocument {
                        reason: format!("the field {field_name} is not indexed"),
                    });
                }
                for (term, frequency) in field.terms() {
                    let posting = Posting::new(
                        key.clone(),
                        commit_version,
                        frequency.frequency(),
                        frequency.positions().to_vec(),
                        field.filter_flags(),
                        field.search_flags(),
                    );
                    postings
                        .entry((field_name.clone(), term.clone()))
                        .or_default()
                        .push(posting);
                }
            }
        }
        let mut segment = Self {
            commit_version,
            schema_digest: manifest.fingerprint(),
            tokenizers,
            positions,
            postings,
            documents: document_map,
            deletes,
            checksum: ContentDigest::from_bytes([0; 32]),
        };
        segment.validate(manifest)?;
        let checksum = ContentDigest::of_bytes(&segment.encode_body());
        segment.checksum = checksum;
        Ok(segment)
    }

    /// Returns the commit version of the segment.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the manifest fingerprint that the segment was built against.
    #[must_use]
    pub const fn schema_digest(&self) -> Hash32 {
        self.schema_digest
    }

    /// Returns the tokenizer identity of every indexed field.
    #[must_use]
    pub const fn tokenizers(&self) -> &BTreeMap<String, TokenizerIdentity> {
        &self.tokenizers
    }

    /// Returns the fields that store positions.
    #[must_use]
    pub const fn positions(&self) -> &BTreeSet<String> {
        &self.positions
    }

    /// Returns the postings in canonical `(field, term)` order.
    #[must_use]
    pub const fn postings(&self) -> &BTreeMap<(String, String), Vec<Posting>> {
        &self.postings
    }

    /// Returns the document map in canonical key order.
    #[must_use]
    pub const fn documents(&self) -> &BTreeMap<Vec<u8>, StoredDocument> {
        &self.documents
    }

    /// Returns the tombstone set in canonical key order.
    #[must_use]
    pub const fn deletes(&self) -> &BTreeSet<Vec<u8>> {
        &self.deletes
    }

    /// Returns the segment checksum.
    #[must_use]
    pub const fn checksum(&self) -> ContentDigest {
        self.checksum
    }

    /// Returns the encoded byte length of the segment.
    #[must_use]
    pub fn overhead_bytes(&self) -> u64 {
        u64::try_from(self.encode().len()).unwrap_or(u64::MAX)
    }

    /// Returns true when the segment has no document and no delete.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty() && self.deletes.is_empty()
    }

    /// Encodes the segment.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.encode_body();
        bytes.extend_from_slice(self.checksum.as_bytes());
        bytes
    }

    fn encode_body(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        canonical::u16(&mut output, FORMAT_VERSION);
        canonical::u64(&mut output, self.commit_version.get());
        output.extend_from_slice(self.schema_digest.as_bytes());
        encode_tokenizers(&mut output, &self.tokenizers);
        encode_positions(&mut output, &self.positions);
        canonical::u64(
            &mut output,
            u64::try_from(self.postings.len()).unwrap_or(u64::MAX),
        );
        for ((field, term), postings) in &self.postings {
            canonical::text(&mut output, field);
            canonical::text(&mut output, term);
            canonical::u64(
                &mut output,
                u64::try_from(postings.len()).unwrap_or(u64::MAX),
            );
            for posting in postings {
                canonical::bytes(&mut output, posting.document());
                canonical::u64(&mut output, posting.commit_version().get());
                canonical::u32(&mut output, posting.term_frequency());
                encode_positions_list(&mut output, posting.positions());
                canonical::u64(&mut output, posting.filter_flags().bits());
                canonical::u64(&mut output, posting.search_flags().bits());
            }
        }
        encode_documents(&mut output, &self.documents);
        canonical::u64(
            &mut output,
            u64::try_from(self.deletes.len()).unwrap_or(u64::MAX),
        );
        for key in &self.deletes {
            canonical::bytes(&mut output, key);
        }
        output
    }

    /// Decodes a segment and verifies its checksum.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::IndexMismatch`] when a byte does not match the
    /// canonical encoding or the checksum. Returns
    /// [`IndexError::TokenizerMismatch`] when the segment tokenizer does not
    /// match the manifest. Returns [`IndexError::TokenizerUnknown`] when the
    /// segment names an unknown tokenizer. Returns
    /// [`IndexError::UnsupportedVersion`] for an unknown segment version.
    pub fn decode(bytes: &[u8], manifest: &IndexManifest) -> Result<Self, IndexError> {
        if bytes.len() < 32 {
            return Err(IndexError::IndexMismatch);
        }
        let (body, checksum) = bytes.split_at(bytes.len() - 32);
        if ContentDigest::of_bytes(body).as_bytes().as_slice() != checksum {
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
        let commit_version = CommitVersion::new(reader.u64()?);
        let schema_digest = Hash32::from_bytes(
            reader
                .take(32)?
                .try_into()
                .map_err(|_| IndexError::IndexMismatch)?,
        );
        if schema_digest != manifest.fingerprint() {
            return Err(IndexError::ManifestDrift);
        }
        let tokenizers = decode_tokenizers(&mut reader)?;
        let positions = decode_positions(&mut reader)?;
        let postings = decode_postings(&mut reader, commit_version)?;
        let documents = decode_documents(&mut reader)?;
        let deletes = decode_deletes(&mut reader)?;
        reader.finish()?;
        let mut checksum_bytes = [0_u8; 32];
        checksum_bytes.copy_from_slice(checksum);
        let segment = Self {
            commit_version,
            schema_digest,
            tokenizers,
            positions,
            postings,
            documents,
            deletes,
            checksum: ContentDigest::from_bytes(checksum_bytes),
        };
        segment.validate(manifest)?;
        Ok(segment)
    }

    /// Verifies the segment checksum.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::IndexMismatch`] when the checksum does not cover
    /// the current body.
    pub fn verify(&self) -> Result<(), IndexError> {
        if ContentDigest::of_bytes(&self.encode_body()) == self.checksum {
            Ok(())
        } else {
            Err(IndexError::IndexMismatch)
        }
    }

    #[allow(clippy::too_many_lines)] // The checks are deliberately explicit.
    fn validate(&self, manifest: &IndexManifest) -> Result<(), IndexError> {
        if self.schema_digest != manifest.fingerprint() {
            return Err(IndexError::ManifestDrift);
        }
        for (field, tokenizer) in &self.tokenizers {
            let Some(spec) = manifest.field(field) else {
                return Err(IndexError::ManifestDrift);
            };
            if !spec.indexed() {
                return Err(IndexError::ManifestDrift);
            }
            if spec.tokenizer() != *tokenizer {
                return Err(IndexError::TokenizerMismatch {
                    expected: spec.tokenizer(),
                    found: *tokenizer,
                });
            }
        }
        for field in manifest.fields() {
            if !field.indexed() {
                continue;
            }
            let Some(tokenizer) = self.tokenizers.get(field.name()) else {
                return Err(IndexError::ManifestDrift);
            };
            if *tokenizer != field.tokenizer() {
                return Err(IndexError::TokenizerMismatch {
                    expected: field.tokenizer(),
                    found: *tokenizer,
                });
            }
            let has_positions = self.positions.contains(field.name());
            if has_positions != field.positions() {
                return Err(IndexError::ManifestDrift);
            }
        }
        if self
            .deletes
            .iter()
            .any(|key| self.documents.contains_key(key))
        {
            return Err(IndexError::IndexMismatch);
        }
        for (key, document) in &self.documents {
            if key.is_empty() || document.key() != key.as_slice() {
                return Err(IndexError::IndexMismatch);
            }
            for (field_name, field) in document.fields() {
                let Some(spec) = manifest.field(field_name) else {
                    return Err(IndexError::MalformedDocument {
                        reason: format!("the field {field_name} is not declared"),
                    });
                };
                if !spec.indexed() {
                    return Err(IndexError::MalformedDocument {
                        reason: format!("the field {field_name} is not indexed"),
                    });
                }
                let positions_enabled = self.positions.contains(field_name);
                let mut total = 0_u64;
                for (term, frequency) in field.terms() {
                    if term.is_empty() || frequency.frequency() == 0 {
                        return Err(IndexError::IndexMismatch);
                    }
                    if positions_enabled {
                        if u64::from(frequency.frequency())
                            != u64::try_from(frequency.positions().len()).unwrap_or(u64::MAX)
                        {
                            return Err(IndexError::IndexMismatch);
                        }
                    } else if !frequency.positions().is_empty() {
                        return Err(IndexError::IndexMismatch);
                    }
                    total += u64::from(frequency.frequency());
                    let Some(list) = self.postings.get(&(field_name.clone(), term.clone())) else {
                        return Err(IndexError::IndexMismatch);
                    };
                    let Ok(index) =
                        list.binary_search_by(|posting| posting.document().cmp(key.as_slice()))
                    else {
                        return Err(IndexError::IndexMismatch);
                    };
                    let posting = &list[index];
                    if posting.term_frequency() != frequency.frequency()
                        || posting.positions() != frequency.positions()
                        || posting.filter_flags() != field.filter_flags()
                        || posting.search_flags() != field.search_flags()
                        || posting.commit_version() != self.commit_version
                    {
                        return Err(IndexError::IndexMismatch);
                    }
                }
                if total != u64::from(field.token_count()) {
                    return Err(IndexError::IndexMismatch);
                }
            }
        }
        for ((field_name, term), list) in &self.postings {
            if term.is_empty() || list.is_empty() {
                return Err(IndexError::IndexMismatch);
            }
            if !self.tokenizers.contains_key(field_name) {
                return Err(IndexError::IndexMismatch);
            }
            for posting in list {
                let Some(document) = self.documents.get(posting.document()) else {
                    return Err(IndexError::IndexMismatch);
                };
                let Some(field) = document.fields().get(field_name) else {
                    return Err(IndexError::IndexMismatch);
                };
                let Some(frequency) = field.terms().get(term) else {
                    return Err(IndexError::IndexMismatch);
                };
                if posting.term_frequency() != frequency.frequency()
                    || posting.commit_version() != self.commit_version
                {
                    return Err(IndexError::IndexMismatch);
                }
            }
        }
        Ok(())
    }
}

fn display_key(key: &[u8]) -> String {
    use core::fmt::Write;
    let mut text = String::with_capacity(key.len() * 2);
    for byte in key {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn encode_tokenizers(output: &mut Vec<u8>, tokenizers: &BTreeMap<String, TokenizerIdentity>) {
    canonical::u64(output, u64::try_from(tokenizers.len()).unwrap_or(u64::MAX));
    for (field, tokenizer) in tokenizers {
        canonical::text(output, field);
        tokenizer.encode(output);
    }
}

fn encode_positions(output: &mut Vec<u8>, positions: &BTreeSet<String>) {
    canonical::u64(output, u64::try_from(positions.len()).unwrap_or(u64::MAX));
    for field in positions {
        canonical::text(output, field);
    }
}

fn encode_positions_list(output: &mut Vec<u8>, positions: &[u32]) {
    canonical::u64(output, u64::try_from(positions.len()).unwrap_or(u64::MAX));
    for position in positions {
        canonical::u32(output, *position);
    }
}

fn encode_documents(output: &mut Vec<u8>, documents: &BTreeMap<Vec<u8>, StoredDocument>) {
    canonical::u64(output, u64::try_from(documents.len()).unwrap_or(u64::MAX));
    for (key, document) in documents {
        canonical::bytes(output, key);
        canonical::u64(
            output,
            u64::try_from(document.fields().len()).unwrap_or(u64::MAX),
        );
        for (field_name, field) in document.fields() {
            canonical::text(output, field_name);
            canonical::u32(output, field.token_count());
            canonical::u64(output, field.filter_flags().bits());
            canonical::u64(output, field.search_flags().bits());
            canonical::u64(
                output,
                u64::try_from(field.terms().len()).unwrap_or(u64::MAX),
            );
            for (term, frequency) in field.terms() {
                canonical::text(output, term);
                canonical::u32(output, frequency.frequency());
                encode_positions_list(output, frequency.positions());
            }
        }
    }
}

fn decode_tokenizers(
    reader: &mut Reader<'_>,
) -> Result<BTreeMap<String, TokenizerIdentity>, IndexError> {
    let count = reader.u64()?;
    let mut tokenizers = BTreeMap::new();
    let mut previous: Option<String> = None;
    for _ in 0..count {
        let field = reader.text()?;
        if previous.as_ref().is_some_and(|value| value >= &field) {
            return Err(IndexError::IndexMismatch);
        }
        let id = reader.text()?;
        let version = reader.u16()?;
        let unicode_version = reader.text()?;
        let tokenizer = TokenizerIdentity::from_parts(&id, version, &unicode_version)?;
        tokenizers.insert(field.clone(), tokenizer);
        previous = Some(field);
    }
    Ok(tokenizers)
}

fn decode_positions(reader: &mut Reader<'_>) -> Result<BTreeSet<String>, IndexError> {
    let count = reader.u64()?;
    let mut positions = BTreeSet::new();
    let mut previous: Option<String> = None;
    for _ in 0..count {
        let field = reader.text()?;
        if previous.as_ref().is_some_and(|value| value >= &field) {
            return Err(IndexError::IndexMismatch);
        }
        positions.insert(field.clone());
        previous = Some(field);
    }
    Ok(positions)
}

fn decode_positions_list(reader: &mut Reader<'_>) -> Result<Vec<u32>, IndexError> {
    let count = reader.u64()?;
    let mut positions = Vec::new();
    let mut previous: Option<u32> = None;
    for _ in 0..count {
        let position = reader.u32()?;
        if previous.is_some_and(|value| value >= position) {
            return Err(IndexError::IndexMismatch);
        }
        positions.push(position);
        previous = Some(position);
    }
    Ok(positions)
}

fn decode_postings(
    reader: &mut Reader<'_>,
    commit_version: CommitVersion,
) -> Result<BTreeMap<(String, String), Vec<Posting>>, IndexError> {
    let count = reader.u64()?;
    let mut postings = BTreeMap::new();
    let mut previous: Option<(String, String)> = None;
    for _ in 0..count {
        let field = reader.text()?;
        let term = reader.text()?;
        let key = (field, term);
        if previous.as_ref().is_some_and(|value| value >= &key) {
            return Err(IndexError::IndexMismatch);
        }
        let posting_count = reader.u64()?;
        let mut list = Vec::new();
        let mut previous_document: Option<Vec<u8>> = None;
        for _ in 0..posting_count {
            let document = reader.bytes()?.to_vec();
            if previous_document
                .as_ref()
                .is_some_and(|value| value >= &document)
            {
                return Err(IndexError::IndexMismatch);
            }
            let version = CommitVersion::new(reader.u64()?);
            if version != commit_version {
                return Err(IndexError::IndexMismatch);
            }
            let term_frequency = reader.u32()?;
            let positions = decode_positions_list(reader)?;
            let filter_flags = PostingFlags::new(reader.u64()?);
            let search_flags = PostingFlags::new(reader.u64()?);
            list.push(Posting::new(
                document.clone(),
                version,
                term_frequency,
                positions,
                filter_flags,
                search_flags,
            ));
            previous_document = Some(document);
        }
        postings.insert(key.clone(), list);
        previous = Some(key);
    }
    Ok(postings)
}

fn decode_documents(
    reader: &mut Reader<'_>,
) -> Result<BTreeMap<Vec<u8>, StoredDocument>, IndexError> {
    let count = reader.u64()?;
    let mut documents = BTreeMap::new();
    let mut previous: Option<Vec<u8>> = None;
    for _ in 0..count {
        let key = reader.bytes()?.to_vec();
        if previous.as_ref().is_some_and(|value| value >= &key) {
            return Err(IndexError::IndexMismatch);
        }
        let field_count = reader.u64()?;
        let mut fields = BTreeMap::new();
        let mut previous_field: Option<String> = None;
        for _ in 0..field_count {
            let field_name = reader.text()?;
            if previous_field
                .as_ref()
                .is_some_and(|value| value >= &field_name)
            {
                return Err(IndexError::IndexMismatch);
            }
            let token_count = reader.u32()?;
            let filter_flags = PostingFlags::new(reader.u64()?);
            let search_flags = PostingFlags::new(reader.u64()?);
            let term_count = reader.u64()?;
            let mut terms = BTreeMap::new();
            let mut previous_term: Option<String> = None;
            for _ in 0..term_count {
                let term = reader.text()?;
                if previous_term.as_ref().is_some_and(|value| value >= &term) {
                    return Err(IndexError::IndexMismatch);
                }
                let frequency = reader.u32()?;
                let positions = decode_positions_list(reader)?;
                terms.insert(term.clone(), TermFrequency::new(frequency, positions));
                previous_term = Some(term);
            }
            fields.insert(
                field_name.clone(),
                StoredField::new(token_count, filter_flags, search_flags, terms),
            );
            previous_field = Some(field_name);
        }
        documents.insert(key.clone(), StoredDocument::new(key.clone(), fields));
        previous = Some(key);
    }
    Ok(documents)
}

fn decode_deletes(reader: &mut Reader<'_>) -> Result<BTreeSet<Vec<u8>>, IndexError> {
    let count = reader.u64()?;
    let mut deletes = BTreeSet::new();
    let mut previous: Option<Vec<u8>> = None;
    for _ in 0..count {
        let key = reader.bytes()?.to_vec();
        if previous.as_ref().is_some_and(|value| value >= &key) {
            return Err(IndexError::IndexMismatch);
        }
        deletes.insert(key.clone());
        previous = Some(key);
    }
    Ok(deletes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::FieldSpec;
    use crate::posting::FlagDecl;
    use crate::score::ScorerSpec;
    use crate::tokenizer::{TokenBudget, TokenizerId};

    fn manifest() -> IndexManifest {
        let field = FieldSpec::new("title", TokenizerIdentity::v1(TokenizerId::UnicodeWord))
            .with_positions(true)
            .with_filterable(true)
            .with_filter_flags(vec![
                FlagDecl::new("published", 0).expect("the flag is valid"),
            ])
            .expect("the flags are valid");
        IndexManifest::new(
            vec![field],
            TokenizerIdentity::unicode_word(),
            ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
            100_000,
            TokenBudget::DEFAULT,
        )
        .expect("the manifest is valid")
    }

    fn document(key: &[u8], text: &str) -> StoredDocument {
        let tokens = crate::tokenizer::tokenize_field(
            "title",
            TokenizerIdentity::unicode_word(),
            text,
            TokenBudget::DEFAULT,
        )
        .expect("the text fits the budget");
        let mut terms: BTreeMap<String, TermFrequency> = BTreeMap::new();
        for token in &tokens {
            terms
                .entry(token.text().to_owned())
                .or_insert_with(|| TermFrequency::new(0, Vec::new()));
            let frequency = terms.get_mut(token.text()).expect("the term exists");
            *frequency = TermFrequency::new(
                frequency.frequency() + 1,
                frequency
                    .positions()
                    .iter()
                    .copied()
                    .chain([token.position()])
                    .collect(),
            );
        }
        let field = StoredField::new(
            u32::try_from(tokens.len()).unwrap_or(u32::MAX),
            PostingFlags::new(1),
            PostingFlags::EMPTY,
            terms,
        );
        StoredDocument::new(key.to_vec(), BTreeMap::from([("title".to_owned(), field)]))
    }

    #[test]
    fn a_segment_round_trips() {
        let segment = Segment::new(
            CommitVersion::new(1),
            &manifest(),
            vec![document(b"a", "café world")],
            BTreeSet::new(),
        )
        .expect("the segment is valid");
        let bytes = segment.encode();
        assert_eq!(Segment::decode(&bytes, &manifest()), Ok(segment));
    }

    #[test]
    fn a_flipped_segment_byte_fails() {
        let segment = Segment::new(
            CommitVersion::new(1),
            &manifest(),
            vec![document(b"a", "café")],
            BTreeSet::new(),
        )
        .expect("the segment is valid");
        let mut bytes = segment.encode();
        bytes[10] ^= 0x01;
        assert_eq!(
            Segment::decode(&bytes, &manifest()),
            Err(IndexError::IndexMismatch)
        );
    }

    #[test]
    fn a_manifest_change_is_named() {
        let segment = Segment::new(
            CommitVersion::new(1),
            &manifest(),
            vec![document(b"a", "café")],
            BTreeSet::new(),
        )
        .expect("the segment is valid");
        let field = FieldSpec::new("title", TokenizerIdentity::v1(TokenizerId::AsciiWord))
            .with_positions(true)
            .with_filterable(true)
            .with_filter_flags(vec![
                FlagDecl::new("published", 0).expect("the flag is valid"),
            ])
            .expect("the flags are valid");
        let other = IndexManifest::new(
            vec![field],
            TokenizerIdentity::ascii_word(),
            ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
            100_000,
            TokenBudget::DEFAULT,
        )
        .expect("the manifest is valid");
        let error = Segment::decode(&segment.encode(), &other).expect_err("the manifest changed");
        assert_eq!(error.code(), "manifest.artifact_drift");
    }

    #[test]
    fn a_tokenizer_mismatch_is_named() {
        let mut segment = Segment::new(
            CommitVersion::new(1),
            &manifest(),
            vec![document(b"a", "café")],
            BTreeSet::new(),
        )
        .expect("the segment is valid");
        segment
            .tokenizers
            .insert("title".to_owned(), TokenizerIdentity::ascii_word());
        segment.checksum = ContentDigest::of_bytes(&segment.encode_body());
        let error =
            Segment::decode(&segment.encode(), &manifest()).expect_err("the tokenizer changed");
        assert_eq!(error.code(), "search.tokenizer_mismatch");
    }
}
