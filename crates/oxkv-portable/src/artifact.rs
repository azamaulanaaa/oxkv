//! Canonical keys, pages, snapshots, and business events.
//!
//! Every artifact in this module has one byte shape. Integers are fixed width
//! and big-endian. Text values are length-prefixed UTF-8. A snapshot has a
//! canonical text preamble and a binary page body. A business event has a
//! binary envelope.
//!
//! The module has no clock, no randomness, no environment, no locale, and no
//! I/O. It uses no floating point.

use core::fmt::Write as _;
use std::collections::BTreeMap;

use oxkv_contracts::keyspace::{
    KEY_ENCODING_VERSION, SYSTEM_MARKER, TableName, USER_COMPONENT_BOOL, USER_COMPONENT_BYTES,
    USER_COMPONENT_I64, USER_COMPONENT_TEXT, USER_COMPONENT_U64,
};
use oxkv_contracts::version::{DigestDomain, Hash32};

use crate::encode::{Decoder, EncodeError, Encoder, decode_hex, hex_lower};

const SNAPSHOT_MAGIC: &str = "oxkv/snapshot-page/v1";
const PAGE_MAGIC: &[u8] = b"OXKP1";
const EVENT_MAGIC: &[u8] = b"OXKE1";
const EVENT_VERSION: &str = "oxkv/event/v1";

/// One component of a user key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Component {
    /// A UTF-8 text component.
    Text(String),
    /// An opaque bytes component.
    Bytes(Vec<u8>),
    /// An unsigned 64-bit component.
    U64(u64),
    /// A signed 64-bit component.
    I64(i64),
    /// A boolean component.
    Bool(bool),
}

impl Component {
    /// Returns the stable component tag.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Text(_) => USER_COMPONENT_TEXT,
            Self::Bytes(_) => USER_COMPONENT_BYTES,
            Self::U64(_) => USER_COMPONENT_U64,
            Self::I64(_) => USER_COMPONENT_I64,
            Self::Bool(_) => USER_COMPONENT_BOOL,
        }
    }

    fn encode(&self, encoder: &mut Encoder) {
        encoder.u8(self.tag());
        match self {
            Self::Text(value) => encoder.text(value),
            Self::Bytes(value) => encoder.bytes(value),
            Self::U64(value) => encoder.u64(*value),
            Self::I64(value) => encoder.i64(*value),
            Self::Bool(value) => encoder.bool(*value),
        }
    }

    fn decode(tag: u8, decoder: &mut Decoder<'_>) -> Result<Self, KeyError> {
        match tag {
            USER_COMPONENT_TEXT => Ok(Self::Text(decoder.text()?)),
            USER_COMPONENT_BYTES => Ok(Self::Bytes(decoder.bytes()?.to_vec())),
            USER_COMPONENT_U64 => Ok(Self::U64(decoder.u64()?)),
            USER_COMPONENT_I64 => Ok(Self::I64(decoder.i64()?)),
            USER_COMPONENT_BOOL => Ok(Self::Bool(decoder.bool()?)),
            SYSTEM_MARKER => Err(KeyError::ReservedMarker),
            _ => Err(KeyError::UnknownComponentTag { tag }),
        }
    }
}

/// An error from key encoding or decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The key encoding version is unknown.
    #[error("The key encoding version {found} is unknown.")]
    UnknownVersion {
        /// The version byte in the key.
        found: u8,
    },
    /// The table name terminator is absent.
    #[error("The key has no table name terminator.")]
    MissingTerminator,
    /// The table name is invalid.
    #[error("The key has an invalid table name.")]
    InvalidTableName,
    /// A user component uses the system marker.
    #[error("A user key cannot use the system marker as a component tag.")]
    ReservedMarker,
    /// A component tag is unknown.
    #[error("The component tag {tag} is unknown.")]
    UnknownComponentTag {
        /// The unknown tag.
        tag: u8,
    },
    /// The key bytes are truncated or malformed.
    #[error("The key bytes are malformed.")]
    Decode(#[from] EncodeError),
}

