//! The local search index.
//!
//! The replica builds this index from its scope members only. It never mirrors
//! the authority per-shard index. A rebuild from the same members produces the
//! same bytes on native and on `wasm32`.
//!
//! Ranking uses document-local features only. It reads the term frequency and
//! the document length. It reads no global corpus statistic. The scorer uses
//! integer arithmetic only.
//!
//! The index records its tokenizer version and its index version. A tokenizer
//! mismatch returns `search.tokenizer_mismatch`. A digest mismatch returns
//! `search.index_mismatch`.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::version::{ContentDigest, Hash32, ScopeIdentity, TokenizerVersion};

use crate::codec::{Decoder, Encoder, as_u64};
use crate::error::ReplicaError;
use crate::mirror::RowEntry;
use crate::scope::Scope;

const MAGIC: [u8; 4] = *b"OXRI";
const DIGEST_LENGTH: usize = 32;

/// The replica index format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IndexVersion(u16);

/// The 1.0 replica index format version.
pub const INDEX_VERSION: IndexVersion = IndexVersion::new(1);

impl IndexVersion {
    /// Creates an index version.
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

/// The integer scorer parameters of one table.
///
/// The score is `term_frequency * term_weight * length_norm / (length_norm +
/// document_length)`. The numerator and the denominator use `u64` arithmetic
/// with saturation. No float enters the score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scorer {
    /// The term weight.
    pub term_weight: u64,
    /// The length normalization scale.
    pub length_norm: u64,
}

impl Scorer {
    /// The default manifest scorer.
    pub const DEFAULT: Self = Self {
        term_weight: 1_000,
        length_norm: 8,
    };

    /// Creates a scorer.
    #[must_use]
    pub const fn new(term_weight: u64, length_norm: u64) -> Self {
        Self {
            term_weight,
            length_norm,
        }
    }
}

impl Default for Scorer {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The header of one replica index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHeader {
    /// The index format version.
    pub index_version: IndexVersion,
    /// The tokenizer version.
    pub tokenizer_version: TokenizerVersion,
    /// The scope identity.
    pub scope_identity: ScopeIdentity,
    /// The document count.
    pub document_count: u64,
    /// The scorer parameters.
    pub scorer: Scorer,
    /// The digest of the index body.
    pub content_digest: ContentDigest,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The canonical key.
    pub key: Vec<u8>,
    /// The integer score.
    pub score: u64,
}

#[derive(Debug, Clone)]
struct Document {
    length: u64,
    tokens: BTreeMap<String, u32>,
}

/// The local replica search index.
#[derive(Debug, Clone)]
pub struct ReplicaIndex {
    header: IndexHeader,
    bytes: Vec<u8>,
    documents: BTreeMap<Vec<u8>, Document>,
    scorer: Scorer,
}

impl ReplicaIndex {
    /// Builds an index from the scope members.
    ///
    /// The build skips a row with a tombstone. It tokenizes the value as UTF-8
    /// with replacement and stores the token frequency. The row order does not
    /// change the bytes.
    #[must_use]
    pub fn build<'a>(
        scope: &Scope,
        rows: impl Iterator<Item = (&'a [u8], &'a RowEntry)>,
        scorer: Scorer,
    ) -> Self {
        let mut documents: BTreeMap<Vec<u8>, Document> = BTreeMap::new();
        for (key, entry) in rows {
            if entry.value.is_none() {
                continue;
            }
            if !scope.admits_row(key) {
                continue;
            }
            let text = String::from_utf8_lossy(entry.value.as_deref().unwrap_or_default());
            let tokens = tokenize(&text);
            if tokens.is_empty() {
                continue;
            }
            let mut frequencies: BTreeMap<String, u32> = BTreeMap::new();
            for token in &tokens {
                let count = frequencies.entry(token.clone()).or_insert(0);
                *count = count.saturating_add(1);
            }
            documents.insert(
                key.to_vec(),
                Document {
                    length: as_u64(tokens.len()),
                    tokens: frequencies,
                },
            );
        }
        Self::from_documents(
            scope.identity(),
            scope.tokenizer_version(),
            documents,
            scorer,
        )
    }

