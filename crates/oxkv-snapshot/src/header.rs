//! The canonical text header preamble.
//!
//! The header is ASCII text with one `name=value` field per line. The field
//! order is fixed. Numeric fields use a fixed width. Hexadecimal fields use
//! lowercase hex. A reader can inspect the header with a text tool.
//!
//! The header ends with the `header_end` line. The binary body follows it.

use core::fmt::Write as _;

use oxkv_contracts::version::{
    ArtifactKind, CommitVersion, ContentDigest, ContentSeal, FactSourceId, FactVersion,
    KeyEncodingVersion, LeaseTerm, ManifestDigest, MapDigest, MapVersion, PolicyVersion,
    PrefixBounds, ProjectionEpoch, ProjectionWatermarks, Provenance, ProvenanceField,
    ScopeIdentity, ScopeVersionVector, ShardId, Signature, SnapshotPageVersion, VersionVector,
};

use crate::codec::CodecId;
use crate::error::SnapshotError;

/// The format identifier on the first header line.
pub const FORMAT_IDENTIFIER: &str = "oxkv-snapshot";

/// The format major version that this crate reads and writes.
pub const FORMAT_MAJOR: u16 = 1;

/// The format minor version that this crate reads and writes.
pub const FORMAT_MINOR: u16 = 0;

/// The required feature bit for the text header.
pub const FEATURE_TEXT_HEADER: u64 = 1 << 0;

/// The required feature bit for `BLAKE3` content digests.
pub const FEATURE_BLAKE3_CONTENT: u64 = 1 << 1;

/// The required feature bit for CRC32C page integrity.
pub const FEATURE_CRC32C_PAGE: u64 = 1 << 2;

/// Every required feature bit that this crate knows.
pub const KNOWN_REQUIRED_FEATURES: u64 =
    FEATURE_TEXT_HEADER | FEATURE_BLAKE3_CONTENT | FEATURE_CRC32C_PAGE;

/// The key encoding annex version that this crate reads.
pub const SUPPORTED_KEY_ENCODING_VERSION: u16 = 1;

const HEADER_END: &str = "header_end";
const U64_WIDTH: usize = 20;
const U16_WIDTH: usize = 5;
const FEATURE_WIDTH: usize = 16;

/// The identity fields of a `ShardSnapshot` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardHeader {
    /// The opaque shard identifier.
    pub shard_id: ShardId,
    /// The inclusive lower bound and the exclusive upper bound.
    pub prefix_bounds: PrefixBounds,
    /// The shard commit version.
    pub commit_version: CommitVersion,
}

/// The identity fields of a `ScopeSnapshot` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeHeader {
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
}

/// The artifact identity carried by a snapshot header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderIdentity {
    /// A shard snapshot identity.
    Shard(ShardHeader),
    /// A scope snapshot identity.
    Scope(ScopeHeader),
}

/// A parsed snapshot header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The snapshot and page contract version.
    pub format_version: SnapshotPageVersion,
    /// The key encoding annex version.
    pub key_encoding_version: KeyEncodingVersion,
    /// The required feature bitset.
    pub required_features: u64,
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The shard map digest.
    pub map_digest: MapDigest,
    /// The shard or scope identity.
    pub identity: HeaderIdentity,
    /// The target page payload size in bytes.
    pub page_size: u64,
    /// The number of pages in the directory.
    pub page_count: u64,
    /// The page codec identifier.
    pub codec: CodecId,
    /// The directory codec identifier.
    pub directory_codec: CodecId,
    /// The absolute directory offset from the start of the snapshot.
    pub directory_offset: u64,
    /// The directory length in bytes.
    pub directory_length: u64,
    /// The content digest of the directory and every page digest.
    pub content_digest: ContentDigest,
    /// The content seal. It covers the content digest and the lease term.
    pub content_seal: ContentSeal,
    /// The authority lease term at seal time.
    pub lease_term: LeaseTerm,
    /// The header length in bytes. It is not a serialized field.
    pub text_length: usize,
}

impl Header {
    /// Returns the artifact kind of this header.
    #[must_use]
    pub const fn artifact(&self) -> ArtifactKind {
        match self.identity {
            HeaderIdentity::Shard(_) => ArtifactKind::ShardSnapshot,
            HeaderIdentity::Scope(_) => ArtifactKind::ScopeSnapshot,
        }
    }