impl KeyError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownVersion { .. } => "key.unknown_version",
            Self::MissingTerminator
            | Self::InvalidTableName
            | Self::Decode(_)
            | Self::UnknownComponentTag { .. } => "key.malformed",
            Self::ReservedMarker => "key.reserved_system_marker",
        }
    }
}

/// A decoded user key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedKey {
    /// The table name.
    pub table: String,
    /// The key components.
    pub components: Vec<Component>,
}

/// Encodes one user key with the v1 key encoding.
///
/// The form is `0x01 || table-name || 0x00 || component...`.
///
/// # Errors
///
/// Returns [`KeyError::InvalidTableName`] when the table name is invalid.
/// Returns [`KeyError::ReservedMarker`] when a component uses `0xFF`.
pub fn encode_key(table: &str, components: &[Component]) -> Result<Vec<u8>, KeyError> {
    let table = TableName::parse(table).map_err(|_| KeyError::InvalidTableName)?;
    let mut encoder = Encoder::new();
    encoder.u8(KEY_ENCODING_VERSION);
    encoder.raw(table.as_str().as_bytes());
    encoder.u8(0);
    for component in components {
        if component.tag() == SYSTEM_MARKER {
            return Err(KeyError::ReservedMarker);
        }
        component.encode(&mut encoder);
    }
    Ok(encoder.finish())
}

/// Decodes one v1 user key.
///
/// # Errors
///
/// Returns [`KeyError`] when the version, table name, terminator, or component
/// is invalid.
pub fn decode_key(bytes: &[u8]) -> Result<DecodedKey, KeyError> {
    let mut decoder = Decoder::new(bytes);
    let version = decoder.u8()?;
    if version != KEY_ENCODING_VERSION {
        return Err(KeyError::UnknownVersion { found: version });
    }
    let mut table_bytes = Vec::new();
    loop {
        let byte = decoder.u8()?;
        if byte == 0 {
            break;
        }
        table_bytes.push(byte);
    }
    let table = core::str::from_utf8(&table_bytes).map_err(|_| KeyError::InvalidTableName)?;
    let table = TableName::parse(table).map_err(|_| KeyError::InvalidTableName)?;
    let mut components = Vec::new();
    while !decoder.is_finished() {
        let tag = decoder.u8()?;
        components.push(Component::decode(tag, &mut decoder)?);
    }
    Ok(DecodedKey {
        table: table.as_str().to_owned(),
        components,
    })
}

/// One page input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageInput {
    /// The page sequence inside the snapshot.
    pub page_sequence: u64,
    /// The records in canonical key order.
    pub records: Vec<(Vec<u8>, Vec<u8>)>,
}

/// One canonical page.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The deterministic page identity.
    pub page_id: [u8; 32],
    /// The page sequence inside the snapshot.
    pub page_sequence: u64,
    /// The shard commit version.
    pub commit_version: u64,
    /// The shard identifier.
    pub shard_id: Vec<u8>,
    /// The inclusive lower prefix bound.
    pub prefix_lower: Vec<u8>,
    /// The exclusive upper prefix bound.
    pub prefix_upper: Vec<u8>,
    /// The records in canonical key order.
    pub records: Vec<(Vec<u8>, Vec<u8>)>,
    /// The digest of every byte before the digest.
    pub content_digest: [u8; 32],
}

impl Page {
    /// Creates a page and computes its identity and digest.
    #[must_use]
    pub fn new(
        page_sequence: u64,
        commit_version: u64,
        shard_id: &[u8],
        prefix_lower: &[u8],
        prefix_upper: &[u8],
        records: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Self {
        let page_id = page_identity(
            shard_id,
            commit_version,
            page_sequence,
            prefix_lower,
            prefix_upper,
        );
        let mut page = Self {
            page_id: *page_id.as_bytes(),
            page_sequence,
            commit_version,
            shard_id: shard_id.to_vec(),
            prefix_lower: prefix_lower.to_vec(),
            prefix_upper: prefix_upper.to_vec(),
            records,
            content_digest: [0; 32],
        };
        let digest = Hash32::of_domain(DigestDomain::Content, &page.body());
        page.content_digest = *digest.as_bytes();
        page
    }

    /// Encodes the page.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.body();
        bytes.extend_from_slice(&self.content_digest);
        bytes
    }