    fn from_documents(
        scope_identity: ScopeIdentity,
        tokenizer_version: TokenizerVersion,
        documents: BTreeMap<Vec<u8>, Document>,
        scorer: Scorer,
    ) -> Self {
        let body = encode_body(scope_identity, tokenizer_version, &documents, scorer);
        let content_digest = ContentDigest::of_bytes(&body);
        let mut bytes = body;
        bytes.extend_from_slice(content_digest.as_bytes());
        let header = IndexHeader {
            index_version: INDEX_VERSION,
            tokenizer_version,
            scope_identity,
            document_count: as_u64(documents.len()),
            scorer,
            content_digest,
        };
        Self {
            header,
            bytes,
            documents,
            scorer,
        }
    }

    /// Opens one encoded index and verifies its digest.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::IndexMismatch`] for a malformed index or a
    /// digest mismatch.
    pub fn open(bytes: &[u8]) -> Result<Self, ReplicaError> {
        if bytes.len() < MAGIC.len() + DIGEST_LENGTH {
            return Err(ReplicaError::IndexMismatch);
        }
        let body_end = bytes.len() - DIGEST_LENGTH;
        let body = &bytes[..body_end];
        let expected = Hash32::of(body);
        if expected.as_bytes().as_slice() != &bytes[body_end..] {
            return Err(ReplicaError::IndexMismatch);
        }
        let mut decoder = Decoder::new(body);
        if decoder.bytes() != Some(MAGIC.as_slice()) {
            return Err(ReplicaError::IndexMismatch);
        }
        let index_version = IndexVersion::new(decoder.u16().ok_or(ReplicaError::IndexMismatch)?);
        if index_version != INDEX_VERSION {
            return Err(ReplicaError::IndexMismatch);
        }
        let tokenizer_version =
            TokenizerVersion::new(decoder.u16().ok_or(ReplicaError::IndexMismatch)?);
        let scope_bytes = decoder.bytes().ok_or(ReplicaError::IndexMismatch)?;
        let scope_identity = ScopeIdentity::from_bytes(
            scope_bytes
                .try_into()
                .map_err(|_| ReplicaError::IndexMismatch)?,
        );
        let document_count = decoder.u64().ok_or(ReplicaError::IndexMismatch)?;
        let scorer = Scorer::new(
            decoder.u64().ok_or(ReplicaError::IndexMismatch)?,
            decoder.u64().ok_or(ReplicaError::IndexMismatch)?,
        );
        let count = usize::try_from(document_count).map_err(|_| ReplicaError::IndexMismatch)?;
        let documents = decode_documents(&mut decoder, count)?;
        if !decoder.is_finished() {
            return Err(ReplicaError::IndexMismatch);
        }
        let header = IndexHeader {
            index_version,
            tokenizer_version,
            scope_identity,
            document_count,
            scorer,
            content_digest: ContentDigest::from_bytes(*expected.as_bytes()),
        };
        Ok(Self {
            header,
            bytes: bytes.to_vec(),
            documents,
            scorer,
        })
    }

    /// Verifies the digest against the index bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::IndexMismatch`] when the digest does not match.
    pub fn verify(&self) -> Result<(), ReplicaError> {
        if self.bytes.len() < DIGEST_LENGTH {
            return Err(ReplicaError::IndexMismatch);
        }
        let body_end = self.bytes.len() - DIGEST_LENGTH;
        let expected = Hash32::of(&self.bytes[..body_end]);
        if expected.as_bytes().as_slice() != &self.bytes[body_end..]
            || self.header.content_digest.as_bytes() != expected.as_bytes()
        {
            return Err(ReplicaError::IndexMismatch);
        }
        Ok(())
    }

    /// Checks the index tokenizer version.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::TokenizerMismatch`] when the versions differ.
    pub fn check_tokenizer(&self, expected: TokenizerVersion) -> Result<(), ReplicaError> {
        if self.header.tokenizer_version == expected {
            Ok(())
        } else {
            Err(ReplicaError::TokenizerMismatch {
                expected,
                found: self.header.tokenizer_version,
            })
        }
    }

    /// Returns the index header.
    #[must_use]
    pub const fn header(&self) -> &IndexHeader {
        &self.header
    }

    /// Returns the index format version.
    #[must_use]
    pub const fn index_version(&self) -> IndexVersion {
        self.header.index_version
    }