    /// Builds the provenance block that this header declares.
    #[must_use]
    pub fn provenance(&self) -> Provenance {
        let mut provenance = Provenance {
            format_version: Some(self.format_version),
            key_encoding_version: Some(self.key_encoding_version),
            manifest_digest: Some(self.manifest_digest),
            map_version: Some(self.map_version),
            map_digest: Some(self.map_digest),
            content_digest: Some(self.content_digest),
            content_seal: Some(self.content_seal),
            ..Provenance::default()
        };
        match &self.identity {
            HeaderIdentity::Shard(shard) => {
                provenance.shard_id = Some(shard.shard_id.clone());
                provenance.prefix_bounds = Some(shard.prefix_bounds.clone());
                provenance.commit_version = Some(shard.commit_version);
            }
            HeaderIdentity::Scope(scope) => {
                provenance.scope_identity = Some(scope.scope_identity);
                provenance.policy_version = Some(scope.policy_version);
                provenance.version_vector = Some(scope.version_vector.clone());
                provenance.projection_epoch = Some(scope.projection_epoch);
                provenance.projection_watermarks = Some(scope.projection_watermarks.clone());
            }
        }
        provenance
    }

    /// Returns the header length in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.preview().render().len()
    }

    /// Renders the header bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.render().into_bytes()
    }

    /// Returns the directory end offset, or an error on overflow.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::MalformedHeader`] when the offset overflows.
    pub fn directory_end(&self) -> Result<u64, SnapshotError> {
        self.directory_offset
            .checked_add(self.directory_length)
            .ok_or(SnapshotError::MalformedHeader {
                offset: self.directory_offset,
                reason: "the directory range overflows",
            })
    }

    fn preview(&self) -> Self {
        let mut preview = self.clone();
        preview.directory_offset = 0;
        preview.directory_length = 0;
        preview.content_digest = ContentDigest::from_bytes([0; 32]);
        preview.content_seal = ContentSeal::new(
            ContentDigest::from_bytes([0; 32]),
            self.content_seal.lease_term(),
            self.content_seal.signature(),
        );
        preview.lease_term = self.content_seal.lease_term();
        preview
    }

    #[allow(clippy::too_many_lines)]
    fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{FORMAT_IDENTIFIER}/v{FORMAT_MAJOR}");
        let _ = writeln!(
            out,
            "format_version={:0width$}.{:0width$}",
            self.format_version.major(),
            self.format_version.minor(),
            width = U16_WIDTH
        );
        let _ = writeln!(
            out,
            "key_encoding_version={:0width$}",
            self.key_encoding_version.get(),
            width = U16_WIDTH
        );
        let _ = writeln!(
            out,
            "required_features={:0width$x}",
            self.required_features,
            width = FEATURE_WIDTH
        );
        let _ = writeln!(out, "artifact={}", self.artifact().name());
        let _ = writeln!(
            out,
            "manifest_digest={}",
            hex(self.manifest_digest.as_bytes())
        );
        let _ = writeln!(
            out,
            "map_version={:0width$}",
            self.map_version.get(),
            width = U64_WIDTH
        );
        let _ = writeln!(out, "map_digest={}", hex(self.map_digest.as_bytes()));
        match &self.identity {
            HeaderIdentity::Shard(shard) => {
                let _ = writeln!(out, "policy_version=-");
                let _ = writeln!(out, "shard_id={}", hex(shard.shard_id.as_bytes()));
                let _ = writeln!(out, "prefix_lower={}", hex(shard.prefix_bounds.lower()));
                let _ = writeln!(out, "prefix_upper={}", hex(shard.prefix_bounds.upper()));
                let _ = writeln!(
                    out,
                    "commit_version={:0width$}",
                    shard.commit_version.get(),
                    width = U64_WIDTH
                );
                let _ = writeln!(out, "scope_identity=-");
                let _ = writeln!(out, "version_vector=-");
                let _ = writeln!(out, "projection_epoch=-");
                let _ = writeln!(out, "projection_watermarks=-");
            }
            HeaderIdentity::Scope(scope) => {
                let _ = writeln!(
                    out,
                    "policy_version={}",
                    hex(scope.policy_version.as_bytes())
                );
                let _ = writeln!(out, "shard_id=-");
                let _ = writeln!(out, "prefix_lower=-");
                let _ = writeln!(out, "prefix_upper=-");
                let _ = writeln!(out, "commit_version=-");
                let _ = writeln!(
                    out,
                    "scope_identity={}",
                    hex(scope.scope_identity.as_bytes())
                );
                let _ = writeln!(
                    out,
                    "version_vector={}",
                    render_version_vector(&scope.version_vector)
                );
                let _ = writeln!(
                    out,
                    "projection_epoch={:0width$}",
                    scope.projection_epoch.get(),
                    width = U64_WIDTH
                );
                let _ = writeln!(
                    out,
                    "projection_watermarks={}",
                    render_watermarks(&scope.projection_watermarks)
                );
            }
        }
        let _ = writeln!(
            out,
            "page_size={:0width$}",
            self.page_size,
            width = U64_WIDTH
        );
        let _ = writeln!(
            out,
            "page_count={:0width$}",
            self.page_count,
            width = U64_WIDTH
        );
        let _ = writeln!(out, "codec={:0width$}", self.codec.get(), width = U16_WIDTH);
        let _ = writeln!(
            out,
            "directory_codec={:0width$}",
            self.directory_codec.get(),
            width = U16_WIDTH
        );
        let _ = writeln!(
            out,
            "directory_offset={:0width$}",
            self.directory_offset,
            width = U64_WIDTH
        );
        let _ = writeln!(
            out,
            "directory_length={:0width$}",
            self.directory_length,
            width = U64_WIDTH
        );
        let _ = writeln!(
            out,
            "content_digest={}",
            hex(self.content_digest.as_bytes())
        );
        let _ = writeln!(
            out,
            "lease_term={:0width$}",
            self.lease_term.get(),
            width = U64_WIDTH
        );
        let signature = self.content_seal.signature();
        let _ = writeln!(
            out,
            "content_seal=digest:{},lease:{:0width$},signature_present:{},signature:{}",
            hex(self.content_seal.content_digest().as_bytes()),
            self.content_seal.lease_term().get(),
            u8::from(signature.is_some()),
            hex(&signature.map_or([0; 64], |value| *value.as_bytes())),
            width = U64_WIDTH
        );
        let _ = writeln!(out, "{HEADER_END}");
        out
    }

    /// Parses a snapshot header from the start of a byte slice.
    ///
    /// The function returns the header and the body start offset.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::TruncatedSnapshot`] when the header end is
    /// absent. Returns [`SnapshotError::UnsupportedVersion`] for an unknown
    /// major version. Returns [`SnapshotError::MissingRequiredFeature`] for an
    /// unknown required feature bit. Returns [`SnapshotError::MissingProvenance`]
    /// for an absent required field. Returns [`SnapshotError::MalformedHeader`]
    /// for a malformed field.
    pub fn parse(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let marker = find_header_end(bytes)?;
        let text =
            core::str::from_utf8(&bytes[..marker]).map_err(|_| SnapshotError::MalformedHeader {
                offset: 0,
                reason: "the header is not ASCII text",
            })?;
        let mut lines = text.lines();
        let first = lines
            .next()
            .ok_or(SnapshotError::TruncatedSnapshot { offset: 0 })?;
        let first_major = parse_first_line(first)?;
        if first_major != FORMAT_MAJOR {
            return Err(SnapshotError::UnsupportedVersion {
                major: first_major,
                minor: 0,
                offset: 0,
            });
        }
        let fields = parse_fields(lines)?;
        let format_version = parse_format_version(&fields)?;
        if format_version.major() != FORMAT_MAJOR {
            return Err(SnapshotError::UnsupportedVersion {
                major: format_version.major(),
                minor: format_version.minor(),
                offset: 0,
            });
        }
        let required_features = parse_required_hex_u64(&fields, "required_features", 0)?;
        let unknown = required_features & !KNOWN_REQUIRED_FEATURES;
        if unknown != 0 {
            return Err(SnapshotError::MissingRequiredFeature {
                bit: first_set_bit(unknown),
                offset: 0,
            });
        }
        let key_encoding_version = parse_required_u16(&fields, "key_encoding_version", 0)?;
        if key_encoding_version != SUPPORTED_KEY_ENCODING_VERSION {
            return Err(SnapshotError::MalformedHeader {
                offset: 0,
                reason: "the key encoding version is not supported",
            });
        }
        let artifact = field(&fields, "artifact").ok_or(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "the header has no artifact field",
        })?;
        let identity = match artifact {
            "ShardSnapshot" => parse_shard_identity(&fields)?,
            "ScopeSnapshot" => parse_scope_identity(&fields)?,
            _ => {
                return Err(SnapshotError::MalformedHeader {
                    offset: 0,
                    reason: "the artifact name is unknown",
                });
            }
        };
        let manifest_digest =
            parse_provenance_hash(&fields, "manifest_digest", ProvenanceField::ManifestDigest)?;
        let map_version = parse_required_u64(&fields, "map_version", 0)?;
        let map_digest = parse_required_hash(&fields, "map_digest")?;
        let page_size = parse_required_u64(&fields, "page_size", 0)?;
        let page_count = parse_required_u64(&fields, "page_count", 0)?;
        let codec = parse_required_u16(&fields, "codec", 0)?;
        let directory_codec = parse_required_u16(&fields, "directory_codec", 0)?;
        let directory_offset = parse_required_u64(&fields, "directory_offset", 0)?;
        let directory_length = parse_required_u64(&fields, "directory_length", 0)?;
        let content_digest =
            parse_provenance_hash(&fields, "content_digest", ProvenanceField::ContentDigest)?;
        let lease_term = parse_required_u64(&fields, "lease_term", 0)?;
        let content_seal = parse_seal(&fields)?;
        let header = Self {
            format_version,
            key_encoding_version: KeyEncodingVersion::new(key_encoding_version),
            required_features,
            manifest_digest: ManifestDigest::from_bytes(manifest_digest),
            map_version: MapVersion::new(map_version),
            map_digest: MapDigest::from_bytes(map_digest),
            identity,
            page_size,
            page_count,
            codec: CodecId::new(codec),
            directory_codec: CodecId::new(directory_codec),
            directory_offset,
            directory_length,
            content_digest: ContentDigest::from_bytes(content_digest),
            content_seal,
            lease_term: LeaseTerm::new(lease_term),
            text_length: marker,
        };
        header.validate()?;
        Ok(header)
    }

    fn validate(&self) -> Result<(), SnapshotError> {
        if !self.codec.is_identity() || !self.directory_codec.is_identity() {
            return Err(SnapshotError::MalformedHeader {
                offset: 0,
                reason: "the codec identifier is unknown",
            });
        }
        if self.page_size == 0 || self.page_size > crate::MAX_PAGE_SIZE {
            return Err(SnapshotError::PageTooLarge {
                page_id: oxkv_contracts::version::PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        if self.page_count > crate::MAX_PAGES {
            return Err(SnapshotError::PageTooLarge {
                page_id: oxkv_contracts::version::PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        if self.directory_length > crate::MAX_DIRECTORY_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id: oxkv_contracts::version::PageId::from_bytes([0; 32]),
                offset: self.directory_offset,
            });
        }
        if self.directory_offset < u64::try_from(self.text_length).unwrap_or(u64::MAX) {
            return Err(SnapshotError::MalformedHeader {
                offset: self.directory_offset,
                reason: "the directory starts inside the header",
            });
        }
        let provenance = self.provenance();
        provenance
            .validate(self.artifact())
            .map_err(|error| match error {
                oxkv_contracts::version::ProvenanceError::MissingField { field } => {
                    SnapshotError::MissingProvenance { field }
                }
            })
    }
}

fn find_header_end(bytes: &[u8]) -> Result<usize, SnapshotError> {
    let limit = bytes.len().min(crate::MAX_HEADER_BYTES);
    let needle = b"header_end\n";
    let mut index = 0;
    while index + needle.len() <= limit {
        if &bytes[index..index + needle.len()] == needle {
            return Ok(index + needle.len());
        }
        index += 1;
    }
    if bytes.len() < crate::MAX_HEADER_BYTES {
        Err(SnapshotError::TruncatedSnapshot {
            offset: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        })
    } else {
        Err(SnapshotError::MalformedHeader {
            offset: u64::try_from(limit).unwrap_or(u64::MAX),
            reason: "the header is too large",
        })
    }
}

fn parse_first_line(line: &str) -> Result<u16, SnapshotError> {
    let version = line
        .strip_prefix(FORMAT_IDENTIFIER)
        .and_then(|rest| rest.strip_prefix("/v"))
        .ok_or(SnapshotError::UnsupportedVersion {
            major: 0,
            minor: 0,
            offset: 0,
        })?;
    version
        .parse::<u16>()
        .map_err(|_| SnapshotError::MalformedHeader {
            offset: 0,
            reason: "the format identifier is malformed",
        })
}

fn parse_fields<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> Result<Vec<(&'a str, &'a str)>, SnapshotError> {
    let mut fields = Vec::new();
    for line in lines {
        if line == HEADER_END {
            break;
        }
        let (name, value) = line.split_once('=').ok_or(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "a header line has no field separator",
        })?;
        if name.is_empty() {
            return Err(SnapshotError::MalformedHeader {
                offset: 0,
                reason: "a header field name is empty",
            });
        }
        if fields.iter().any(|(existing, _)| *existing == name) {
            return Err(SnapshotError::MalformedHeader {
                offset: 0,
                reason: "a header field appears more than one time",
            });
        }
        fields.push((name, value));
    }
    Ok(fields)
}

fn field<'a>(fields: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(field_name, _)| *field_name == name)
        .map(|(_, value)| *value)
}