    /// Decodes and verifies one page.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] when the bytes are truncated or malformed.
    /// Returns [`SnapshotError::PageIdentityMismatch`] when the page identity
    /// does not match the page fields.
    /// Returns [`SnapshotError::ContentDigestMismatch`] when the digest does
    /// not match the bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        if bytes.len() < PAGE_MAGIC.len() + 32 {
            return Err(SnapshotError::Encode(EncodeError::Truncated));
        }
        let mut decoder = Decoder::new(bytes);
        if decoder.take(PAGE_MAGIC.len())? != PAGE_MAGIC {
            return Err(SnapshotError::InvalidPageMagic);
        }
        let page_id = decoder.take(32)?;
        let page_sequence = decoder.u64()?;
        let commit_version = decoder.u64()?;
        let shard_id = decoder.bytes()?.to_vec();
        let prefix_lower = decoder.bytes()?.to_vec();
        let prefix_upper = decoder.bytes()?.to_vec();
        let record_count = decoder.u64()?;
        let record_count =
            usize::try_from(record_count).map_err(|_| EncodeError::LengthOverflow)?;
        let mut records = Vec::with_capacity(record_count);
        for _ in 0..record_count {
            let key = decoder.bytes()?.to_vec();
            let value = decoder.bytes()?.to_vec();
            records.push((key, value));
        }
        let digest_offset = bytes.len().saturating_sub(32);
        let stored_digest = decoder.take(32)?;
        decoder.finish()?;
        let expected_id = page_identity(
            &shard_id,
            commit_version,
            page_sequence,
            &prefix_lower,
            &prefix_upper,
        );
        if expected_id.as_bytes().as_slice() != page_id {
            return Err(SnapshotError::PageIdentityMismatch);
        }
        let expected_digest = Hash32::of_domain(
            DigestDomain::Content,
            bytes.get(..digest_offset).unwrap_or_default(),
        );
        if expected_digest.as_bytes().as_slice() != stored_digest {
            return Err(SnapshotError::ContentDigestMismatch);
        }
        let mut content_digest = [0_u8; 32];
        content_digest.copy_from_slice(stored_digest);
        Ok(Self {
            page_id: expected_id_bytes(page_id),
            page_sequence,
            commit_version,
            shard_id,
            prefix_lower,
            prefix_upper,
            records,
            content_digest,
        })
    }

    fn body(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.raw(PAGE_MAGIC);
        encoder.raw(&self.page_id);
        encoder.u64(self.page_sequence);
        encoder.u64(self.commit_version);
        encoder.bytes(&self.shard_id);
        encoder.bytes(&self.prefix_lower);
        encoder.bytes(&self.prefix_upper);
        encoder.u64(u64::try_from(self.records.len()).unwrap_or(u64::MAX));
        for (key, value) in &self.records {
            encoder.bytes(key);
            encoder.bytes(value);
        }
        encoder.finish()
    }
}

fn page_identity(
    shard_id: &[u8],
    commit_version: u64,
    page_sequence: u64,
    prefix_lower: &[u8],
    prefix_upper: &[u8],
) -> Hash32 {
    let mut encoder = Encoder::new();
    encoder.bytes(shard_id);
    encoder.u64(commit_version);
    encoder.u64(page_sequence);
    encoder.bytes(prefix_lower);
    encoder.bytes(prefix_upper);
    Hash32::of_domain(DigestDomain::Page, &encoder.finish())
}

fn expected_id_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut id = [0_u8; 32];
    id.copy_from_slice(bytes);
    id
}

/// One snapshot input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInput {
    /// The snapshot and page format major number.
    pub format_major: u16,
    /// The snapshot and page format minor number.
    pub format_minor: u16,
    /// The key encoding annex version.
    pub key_encoding_version: u16,
    /// The manifest digest.
    pub manifest_digest: [u8; 32],
    /// The shard identifier.
    pub shard_id: Vec<u8>,
    /// The inclusive lower prefix bound.
    pub prefix_lower: Vec<u8>,
    /// The exclusive upper prefix bound.
    pub prefix_upper: Vec<u8>,
    /// The shard commit version.
    pub commit_version: u64,
    /// The authority lease term.
    pub lease_term: u64,
    /// The pages in page sequence order.
    pub pages: Vec<PageInput>,
}

