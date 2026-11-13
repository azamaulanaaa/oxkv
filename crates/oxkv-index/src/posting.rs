//! Postings, document maps, flags, and the logical index key.
//!
//! A posting lives in the `0x05` system namespace. [`PostingKey`] encodes the
//! logical part only. The storage layer prepends the table prefix and the
//! `0xFF` system marker:
//!
//! ```text
//! P || 0xFF || 0x05 || field || term || document
//! ```
//!
//! A posting carries the document key, the commit version, the term frequency,
//! positions, the filter flags, and the search flags. A field declares the
//! flag names that a posting may use.

use std::collections::BTreeMap;

use oxkv_contracts::keyspace::SYSTEM_ID_INDEX;
use oxkv_contracts::version::CommitVersion;

use crate::error::IndexError;

/// The logical namespace byte for index postings.
pub const INDEX_NAMESPACE: u8 = SYSTEM_ID_INDEX;

/// One flag declaration from the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagDecl {
    name: String,
    bit: u8,
}

impl FlagDecl {
    /// Creates a flag declaration.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidManifest`] when the name is empty or the
    /// bit is not below 64.
    pub fn new(name: impl Into<String>, bit: u8) -> Result<Self, IndexError> {
        let name = name.into();
        if name.is_empty() {
            return Err(IndexError::InvalidManifest {
                reason: "a flag name is empty".to_owned(),
            });
        }
        if bit >= 64 {
            return Err(IndexError::InvalidManifest {
                reason: format!("the flag {name} uses a bit above 63"),
            });
        }
        Ok(Self { name, bit })
    }

    /// Returns the flag name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the flag bit.
    #[must_use]
    pub const fn bit(&self) -> u8 {
        self.bit
    }

    /// Returns the one-bit mask of the flag.
    #[must_use]
    pub const fn mask(&self) -> u64 {
        1_u64 << self.bit
    }
}

/// The declared flag set of one field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlagSet {
    declarations: Vec<FlagDecl>,
    mask: u64,
}

impl FlagSet {
    /// Creates a flag set from declarations.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidManifest`] when two declarations share a
    /// name or a bit.
    pub fn new(declarations: Vec<FlagDecl>) -> Result<Self, IndexError> {
        let mut mask = 0_u64;
        let mut names: Vec<&str> = Vec::with_capacity(declarations.len());
        for declaration in &declarations {
            if mask & declaration.mask() != 0 {
                return Err(IndexError::DuplicateName {
                    name: declaration.name().to_owned(),
                });
            }
            if names.contains(&declaration.name()) {
                return Err(IndexError::DuplicateName {
                    name: declaration.name().to_owned(),
                });
            }
            mask |= declaration.mask();
            names.push(declaration.name());
        }
        let mut declarations = declarations;
        declarations.sort_by_key(FlagDecl::bit);
        Ok(Self { declarations, mask })
    }

    /// Returns the declarations.
    #[must_use]
    pub fn declarations(&self) -> &[FlagDecl] {
        &self.declarations
    }

    /// Returns the union mask of every declared bit.
    #[must_use]
    pub const fn mask(&self) -> u64 {
        self.mask
    }

    /// Returns true when no flag is declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    /// Returns the bit of a declared flag name.
    #[must_use]
    pub fn bit_for(&self, name: &str) -> Option<u8> {
        self.declarations
            .iter()
            .find(|declaration| declaration.name() == name)
            .map(FlagDecl::bit)
    }

    /// Returns the name of a declared flag bit.
    #[must_use]
    pub fn name_for(&self, bit: u8) -> Option<&str> {
        self.declarations
            .iter()
            .find(|declaration| declaration.bit() == bit)
            .map(FlagDecl::name)
    }

    /// Checks that every set bit is declared.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::UnknownFlag`] for the first undeclared bit.
    pub fn validate(&self, flags: PostingFlags, field: &str) -> Result<(), IndexError> {
        let unknown = flags.bits() & !self.mask;
        if unknown == 0 {
            return Ok(());
        }
        let bit = u8::try_from(unknown.trailing_zeros()).unwrap_or(u8::MAX);
        Err(IndexError::UnknownFlag {
            field: field.to_owned(),
            bit,
        })
    }
}

/// A set of posting flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PostingFlags(u64);

impl PostingFlags {
    /// No flag is set.
    pub const EMPTY: Self = Self(0);