fn parse_format_version(fields: &[(&str, &str)]) -> Result<SnapshotPageVersion, SnapshotError> {
    let text = field(fields, "format_version").ok_or(SnapshotError::MissingProvenance {
        field: ProvenanceField::FormatVersion,
    })?;
    text.parse::<SnapshotPageVersion>()
        .map_err(|_| SnapshotError::MalformedHeader {
            offset: 0,
            reason: "the format version text is malformed",
        })
}

fn parse_required_u64(
    fields: &[(&str, &str)],
    name: &str,
    offset: u64,
) -> Result<u64, SnapshotError> {
    let text = field(fields, name).ok_or(SnapshotError::MalformedHeader {
        offset,
        reason: "the header has no required numeric field",
    })?;
    parse_u64(text, offset)
}

fn parse_required_u16(
    fields: &[(&str, &str)],
    name: &str,
    offset: u64,
) -> Result<u16, SnapshotError> {
    let text = field(fields, name).ok_or(SnapshotError::MalformedHeader {
        offset,
        reason: "the header has no required numeric field",
    })?;
    text.parse::<u16>()
        .map_err(|_| SnapshotError::MalformedHeader {
            offset,
            reason: "a numeric field is malformed",
        })
}

fn parse_required_hex_u64(
    fields: &[(&str, &str)],
    name: &str,
    offset: u64,
) -> Result<u64, SnapshotError> {
    let text = field(fields, name).ok_or(SnapshotError::MalformedHeader {
        offset,
        reason: "the header has no required numeric field",
    })?;
    u64::from_str_radix(text, 16).map_err(|_| SnapshotError::MalformedHeader {
        offset,
        reason: "a hexadecimal numeric field is malformed",
    })
}