/// One canonical snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The snapshot and page format major number.
    pub format_major: u16,
    /// The snapshot and page format minor number.
    pub format_minor: u16,
    /// The key encoding annex version.
    pub key_encoding_version: u16,
    /// The manifest digest.
    pub manifest_digest: [u8; 32],
    /// The shard identifier.
    pub shard_id: Vec<u8>,
    /// The inclusive lower prefix bound.
    pub prefix_lower: Vec<u8>,
    /// The exclusive upper prefix bound.
    pub prefix_upper: Vec<u8>,
    /// The shard commit version.
    pub commit_version: u64,
    /// The authority lease term.
    pub lease_term: u64,
    /// The pages in page sequence order.
    pub pages: Vec<Page>,
}

impl Snapshot {
    /// Builds a snapshot and every page from the input.
    #[must_use]
    pub fn from_input(input: &SnapshotInput) -> Self {
        let pages = input
            .pages
            .iter()
            .map(|page| {
                Page::new(
                    page.page_sequence,
                    input.commit_version,
                    &input.shard_id,
                    &input.prefix_lower,
                    &input.prefix_upper,
                    page.records.clone(),
                )
            })
            .collect();
        Self {
            format_major: input.format_major,
            format_minor: input.format_minor,
            key_encoding_version: input.key_encoding_version,
            manifest_digest: input.manifest_digest,
            shard_id: input.shard_id.clone(),
            prefix_lower: input.prefix_lower.clone(),
            prefix_upper: input.prefix_upper.clone(),
            commit_version: input.commit_version,
            lease_term: input.lease_term,
            pages,
        }
    }

    /// Encodes the snapshot.
    ///
    /// The content digest covers the page directory and every page digest.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Encoder::new();
        body.u64(u64::try_from(self.pages.len()).unwrap_or(u64::MAX));
        for page in &self.pages {
            let bytes = page.encode();
            body.u64(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            body.raw(&bytes);
        }
        let body = body.finish();
        let content_digest = Hash32::of_domain(DigestDomain::Content, &body);
        let mut header = String::new();
        header.push_str(SNAPSHOT_MAGIC);
        header.push('\n');
        let _ = writeln!(header, "commit_version={}", hex_u64(self.commit_version));
        let _ = writeln!(
            header,
            "content_digest={}",
            hex_lower(content_digest.as_bytes())
        );
        let _ = writeln!(
            header,
            "format_version={}.{}",
            self.format_major, self.format_minor
        );
        let _ = writeln!(
            header,
            "key_encoding_version={}",
            hex_u16(self.key_encoding_version)
        );
        let _ = writeln!(header, "lease_term={}", hex_u64(self.lease_term));
        let _ = writeln!(
            header,
            "manifest_digest={}",
            hex_lower(&self.manifest_digest)
        );
        let _ = writeln!(header, "prefix_lower={}", hex_lower(&self.prefix_lower));
        let _ = writeln!(header, "prefix_upper={}", hex_lower(&self.prefix_upper));
        let _ = writeln!(header, "shard_id={}", hex_lower(&self.shard_id));
        header.push('\n');
        let mut bytes = header.into_bytes();
        bytes.extend_from_slice(&body);
        bytes
    }