    /// Creates a flag set from raw bits.
    #[must_use]
    pub const fn new(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns the raw bits.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Returns true when no bit is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns true when every bit of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns the union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl From<u64> for PostingFlags {
    fn from(bits: u64) -> Self {
        Self::new(bits)
    }
}

/// One posting for one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posting {
    document: Vec<u8>,
    commit_version: CommitVersion,
    term_frequency: u32,
    positions: Vec<u32>,
    filter_flags: PostingFlags,
    search_flags: PostingFlags,
}

impl Posting {
    /// Creates a posting.
    ///
    /// The constructor sorts and deduplicates the positions.
    #[must_use]
    pub fn new(
        document: Vec<u8>,
        commit_version: CommitVersion,
        term_frequency: u32,
        mut positions: Vec<u32>,
        filter_flags: PostingFlags,
        search_flags: PostingFlags,
    ) -> Self {
        positions.sort_unstable();
        positions.dedup();
        Self {
            document,
            commit_version,
            term_frequency,
            positions,
            filter_flags,
            search_flags,
        }
    }

    /// Returns the document key.
    #[must_use]
    pub fn document(&self) -> &[u8] {
        &self.document
    }

    /// Returns the commit version of the posting.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the term frequency.
    #[must_use]
    pub const fn term_frequency(&self) -> u32 {
        self.term_frequency
    }

    /// Returns the token positions. The list is empty when the field does not
    /// store positions.
    #[must_use]
    pub fn positions(&self) -> &[u32] {
        &self.positions
    }

    /// Returns the filter flags.
    #[must_use]
    pub const fn filter_flags(&self) -> PostingFlags {
        self.filter_flags
    }

    /// Returns the search flags.
    #[must_use]
    pub const fn search_flags(&self) -> PostingFlags {
        self.search_flags
    }
}

/// The term frequency and positions of one term in one field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TermFrequency {
    frequency: u32,
    positions: Vec<u32>,
}

impl TermFrequency {
    /// Creates a term frequency.
    #[must_use]
    pub fn new(frequency: u32, positions: Vec<u32>) -> Self {
        Self {
            frequency,
            positions,
        }
    }

    /// Returns the term frequency.
    #[must_use]
    pub const fn frequency(&self) -> u32 {
        self.frequency
    }

    /// Returns the token positions.
    #[must_use]
    pub fn positions(&self) -> &[u32] {
        &self.positions
    }
}

/// One indexed field of a stored document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredField {
    token_count: u32,
    filter_flags: PostingFlags,
    search_flags: PostingFlags,
    terms: BTreeMap<String, TermFrequency>,
}

impl StoredField {
    /// Creates a stored field.
    #[must_use]
    pub const fn new(
        token_count: u32,
        filter_flags: PostingFlags,
        search_flags: PostingFlags,
        terms: BTreeMap<String, TermFrequency>,
    ) -> Self {
        Self {
            token_count,
            filter_flags,
            search_flags,
            terms,
        }
    }

    /// Returns the number of tokens in the field.
    #[must_use]
    pub const fn token_count(&self) -> u32 {
        self.token_count
    }

    /// Returns the filter flags of the field.
    #[must_use]
    pub const fn filter_flags(&self) -> PostingFlags {
        self.filter_flags
    }

    /// Returns the search flags of the field.
    #[must_use]
    pub const fn search_flags(&self) -> PostingFlags {
        self.search_flags
    }

    /// Returns the terms in canonical term order.
    #[must_use]
    pub const fn terms(&self) -> &BTreeMap<String, TermFrequency> {
        &self.terms
    }
}

/// One stored document in the document map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDocument {
    key: Vec<u8>,
    fields: BTreeMap<String, StoredField>,
}

impl StoredDocument {
    /// Creates a stored document.
    #[must_use]
    pub const fn new(key: Vec<u8>, fields: BTreeMap<String, StoredField>) -> Self {
        Self { key, fields }
    }

    /// Returns the document key.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Returns the fields in canonical field order.
    #[must_use]
    pub const fn fields(&self) -> &BTreeMap<String, StoredField> {
        &self.fields
    }
}

/// One field of a document to index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentField {
    field: String,
    text: String,
    filter_flags: u64,
    search_flags: u64,
}

impl DocumentField {
    /// Creates a document field.
    #[must_use]
    pub fn new(
        field: impl Into<String>,
        text: impl Into<String>,
        filter_flags: u64,
        search_flags: u64,
    ) -> Self {
        Self {
            field: field.into(),
            text: text.into(),
            filter_flags,
            search_flags,
        }
    }

    /// Returns the field name.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Returns the field text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the filter flag bits.
    #[must_use]
    pub const fn filter_flags(&self) -> u64 {
        self.filter_flags
    }

    /// Returns the search flag bits.
    #[must_use]
    pub const fn search_flags(&self) -> u64 {
        self.search_flags
    }
}

/// One document to index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDocument {
    key: Vec<u8>,
    fields: Vec<DocumentField>,
}

impl IndexDocument {
    /// Creates a document.
    #[must_use]
    pub fn new(key: impl Into<Vec<u8>>, fields: Vec<DocumentField>) -> Self {
        Self {
            key: key.into(),
            fields,
        }
    }