fn parse_required_hash(fields: &[(&str, &str)], name: &str) -> Result<[u8; 32], SnapshotError> {
    let text = field(fields, name).ok_or(SnapshotError::MalformedHeader {
        offset: 0,
        reason: "the header has no required hash field",
    })?;
    parse_hash(text, 0)
}

fn parse_provenance_hash(
    fields: &[(&str, &str)],
    name: &str,
    field_name: ProvenanceField,
) -> Result<[u8; 32], SnapshotError> {
    let text = field(fields, name).ok_or(SnapshotError::MissingProvenance { field: field_name })?;
    parse_hash(text, 0)
}

fn parse_signature(text: &str, offset: u64) -> Result<[u8; 64], SnapshotError> {
    let bytes = parse_hex(text, offset)?;
    bytes
        .try_into()
        .map_err(|_| SnapshotError::MalformedHeader {
            offset,
            reason: "the signature field has the wrong length",
        })
}

fn parse_u64(text: &str, offset: u64) -> Result<u64, SnapshotError> {
    text.parse::<u64>()
        .map_err(|_| SnapshotError::MalformedHeader {
            offset,
            reason: "a numeric field is malformed",
        })
}

fn parse_hash(text: &str, offset: u64) -> Result<[u8; 32], SnapshotError> {
    let bytes = parse_hex(text, offset)?;
    bytes
        .try_into()
        .map_err(|_| SnapshotError::MalformedHeader {
            offset,
            reason: "a hash field has the wrong length",
        })
}