    /// Decodes and verifies one snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] when the header, digest, or page is invalid.
    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let separator = bytes
            .windows(2)
            .position(|window| window == b"\n\n")
            .ok_or(SnapshotError::MissingHeader)?;
        let header =
            core::str::from_utf8(&bytes[..separator]).map_err(|_| SnapshotError::InvalidHeader)?;
        let body = bytes.get(separator + 2..).unwrap_or_default();
        let mut lines = header.lines();
        if lines.next() != Some(SNAPSHOT_MAGIC) {
            return Err(SnapshotError::InvalidHeader);
        }
        let mut fields = BTreeMap::new();
        for line in lines {
            let (name, value) = line.split_once('=').ok_or(SnapshotError::InvalidHeader)?;
            if fields.insert(name, value).is_some() {
                return Err(SnapshotError::InvalidHeader);
            }
        }
        let format_version = fields
            .get("format_version")
            .ok_or(SnapshotError::MissingHeader)?;
        let (major, minor) = format_version
            .split_once('.')
            .ok_or(SnapshotError::InvalidHeader)?;
        let format_major = parse_u16(major)?;
        let format_minor = parse_u16(minor)?;
        let commit_version = parse_u64_field(&fields, "commit_version")?;
        let key_encoding_version = parse_u16_hex_field(&fields, "key_encoding_version")?;
        let lease_term = parse_u64_field(&fields, "lease_term")?;
        let manifest_digest = parse_digest_field(&fields, "manifest_digest")?;
        let stored_digest = parse_digest_field(&fields, "content_digest")?;
        let shard_id = parse_bytes_field(&fields, "shard_id")?;
        let prefix_lower = parse_bytes_field(&fields, "prefix_lower")?;
        let prefix_upper = parse_bytes_field(&fields, "prefix_upper")?;
        let expected_digest = Hash32::of_domain(DigestDomain::Content, body);
        if expected_digest.as_bytes().as_slice() != stored_digest {
            return Err(SnapshotError::ContentDigestMismatch);
        }
        let mut decoder = Decoder::new(body);
        let page_count = decoder.u64()?;
        let page_count = usize::try_from(page_count).map_err(|_| EncodeError::LengthOverflow)?;
        let mut pages = Vec::with_capacity(page_count);
        for _ in 0..page_count {
            let page_length = decoder.u64()?;
            let page_length =
                usize::try_from(page_length).map_err(|_| EncodeError::LengthOverflow)?;
            let page_bytes = decoder.take(page_length)?;
            pages.push(Page::decode(page_bytes)?);
        }
        decoder.finish()?;
        Ok(Self {
            format_major,
            format_minor,
            key_encoding_version,
            manifest_digest,
            shard_id,
            prefix_lower,
            prefix_upper,
            commit_version,
            lease_term,
            pages,
        })
    }
}

fn hex_u64(value: u64) -> String {
    hex_lower(&value.to_be_bytes())
}

fn hex_u16(value: u16) -> String {
    hex_lower(&value.to_be_bytes())
}

fn parse_u16(text: &str) -> Result<u16, SnapshotError> {
    text.parse::<u16>()
        .map_err(|_| SnapshotError::InvalidHeader)
}

fn parse_u16_hex_field(fields: &BTreeMap<&str, &str>, name: &str) -> Result<u16, SnapshotError> {
    let bytes = parse_hex_field(fields, name)?;
    if bytes.len() != 2 {
        return Err(SnapshotError::InvalidHeader);
    }
    let mut value = [0_u8; 2];
    value.copy_from_slice(&bytes);
    Ok(u16::from_be_bytes(value))
}

fn parse_u64_field(fields: &BTreeMap<&str, &str>, name: &str) -> Result<u64, SnapshotError> {
    let bytes = parse_hex_field(fields, name)?;
    if bytes.len() != 8 {
        return Err(SnapshotError::InvalidHeader);
    }
    let mut value = [0_u8; 8];
    value.copy_from_slice(&bytes);
    Ok(u64::from_be_bytes(value))
}

fn parse_digest_field(
    fields: &BTreeMap<&str, &str>,
    name: &str,
) -> Result<[u8; 32], SnapshotError> {
    let bytes = parse_hex_field(fields, name)?;
    if bytes.len() != 32 {
        return Err(SnapshotError::InvalidHeader);
    }
    let mut value = [0_u8; 32];
    value.copy_from_slice(&bytes);
    Ok(value)
}

fn parse_bytes_field(fields: &BTreeMap<&str, &str>, name: &str) -> Result<Vec<u8>, SnapshotError> {
    parse_hex_field(fields, name)
}

fn parse_hex_field(fields: &BTreeMap<&str, &str>, name: &str) -> Result<Vec<u8>, SnapshotError> {
    let text = fields.get(name).ok_or(SnapshotError::MissingHeader)?;
    decode_hex(text).map_err(|_| SnapshotError::InvalidHeader)
}

