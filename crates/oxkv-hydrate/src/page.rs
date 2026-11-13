//! One verified snapshot page and its checksum.
//!
//! A page carries its format version, required features, identity, sequence,
//! shard provenance, key bounds, record count, codec, logical length, CRC32C,
//! and payload. The page identity is a deterministic BLAKE3 hash of the shard,
//! the commit version, the page sequence, and the key range.

use oxkv_contracts::version::{
    CommitVersion, ContentDigest, PageId, PageSequence, PrefixBounds, ShardId, SnapshotPageVersion,
};

use crate::encoding::{DecodeError, Reader, Writer};
use crate::error::HydrationError;

/// The page format major version.
pub const PAGE_FORMAT_MAJOR: u16 = 1;

/// The page format minor version.
pub const PAGE_FORMAT_MINOR: u16 = 0;

/// The feature bits that this reader implements.
pub const KNOWN_REQUIRED_FEATURES: u64 = 0b1;

/// The page magic bytes.
const PAGE_MAGIC: [u8; 4] = *b"OXPG";

/// The page payload holds canonical records.
pub const CODEC_RECORDS: u8 = 0;

/// The page payload is opaque bytes.
pub const CODEC_OPAQUE: u8 = 1;

/// The page payload models a larger logical page.
pub const CODEC_SYNTHETIC: u8 = 2;

const CRC32C_POLYNOMIAL: u32 = 0x82F6_3B78;

/// Computes CRC32C over the bytes.
///
/// The function uses the Castagnoli polynomial. The registry reserves CRC32C
/// for page corruption detection.
#[must_use]
pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (CRC32C_POLYNOMIAL & mask);
        }
    }
    !crc
}

/// One key and value record in a page payload.
pub type Record = (Vec<u8>, Vec<u8>);

/// One snapshot page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPage {
    /// The page format version.
    pub format_version: SnapshotPageVersion,
    /// The required feature bits.
    pub required_features: u64,
    /// The deterministic page identity.
    pub page_id: PageId,
    /// The strictly increasing page sequence.
    pub page_sequence: PageSequence,
    /// The shard that owns the page.
    pub shard_id: ShardId,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The inclusive lower and exclusive upper key bounds.
    pub bounds: PrefixBounds,
    /// The number of records in the payload.
    pub record_count: u32,
    /// The payload codec identifier.
    pub codec: u8,
    /// The logical uncompressed length.
    pub uncompressed_length: u64,
    /// The page payload bytes.
    pub payload: Vec<u8>,
}