fn parse_hex(text: &str, offset: u64) -> Result<Vec<u8>, SnapshotError> {
    if !text.len().is_multiple_of(2) {
        return Err(SnapshotError::MalformedHeader {
            offset,
            reason: "a hexadecimal field has an odd length",
        });
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let raw = text.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        let high = hex_digit(raw[index]).ok_or(SnapshotError::MalformedHeader {
            offset,
            reason: "a hexadecimal field has a non-hex character",
        })?;
        let low = hex_digit(raw[index + 1]).ok_or(SnapshotError::MalformedHeader {
            offset,
            reason: "a hexadecimal field has a non-hex character",
        })?;
        bytes.push((high << 4) | low);
        index += 2;
    }
    Ok(bytes)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn parse_shard_identity(fields: &[(&str, &str)]) -> Result<HeaderIdentity, SnapshotError> {
    let shard_id = field(fields, "shard_id")
        .map(|text| parse_hex(text, 0))
        .transpose()?
        .ok_or(SnapshotError::MissingProvenance {
            field: ProvenanceField::ShardId,
        })?;
    let lower = field(fields, "prefix_lower")
        .map(|text| parse_hex(text, 0))
        .transpose()?;
    let upper = field(fields, "prefix_upper")
        .map(|text| parse_hex(text, 0))
        .transpose()?;
    let (Some(lower), Some(upper)) = (lower, upper) else {
        return Err(SnapshotError::MissingProvenance {
            field: ProvenanceField::PrefixBounds,
        });
    };
    let commit_version = field(fields, "commit_version")
        .map(|text| parse_u64(text, 0))
        .transpose()?
        .ok_or(SnapshotError::MissingProvenance {
            field: ProvenanceField::CommitVersion,
        })?;
    Ok(HeaderIdentity::Shard(ShardHeader {
        shard_id: ShardId::new(shard_id),
        prefix_bounds: PrefixBounds::new(lower, upper),
        commit_version: CommitVersion::new(commit_version),
    }))
}

fn parse_scope_identity(fields: &[(&str, &str)]) -> Result<HeaderIdentity, SnapshotError> {
    let scope_identity = field(fields, "scope_identity")
        .map(|text| parse_hash(text, 0))
        .transpose()?
        .ok_or(SnapshotError::MissingProvenance {
            field: ProvenanceField::ScopeIdentity,
        })?;
    let policy_version = field(fields, "policy_version")
        .map(|text| parse_hash(text, 0))
        .transpose()?
        .ok_or(SnapshotError::MissingProvenance {
            field: ProvenanceField::PolicyVersion,
        })?;
    let version_vector = parse_version_vector(field(fields, "version_vector").ok_or(
        SnapshotError::MissingProvenance {
            field: ProvenanceField::VersionVector,
        },
    )?)?;
    let projection_epoch = field(fields, "projection_epoch")
        .map(|text| parse_u64(text, 0))
        .transpose()?
        .ok_or(SnapshotError::MissingProvenance {
            field: ProvenanceField::ProjectionEpoch,
        })?;
    let projection_watermarks = parse_watermarks(field(fields, "projection_watermarks").ok_or(
        SnapshotError::MissingProvenance {
            field: ProvenanceField::ProjectionWatermarks,
        },
    )?)?;
    Ok(HeaderIdentity::Scope(ScopeHeader {
        scope_identity: ScopeIdentity::from_bytes(scope_identity),
        policy_version: PolicyVersion::from_bytes(policy_version),
        version_vector,
        projection_epoch: ProjectionEpoch::new(projection_epoch),
        projection_watermarks,
    }))
}

fn parse_seal(fields: &[(&str, &str)]) -> Result<ContentSeal, SnapshotError> {
    let text = field(fields, "content_seal").ok_or(SnapshotError::MissingProvenance {
        field: ProvenanceField::ContentSeal,
    })?;
    let mut digest = None;
    let mut lease = None;
    let mut present = None;
    let mut signature = None;
    for part in text.split(',') {
        let (name, value) = part.split_once(':').ok_or(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "the content seal is malformed",
        })?;
        match name {
            "digest" => digest = Some(parse_hash(value, 0)?),
            "lease" => lease = Some(parse_u64(value, 0)?),
            "signature_present" => {
                present = Some(match value {
                    "0" => false,
                    "1" => true,
                    _ => {
                        return Err(SnapshotError::MalformedHeader {
                            offset: 0,
                            reason: "the signature flag is malformed",
                        });
                    }
                });
            }
            "signature" => signature = Some(parse_signature(value, 0)?),
            _ => {
                return Err(SnapshotError::MalformedHeader {
                    offset: 0,
                    reason: "the content seal has an unknown field",
                });
            }
        }
    }
    let digest = digest.ok_or(SnapshotError::MalformedHeader {
        offset: 0,
        reason: "the content seal has no digest",
    })?;
    let lease = lease.ok_or(SnapshotError::MalformedHeader {
        offset: 0,
        reason: "the content seal has no lease term",
    })?;
    let present = present.ok_or(SnapshotError::MalformedHeader {
        offset: 0,
        reason: "the content seal has no signature flag",
    })?;
    let signature = signature.ok_or(SnapshotError::MalformedHeader {
        offset: 0,
        reason: "the content seal has no signature",
    })?;
    let signature = if present {
        Some(Signature::from_bytes(signature))
    } else {
        None
    };
    Ok(ContentSeal::new(
        ContentDigest::from_bytes(digest),
        LeaseTerm::new(lease),
        signature,
    ))
}