/// An error from snapshot or page handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    /// The snapshot header is absent.
    #[error("The snapshot has no canonical header.")]
    MissingHeader,
    /// The snapshot header is malformed.
    #[error("The snapshot header is malformed.")]
    InvalidHeader,
    /// The page magic is wrong.
    #[error("The page magic is wrong.")]
    InvalidPageMagic,
    /// The page identity does not match the page fields.
    #[error("The page identity does not match the page fields.")]
    PageIdentityMismatch,
    /// The content digest does not match the bytes.
    #[error("The content digest does not match the bytes.")]
    ContentDigestMismatch,
    /// The bytes are truncated or malformed.
    #[error("The snapshot bytes are malformed.")]
    Encode(#[from] EncodeError),
}

impl SnapshotError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingHeader => "format.missing_provenance",
            Self::InvalidHeader
            | Self::InvalidPageMagic
            | Self::PageIdentityMismatch
            | Self::ContentDigestMismatch => "format.corrupt_page",
            Self::Encode(error) => error.code(),
        }
    }
}

/// One business event input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventInput {
    /// The shard identifier.
    pub shard_id: Vec<u8>,
    /// The table name.
    pub table: String,
    /// The canonical key bytes.
    pub key: Vec<u8>,
    /// The sequence of the key inside the commit.
    pub key_sequence: u64,
    /// The shard commit version.
    pub commit_version: u64,
    /// The injected commit time in milliseconds.
    pub committed_at: u64,
    /// The rule identity.
    pub rule_id: String,
    /// The pinned policy version.
    pub policy_version: [u8; 32],
    /// The emission index inside the commit.
    pub emission_index: u64,
    /// The effect payload.
    pub payload: Vec<u8>,
    /// The payload schema.
    pub payload_schema: String,
    /// The origin request identity.
    pub origin_request_id: [u8; 32],
}

/// One canonical business event.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The shard identifier.
    pub shard_id: Vec<u8>,
    /// The table name.
    pub table: String,
    /// The canonical key bytes.
    pub key: Vec<u8>,
    /// The sequence of the key inside the commit.
    pub key_sequence: u64,
    /// The shard commit version.
    pub commit_version: u64,
    /// The injected commit time in milliseconds.
    pub committed_at: u64,
    /// The rule identity.
    pub rule_id: String,
    /// The pinned policy version.
    pub policy_version: [u8; 32],
    /// The emission index inside the commit.
    pub emission_index: u64,
    /// The effect payload.
    pub payload: Vec<u8>,
    /// The payload schema.
    pub payload_schema: String,
    /// The origin request identity.
    pub origin_request_id: [u8; 32],
    /// The deterministic event identity.
    pub event_id: [u8; 32],
}

impl Event {
    /// Builds an event and computes its identity.
    #[must_use]
    pub fn from_input(input: &EventInput) -> Self {
        let event_id = event_identity(
            &input.shard_id,
            &input.key,
            input.commit_version,
            &input.rule_id,
            input.emission_index,
        );
        Self {
            shard_id: input.shard_id.clone(),
            table: input.table.clone(),
            key: input.key.clone(),
            key_sequence: input.key_sequence,
            commit_version: input.commit_version,
            committed_at: input.committed_at,
            rule_id: input.rule_id.clone(),
            policy_version: input.policy_version,
            emission_index: input.emission_index,
            payload: input.payload.clone(),
            payload_schema: input.payload_schema.clone(),
            origin_request_id: input.origin_request_id,
            event_id: *event_id.as_bytes(),
        }
    }

    /// Encodes the event envelope.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.raw(EVENT_MAGIC);
        encoder.text(EVENT_VERSION);
        encoder.raw(&self.event_id);
        encoder.bytes(&self.shard_id);
        encoder.text(&self.table);
        encoder.bytes(&self.key);
        encoder.u64(self.key_sequence);
        encoder.u64(self.commit_version);
        encoder.u64(self.committed_at);
        encoder.text(&self.rule_id);
        encoder.raw(&self.policy_version);
        encoder.u64(self.emission_index);
        encoder.bytes(&self.payload);
        encoder.text(&self.payload_schema);
        encoder.raw(&self.origin_request_id);
        encoder.finish()
    }
}