    /// Returns the document key.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Returns the fields.
    #[must_use]
    pub fn fields(&self) -> &[DocumentField] {
        &self.fields
    }
}

/// The logical key of one posting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PostingKey {
    field: String,
    term: String,
    document: Vec<u8>,
}

impl PostingKey {
    /// The logical namespace byte.
    pub const NAMESPACE: u8 = INDEX_NAMESPACE;

    /// Creates a posting key.
    #[must_use]
    pub fn new(field: impl Into<String>, term: impl Into<String>, document: Vec<u8>) -> Self {
        Self {
            field: field.into(),
            term: term.into(),
            document,
        }
    }

    /// Returns the field name.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Returns the term.
    #[must_use]
    pub fn term(&self) -> &str {
        &self.term
    }

    /// Returns the document key.
    #[must_use]
    pub fn document(&self) -> &[u8] {
        &self.document
    }

    /// Encodes the logical key.
    ///
    /// The encoding is canonical. It starts with [`INDEX_NAMESPACE`].
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(1 + 8 + self.field.len() + 8 + self.term.len() + 8);
        bytes.push(Self::NAMESPACE);
        push_bytes(&mut bytes, self.field.as_bytes());
        push_bytes(&mut bytes, self.term.as_bytes());
        push_bytes(&mut bytes, &self.document);
        bytes
    }

    /// Decodes a logical key.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::IndexMismatch`] when the bytes are malformed or do
    /// not start with [`INDEX_NAMESPACE`].
    pub fn decode(bytes: &[u8]) -> Result<Self, IndexError> {
        let mut decoder = Decoder::new(bytes);
        if decoder.u8()? != Self::NAMESPACE {
            return Err(IndexError::IndexMismatch);
        }
        let field = decoder.text()?;
        let term = decoder.text()?;
        let document = decoder.bytes()?.to_vec();
        decoder.finish()?;
        Ok(Self {
            field,
            term,
            document,
        })
    }
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
}

struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn finish(&self) -> Result<(), IndexError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(IndexError::IndexMismatch)
        }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], IndexError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(IndexError::IndexMismatch)?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(IndexError::IndexMismatch)?;
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, IndexError> {
        let slice = self.take(1)?;
        Ok(slice[0])
    }

    fn u64(&mut self) -> Result<u64, IndexError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(u64::from_be_bytes(bytes))
    }

    fn bytes(&mut self) -> Result<&'a [u8], IndexError> {
        let length = self.u64()?;
        let length = usize::try_from(length).map_err(|_| IndexError::IndexMismatch)?;
        self.take(length)
    }

    fn text(&mut self) -> Result<String, IndexError> {
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| IndexError::IndexMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posting_key_round_trips() {
        let key = PostingKey::new("title", "café", vec![0x10, b'a']);
        let bytes = key.encode();
        assert_eq!(bytes[0], INDEX_NAMESPACE);
        assert_eq!(PostingKey::decode(&bytes), Ok(key));
    }

    #[test]
    fn a_posting_key_rejects_a_bad_namespace() {
        let mut bytes = PostingKey::new("title", "café", vec![1]).encode();
        bytes[0] = 0x04;
        assert_eq!(PostingKey::decode(&bytes), Err(IndexError::IndexMismatch));
    }

    #[test]
    fn a_posting_key_rejects_trailing_bytes() {
        let mut bytes = PostingKey::new("title", "café", vec![1]).encode();
        bytes.push(0);
        assert_eq!(PostingKey::decode(&bytes), Err(IndexError::IndexMismatch));
    }

    #[test]
    fn a_flag_set_rejects_duplicate_bits_and_names() {
        let one = FlagDecl::new("published", 1).expect("the flag is valid");
        let two = FlagDecl::new("public", 1).expect("the flag is valid");
        assert!(FlagSet::new(vec![one.clone(), two]).is_err());
        let three = FlagDecl::new("published", 2).expect("the flag is valid");
        assert!(FlagSet::new(vec![one, three]).is_err());
    }

    #[test]
    fn a_flag_set_validates_unknown_bits() {
        let flags = FlagSet::new(vec![
            FlagDecl::new("published", 1).expect("the flag is valid"),
        ])
        .expect("the set is valid");
        assert!(flags.validate(PostingFlags::new(0b10), "title").is_ok());
        assert_eq!(
            flags.validate(PostingFlags::new(0b100), "title"),
            Err(IndexError::UnknownFlag {
                field: "title".to_owned(),
                bit: 2
            })
        );
    }

    #[test]
    fn posting_positions_are_sorted_and_unique() {
        let posting = Posting::new(
            vec![1],
            CommitVersion::new(3),
            3,
            vec![2, 0, 2, 1],
            PostingFlags::EMPTY,
            PostingFlags::EMPTY,
        );
        assert_eq!(posting.positions(), [0, 1, 2]);
    }
}