    /// Returns the tokenizer version.
    #[must_use]
    pub const fn tokenizer_version(&self) -> TokenizerVersion {
        self.header.tokenizer_version
    }

    /// Returns the scope identity.
    #[must_use]
    pub const fn scope_identity(&self) -> ScopeIdentity {
        self.header.scope_identity
    }

    /// Returns the document count.
    #[must_use]
    pub const fn document_count(&self) -> u64 {
        self.header.document_count
    }

    /// Returns the index digest.
    #[must_use]
    pub const fn content_digest(&self) -> ContentDigest {
        self.header.content_digest
    }

    /// Returns the canonical index bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the resident byte count.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        as_u64(self.bytes.len())
    }

    /// Runs one search at the given scope.
    ///
    /// The method filters candidates through the scope before top-k selection.
    /// The sort order is score descending, then canonical key ascending.
    #[must_use]
    pub fn search(&self, scope: &Scope, query: &str, limit: usize) -> Vec<SearchHit> {
        let query_tokens: BTreeSet<String> = tokenize(query).into_iter().collect();
        if query_tokens.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut hits: Vec<SearchHit> = Vec::new();
        for (key, document) in &self.documents {
            if !scope.admits_row(key) {
                continue;
            }
            if !query_tokens
                .iter()
                .any(|token| document.tokens.contains_key(token))
            {
                continue;
            }
            hits.push(SearchHit {
                key: key.clone(),
                score: self.score_document(document, &query_tokens),
            });
        }
        hits.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.key.cmp(&right.key))
        });
        hits.truncate(limit);
        hits
    }

    fn score_document(&self, document: &Document, query_tokens: &BTreeSet<String>) -> u64 {
        let length_norm = self.scorer.length_norm.max(1);
        let denominator = length_norm.saturating_add(document.length);
        let mut score = 0_u64;
        for token in query_tokens {
            if let Some(frequency) = document.tokens.get(token) {
                let numerator = u64::from(*frequency)
                    .saturating_mul(self.scorer.term_weight)
                    .saturating_mul(length_norm);
                score = score.saturating_add(numerator / denominator);
            }
        }
        score
    }
}

fn encode_body(
    scope_identity: ScopeIdentity,
    tokenizer_version: TokenizerVersion,
    documents: &BTreeMap<Vec<u8>, Document>,
    scorer: Scorer,
) -> Vec<u8> {
    let mut encoder = Encoder::with_capacity(64 + documents.len().saturating_mul(32));
    encoder.bytes(&MAGIC);
    encoder.u16(INDEX_VERSION.get());
    encoder.u16(tokenizer_version.revision());
    encoder.bytes(scope_identity.as_bytes());
    encoder.u64(as_u64(documents.len()));
    encoder.u64(scorer.term_weight);
    encoder.u64(scorer.length_norm);
    for (key, document) in documents {
        encoder.bytes(key);
        encoder.u64(as_u64(document.tokens.len()));
        for (token, frequency) in &document.tokens {
            encoder.text(token);
            encoder.u64(u64::from(*frequency));
        }
    }
    encoder.into_bytes()
}

fn decode_documents(
    decoder: &mut Decoder<'_>,
    count: usize,
) -> Result<BTreeMap<Vec<u8>, Document>, ReplicaError> {
    let mut documents = BTreeMap::new();
    for _ in 0..count {
        let key = decoder.bytes().ok_or(ReplicaError::IndexMismatch)?.to_vec();
        let token_count = decoder.count(1).ok_or(ReplicaError::IndexMismatch)?;
        let mut tokens = BTreeMap::new();
        let mut length = 0_u64;
        for _ in 0..token_count {
            let token = decoder
                .text()
                .ok_or(ReplicaError::IndexMismatch)?
                .to_owned();
            let frequency = decoder.u64().ok_or(ReplicaError::IndexMismatch)?;
            let frequency = u32::try_from(frequency).map_err(|_| ReplicaError::IndexMismatch)?;
            if tokens.insert(token, frequency).is_some() {
                return Err(ReplicaError::IndexMismatch);
            }
            length = length.saturating_add(u64::from(frequency));
        }
        if documents.insert(key, Document { length, tokens }).is_some() {
            return Err(ReplicaError::IndexMismatch);
        }
    }
    Ok(documents)
}