impl SnapshotPage {
    /// Creates one page and computes its identity.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        page_sequence: u64,
        shard_id: ShardId,
        commit_version: CommitVersion,
        bounds: PrefixBounds,
        record_count: u32,
        codec: u8,
        uncompressed_length: u64,
        payload: Vec<u8>,
    ) -> Self {
        let page_id = deterministic_page_id(
            &shard_id,
            commit_version,
            PageSequence::new(page_sequence),
            &bounds,
        );
        Self {
            format_version: SnapshotPageVersion::new(PAGE_FORMAT_MAJOR, PAGE_FORMAT_MINOR),
            required_features: 0,
            page_id,
            page_sequence: PageSequence::new(page_sequence),
            shard_id,
            commit_version,
            bounds,
            record_count,
            codec,
            uncompressed_length,
            payload,
        }
    }

    /// Creates one record page with the sample shard and commit version.
    #[allow(clippy::needless_pass_by_value)]
    #[must_use]
    pub fn with_records(page_sequence: u64, records: Vec<Record>, bounds: PrefixBounds) -> Self {
        Self::record_page(
            page_sequence,
            ShardId::new(vec![1]),
            CommitVersion::new(1),
            bounds,
            &records,
        )
    }

    /// Creates one record page for a shard and commit version.
    #[must_use]
    pub fn record_page(
        page_sequence: u64,
        shard_id: ShardId,
        commit_version: CommitVersion,
        bounds: PrefixBounds,
        records: &[Record],
    ) -> Self {
        let payload = encode_records(records);
        let record_count = u32::try_from(records.len()).unwrap_or(u32::MAX);
        let length = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        Self::new(
            page_sequence,
            shard_id,
            commit_version,
            bounds,
            record_count,
            CODEC_RECORDS,
            length,
            payload,
        )
    }

    /// Creates one synthetic page with a logical length and a small payload.
    ///
    /// Use this constructor to model a large scope without large allocations.
    #[must_use]
    pub fn synthetic(
        page_sequence: u64,
        shard_id: ShardId,
        commit_version: CommitVersion,
        bounds: PrefixBounds,
        logical_length: u64,
    ) -> Self {
        let payload = format!("synthetic-page-{page_sequence}").into_bytes();
        Self::new(
            page_sequence,
            shard_id,
            commit_version,
            bounds,
            0,
            CODEC_SYNTHETIC,
            logical_length,
            payload,
        )
    }

    /// Encodes the page into canonical bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::new();
        writer.raw(&PAGE_MAGIC);
        writer.u16(self.format_version.major());
        writer.u16(self.format_version.minor());
        writer.u64(self.required_features);
        writer.hash32(self.page_id.as_bytes());
        writer.u64(self.page_sequence.get());
        writer.bytes(self.shard_id.as_bytes());
        writer.u64(self.commit_version.get());
        writer.bytes(self.bounds.lower());
        writer.bytes(self.bounds.upper());
        writer.u32(self.record_count);
        writer.u8(self.codec);
        writer.u64(self.uncompressed_length);
        writer.u32(crc32c(&self.payload));
        writer.bytes(&self.payload);
        writer.into_bytes()
    }

    /// Returns the BLAKE3 digest of the canonical page bytes.
    #[must_use]
    pub fn digest(&self) -> ContentDigest {
        ContentDigest::of_bytes(&self.encode())
    }

    /// Decodes and verifies one page frame.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::UnsupportedVersion`] for an unknown major
    /// version. Returns [`HydrationError::MissingRequiredFeature`] for an
    /// unknown required feature. Returns [`HydrationError::CorruptPage`] for a
    /// bad magic value, a bad page identity, or a bad CRC32C checksum. Returns
    /// [`HydrationError::TruncatedSnapshot`] for a short frame.
    pub fn decode(bytes: &[u8]) -> Result<Self, HydrationError> {
        let mut reader = Reader::new(bytes);
        let magic = reader.exact(4).map_err(corrupt_page)?;
        if magic != PAGE_MAGIC {
            return Err(HydrationError::CorruptPage {
                page_id: None,
                byte_offset: 0,
            });
        }
        let major = reader.u16().map_err(corrupt_page)?;
        let minor = reader.u16().map_err(corrupt_page)?;
        let format_version = SnapshotPageVersion::new(major, minor);
        if major != PAGE_FORMAT_MAJOR {
            return Err(HydrationError::UnsupportedVersion {
                found: format_version,
                supported_major: PAGE_FORMAT_MAJOR,
            });
        }
        let required_features = reader.u64().map_err(corrupt_page)?;
        check_features(required_features)?;
        let page_id = PageId::from_bytes(reader.hash32().map_err(corrupt_page)?);
        let page_sequence = PageSequence::new(reader.u64().map_err(corrupt_page)?);
        let shard_id = ShardId::new(reader.bytes().map_err(corrupt_page)?.to_vec());
        let commit_version = CommitVersion::new(reader.u64().map_err(corrupt_page)?);
        let bounds = PrefixBounds::new(
            reader.bytes().map_err(corrupt_page)?.to_vec(),
            reader.bytes().map_err(corrupt_page)?.to_vec(),
        );
        let record_count = reader.u32().map_err(corrupt_page)?;
        let codec = reader.u8().map_err(corrupt_page)?;
        let uncompressed_length = reader.u64().map_err(corrupt_page)?;
        let expected_crc = reader.u32().map_err(corrupt_page)?;
        let payload_offset = reader.offset();
        let payload = reader.bytes().map_err(corrupt_page)?.to_vec();
        if crc32c(&payload) != expected_crc {
            return Err(HydrationError::CorruptPage {
                page_id: Some(page_id),
                byte_offset: payload_offset,
            });
        }
        let expected_id = deterministic_page_id(&shard_id, commit_version, page_sequence, &bounds);
        if page_id != expected_id {
            return Err(HydrationError::CorruptPage {
                page_id: Some(page_id),
                byte_offset: 0,
            });
        }
        if bounds.is_empty() {
            return Err(HydrationError::PageOutOfBounds { page_sequence });
        }
        reader.finish().map_err(corrupt_page)?;
        Ok(Self {
            format_version,
            required_features,
            page_id,
            page_sequence,
            shard_id,
            commit_version,
            bounds,
            record_count,
            codec,
            uncompressed_length,
            payload,
        })
    }

    /// Decodes the records in the payload.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::CorruptPage`] when the payload does not match
    /// the record count.
    pub fn records(&self) -> Result<Vec<Record>, HydrationError> {
        if self.record_count == 0 {
            return Ok(Vec::new());
        }
        let mut reader = Reader::new(&self.payload);
        let count = reader.u32().map_err(|_| HydrationError::CorruptPage {
            page_id: Some(self.page_id),
            byte_offset: 0,
        })?;
        if count != self.record_count {
            return Err(HydrationError::CorruptPage {
                page_id: Some(self.page_id),
                byte_offset: 0,
            });
        }
        let mut records = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        for _ in 0..count {
            let key = reader
                .bytes()
                .map_err(|_| HydrationError::CorruptPage {
                    page_id: Some(self.page_id),
                    byte_offset: 0,
                })?
                .to_vec();
            let value = reader
                .bytes()
                .map_err(|_| HydrationError::CorruptPage {
                    page_id: Some(self.page_id),
                    byte_offset: 0,
                })?
                .to_vec();
            records.push((key, value));
        }
        reader.finish().map_err(|_| HydrationError::CorruptPage {
            page_id: Some(self.page_id),
            byte_offset: 0,
        })?;
        Ok(records)
    }

    /// Checks that the page key bounds fit inside the snapshot bounds.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::PageOutOfBounds`] when the page lies outside
    /// the snapshot bounds.
    pub fn check_bounds(&self, snapshot: &PrefixBounds) -> Result<(), HydrationError> {
        let inside = self.bounds.lower() >= snapshot.lower()
            && self.bounds.upper() <= snapshot.upper()
            && !self.bounds.is_empty();
        if inside {
            Ok(())
        } else {
            Err(HydrationError::PageOutOfBounds {
                page_sequence: self.page_sequence,
            })
        }
    }
}