/// Encodes one business event.
#[must_use]
pub fn encode_event(input: &EventInput) -> Vec<u8> {
    Event::from_input(input).encode()
}

fn event_identity(
    shard_id: &[u8],
    key: &[u8],
    commit_version: u64,
    rule_id: &str,
    emission_index: u64,
) -> Hash32 {
    let mut encoder = Encoder::new();
    encoder.bytes(shard_id);
    encoder.bytes(key);
    encoder.u64(commit_version);
    encoder.text(rule_id);
    encoder.u64(emission_index);
    Hash32::of_domain(DigestDomain::EventId, &encoder.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_uses_the_v1_layout() {
        let bytes =
            encode_key("accounts", &[Component::Text("key".to_owned())]).expect("the key is valid");
        assert_eq!(
            bytes,
            vec![
                0x01, b'a', b'c', b'c', b'o', b'u', b'n', b't', b's', 0x00, 0x10, 0, 0, 0, 0, 0, 0,
                0, 3, b'k', b'e', b'y',
            ]
        );
        let decoded = decode_key(&bytes).expect("the key decodes");
        assert_eq!(decoded.table, "accounts");
        assert_eq!(decoded.components, vec![Component::Text("key".to_owned())]);
    }

    #[test]
    fn a_key_round_trips_every_component() {
        let components = vec![
            Component::Text("alpha".to_owned()),
            Component::Bytes(vec![0, 1, 2]),
            Component::U64(u64::MAX),
            Component::I64(-7),
            Component::Bool(true),
        ];
        let bytes = encode_key("ledger_entry", &components).expect("the key is valid");
        let decoded = decode_key(&bytes).expect("the key decodes");
        assert_eq!(decoded.components, components);
    }

    #[test]
    fn a_user_key_rejects_the_system_marker() {
        let mut bytes = vec![0x01];
        bytes.extend_from_slice(b"accounts");
        bytes.push(0x00);
        bytes.push(SYSTEM_MARKER);
        assert_eq!(decode_key(&bytes), Err(KeyError::ReservedMarker));
    }

    #[test]
    fn a_snapshot_round_trips_and_re_encodes_to_the_same_bytes() {
        let input = SnapshotInput {
            format_major: 1,
            format_minor: 0,
            key_encoding_version: 1,
            manifest_digest: [1; 32],
            shard_id: vec![0xab],
            prefix_lower: vec![0x01, b'a'],
            prefix_upper: vec![0x01, b'b'],
            commit_version: 18,
            lease_term: 9,
            pages: vec![PageInput {
                page_sequence: 1,
                records: vec![(vec![1, 2], vec![3, 4])],
            }],
        };
        let snapshot = Snapshot::from_input(&input);
        let bytes = snapshot.encode();
        let decoded = Snapshot::decode(&bytes).expect("the snapshot decodes");
        assert_eq!(decoded, snapshot);
        assert_eq!(decoded.encode(), bytes);
    }

    #[test]
    fn a_corrupt_snapshot_fails() {
        let input = SnapshotInput {
            format_major: 1,
            format_minor: 0,
            key_encoding_version: 1,
            manifest_digest: [1; 32],
            shard_id: vec![1],
            prefix_lower: vec![1],
            prefix_upper: vec![2],
            commit_version: 3,
            lease_term: 4,
            pages: Vec::new(),
        };
        let mut bytes = Snapshot::from_input(&input).encode();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert_eq!(
            Snapshot::decode(&bytes),
            Err(SnapshotError::ContentDigestMismatch)
        );
    }

    #[test]
    fn an_event_identity_is_deterministic() {
        let input = EventInput {
            shard_id: vec![1],
            table: "accounts".to_owned(),
            key: vec![2],
            key_sequence: 3,
            commit_version: 4,
            committed_at: 5,
            rule_id: "rule".to_owned(),
            policy_version: [6; 32],
            emission_index: 7,
            payload: vec![8],
            payload_schema: "schema".to_owned(),
            origin_request_id: [9; 32],
        };
        let first = Event::from_input(&input);
        let second = Event::from_input(&input);
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(first.encode(), second.encode());
    }
}