fn parse_version_vector(text: &str) -> Result<ScopeVersionVector, SnapshotError> {
    if text == "-" {
        return Ok(ScopeVersionVector::new());
    }
    let mut entries = Vec::new();
    for part in text.split(',') {
        let (shard, version) = part.split_once(':').ok_or(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "a version vector entry is malformed",
        })?;
        let shard = parse_hex(shard, 0)?;
        if shard.is_empty() {
            return Err(SnapshotError::MalformedHeader {
                offset: 0,
                reason: "a version vector shard is empty",
            });
        }
        let version = parse_u64(version, 0)?;
        entries.push((ShardId::new(shard), CommitVersion::new(version)));
    }
    let mut vector = VersionVector::new();
    for (shard, version) in entries {
        vector.set(shard, version);
    }
    let expected = text.split(',').count();
    if vector.len() != expected {
        return Err(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "a version vector shard is duplicated",
        });
    }
    Ok(ScopeVersionVector::from_vector(vector))
}

fn parse_watermarks(text: &str) -> Result<ProjectionWatermarks, SnapshotError> {
    if text == "-" {
        return Ok(ProjectionWatermarks::new());
    }
    let mut watermarks = ProjectionWatermarks::new();
    let mut count = 0;
    for part in text.split(',') {
        let (source, version) = part.split_once(':').ok_or(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "a projection watermark is malformed",
        })?;
        let source = parse_hex(source, 0)?;
        let source = String::from_utf8(source).map_err(|_| SnapshotError::MalformedHeader {
            offset: 0,
            reason: "a fact source is not UTF-8",
        })?;
        let version = parse_u64(version, 0)?;
        watermarks.set(FactSourceId::new(source), FactVersion::new(version));
        count += 1;
    }
    let mut unique = 0;
    let mut previous: Option<&str> = None;
    for (source, _) in watermarks.iter() {
        let current = source.as_str();
        if previous == Some(current) {
            return Err(SnapshotError::MalformedHeader {
                offset: 0,
                reason: "a projection watermark is duplicated",
            });
        }
        previous = Some(current);
        unique += 1;
    }
    if unique != count {
        return Err(SnapshotError::MalformedHeader {
            offset: 0,
            reason: "a projection watermark is duplicated",
        });
    }
    Ok(watermarks)
}

fn render_version_vector(vector: &ScopeVersionVector) -> String {
    let mut parts = Vec::new();
    for (shard_id, commit_version) in vector.as_vector().iter() {
        parts.push(format!(
            "{}:{commit_version:0width$}",
            hex(shard_id.as_bytes()),
            width = U64_WIDTH
        ));
    }
    if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join(",")
    }
}

fn render_watermarks(watermarks: &ProjectionWatermarks) -> String {
    let mut parts = Vec::new();
    for (source, version) in watermarks.iter() {
        parts.push(format!(
            "{}:{version:0width$}",
            hex(source.as_str().as_bytes()),
            width = U64_WIDTH
        ));
    }
    if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join(",")
    }
}

fn first_set_bit(value: u64) -> u8 {
    let mut bit = 0;
    while bit < 64 {
        if value & (1 << bit) != 0 {
            return bit;
        }
        bit += 1;
    }
    0
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}