/// Checks that every required feature bit is known.
pub(crate) fn check_features(required_features: u64) -> Result<(), HydrationError> {
    let unknown = required_features & !KNOWN_REQUIRED_FEATURES;
    if unknown == 0 {
        return Ok(());
    }
    let lowest = unknown.trailing_zeros();
    Err(HydrationError::MissingRequiredFeature {
        feature: 1_u64 << lowest,
    })
}

fn corrupt_page(error: DecodeError) -> HydrationError {
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

/// Computes the deterministic page identity.
///
/// The hash covers the shard identifier, the commit version, the page
/// sequence, and the key bounds. The bytes use a fixed domain tag.
#[must_use]
fn deterministic_page_id(
    shard_id: &ShardId,
    commit_version: CommitVersion,
    page_sequence: PageSequence,
    bounds: &PrefixBounds,
) -> PageId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"oxkv/page-id/v1\0");
    hasher.update(
        &u64::try_from(shard_id.as_bytes().len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(shard_id.as_bytes());
    hasher.update(&commit_version.get().to_le_bytes());
    hasher.update(&page_sequence.get().to_le_bytes());
    hasher.update(
        &u64::try_from(bounds.lower().len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(bounds.lower());
    hasher.update(
        &u64::try_from(bounds.upper().len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(bounds.upper());
    PageId::from_bytes(*hasher.finalize().as_bytes())
}

/// Encodes records into a canonical payload.
#[must_use]
fn encode_records(records: &[Record]) -> Vec<u8> {
    let mut writer = Writer::new();
    writer.u32(u32::try_from(records.len()).unwrap_or(u32::MAX));
    for (key, value) in records {
        writer.bytes(key);
        writer.bytes(value);
    }
    writer.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_matches_the_castagnoli_vector() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn a_page_round_trips() {
        let page = SnapshotPage::with_records(
            0,
            vec![(b"alpha".to_vec(), b"one".to_vec())],
            PrefixBounds::new(vec![b'a'], vec![b'z']),
        );
        let decoded = SnapshotPage::decode(&page.encode()).expect("the page decodes");
        assert_eq!(decoded, page);
        assert_eq!(decoded.records().expect("records decode").len(), 1);
    }

    #[test]
    fn a_changed_payload_returns_a_corrupt_page() {
        let page = SnapshotPage::with_records(
            0,
            vec![(b"alpha".to_vec(), b"one".to_vec())],
            PrefixBounds::new(vec![b'a'], vec![b'z']),
        );
        let mut bytes = page.encode();
        let last = bytes.len() - 2;
        bytes[last] ^= 0xff;
        let error = SnapshotPage::decode(&bytes).expect_err("the payload change fails");
        assert_eq!(error.code(), "format.corrupt_page");
        match error {
            HydrationError::CorruptPage {
                page_id,
                byte_offset,
            } => {
                assert_eq!(page_id, Some(page.page_id));
                assert!(byte_offset > 0);
            }
            other => panic!("unexpected error {other}"),
        }
    }

    #[test]
    fn a_changed_page_identity_returns_a_corrupt_page() {
        let page = SnapshotPage::with_records(
            0,
            vec![(b"alpha".to_vec(), b"one".to_vec())],
            PrefixBounds::new(vec![b'a'], vec![b'z']),
        );
        let mut bytes = page.encode();
        bytes[4 + 2 + 2 + 8] ^= 0xff;
        assert_eq!(
            SnapshotPage::decode(&bytes)
                .expect_err("the identity change fails")
                .code(),
            "format.corrupt_page"
        );
    }

    #[test]
    fn a_short_page_returns_a_truncated_snapshot() {
        let page = SnapshotPage::with_records(
            0,
            vec![(b"alpha".to_vec(), b"one".to_vec())],
            PrefixBounds::new(vec![b'a'], vec![b'z']),
        );
        let bytes = page.encode();
        let error =
            SnapshotPage::decode(&bytes[..bytes.len() - 1]).expect_err("the short page fails");
        assert_eq!(error.code(), "format.truncated_snapshot");
    }
}