/// Tokenizes one text with the frozen `tokenizer/v1` rule.
///
/// A token is a run of ASCII letters and digits. The function lowercases each
/// token. Every other byte separates tokens. The function uses no Unicode
/// case table, no locale, and no randomness.
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = Vec::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() {
            current.push(byte.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(token_text(&current));
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(token_text(&current));
    }
    tokens
}

fn token_text(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{
        KeyVersion, ManifestDigest, MapDigest, MapVersion, PolicyVersion, ScopeIdentity,
        TokenizerVersion,
    };

    use super::{INDEX_VERSION, ReplicaIndex, Scorer, tokenize};
    use crate::mirror::RowEntry;
    use crate::scope::{Scope, ScopePin};

    fn scope() -> Scope {
        Scope::new(
            ScopePin::new(
                ManifestDigest::from_bytes([1; 32]),
                PolicyVersion::from_bytes([2; 32]),
                MapVersion::new(3),
                MapDigest::from_bytes([4; 32]),
                ScopeIdentity::from_bytes([5; 32]),
            ),
            TokenizerVersion::new(1),
            [oxkv_contracts::TableName::parse_user("accounts").expect("the table name is valid")],
            [],
        )
    }

    fn rows() -> Vec<(Vec<u8>, RowEntry)> {
        vec![
            (
                b"\x01accounts\x00\x10alice".to_vec(),
                RowEntry {
                    key_version: KeyVersion::new(1),
                    commit_version: oxkv_contracts::version::CommitVersion::new(1),
                    value: Some(b"red green".to_vec()),
                    page_id: None,
                },
            ),
            (
                b"\x01accounts\x00\x10bob".to_vec(),
                RowEntry {
                    key_version: KeyVersion::new(1),
                    commit_version: oxkv_contracts::version::CommitVersion::new(1),
                    value: Some(b"green blue".to_vec()),
                    page_id: None,
                },
            ),
        ]
    }

    #[test]
    fn the_tokenizer_lowercases_and_splits() {
        assert_eq!(
            tokenize("Red, GREEN-blue 42"),
            vec!["red", "green", "blue", "42"]
        );
        assert_eq!(tokenize(" ,.; ").len(), 0);
    }

    #[test]
    fn a_build_round_trips_through_its_bytes() {
        let scope = scope();
        let rows = rows();
        let index = ReplicaIndex::build(
            &scope,
            rows.iter().map(|(key, entry)| (key.as_slice(), entry)),
            Scorer::DEFAULT,
        );
        assert_eq!(index.index_version(), INDEX_VERSION);
        assert_eq!(index.tokenizer_version(), TokenizerVersion::new(1));
        assert_eq!(index.document_count(), 2);
        assert!(index.verify().is_ok());
        let opened = ReplicaIndex::open(index.bytes()).expect("the index opens");
        assert_eq!(opened.bytes(), index.bytes());
        assert_eq!(
            opened.search(&scope, "green", 10),
            index.search(&scope, "green", 10)
        );
    }

    #[test]
    fn the_score_uses_no_global_statistics() {
        let scope = scope();
        let rows = rows();
        let index = ReplicaIndex::build(
            &scope,
            rows.iter().map(|(key, entry)| (key.as_slice(), entry)),
            Scorer::DEFAULT,
        );
        let hits = index.search(&scope, "green", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits.iter().map(|hit| hit.key.clone()).collect::<Vec<_>>(),
            vec![
                b"\x01accounts\x00\x10alice".to_vec(),
                b"\x01accounts\x00\x10bob".to_vec()
            ]
        );
    }

    #[test]
    fn a_changed_byte_fails_the_digest() {
        let scope = scope();
        let rows = rows();
        let index = ReplicaIndex::build(
            &scope,
            rows.iter().map(|(key, entry)| (key.as_slice(), entry)),
            Scorer::DEFAULT,
        );
        let mut bytes = index.bytes().to_vec();
        let position = bytes.len() / 2;
        bytes[position] ^= 0x01;
        assert_eq!(
            ReplicaIndex::open(&bytes)
                .expect_err("the digest differs")
                .code(),
            "search.index_mismatch"
        );
    }
}
