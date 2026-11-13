//! Binary snapshot pages.
//!
//! A page carries its identity, sequence, shard, commit version, key range,
//! record count, codec, uncompressed length, and CRC32C. The payload holds
//! canonical records in strictly ascending key order.
//!
//! The page identity is a `BLAKE3` hash of the shard identifier, the commit
//! version, the page sequence, and the key range. The identity does not depend
//! on the byte offset.

use oxkv_contracts::version::{CommitVersion, PageId, PageSequence, ShardId};

use crate::codec::CodecId;
use crate::error::SnapshotError;
use crate::{MAX_KEY_BYTES, MAX_PAGE_SIZE, MAX_RECORD_COUNT, MAX_SHARD_ID_BYTES, MAX_VALUE_BYTES};

/// The page frame magic bytes.
pub const PAGE_MAGIC: &[u8; 4] = b"OXPG";

const PAGE_ID_TAG: &str = "oxkv/page-id/v1";

/// One encoded key and value pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Record {
    /// The encoded key bytes.
    pub key: Vec<u8>,
    /// The encoded value bytes.
    pub value: Vec<u8>,
}

impl Record {
    /// Creates a record.
    #[must_use]
    pub fn new(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// One binary snapshot page.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The deterministic page identity.
    pub page_id: PageId,
    /// The page sequence. It is strictly increasing in one snapshot.
    pub page_sequence: PageSequence,
    /// The shard that owns the page.
    pub shard_id: ShardId,
    /// The commit version of the page.
    pub commit_version: CommitVersion,
    /// The first key in the payload.
    pub first_key: Vec<u8>,
    /// The last key in the payload.
    pub last_key: Vec<u8>,
    /// The number of records in the payload.
    pub record_count: u32,
    /// The payload codec identifier.
    pub codec: CodecId,
    /// The uncompressed payload length.
    pub uncompressed_length: u64,
    /// The CRC32C of the page frame without the CRC field.
    pub integrity: u32,
    /// The records in strictly ascending key order.
    pub records: Vec<Record>,
}

impl Page {
    /// Builds one page from records.
    ///
    /// The function sorts no record. The records must already use strictly
    /// ascending key order.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::CorruptPage`] for an empty page, an unsorted
    /// key, or a duplicate key. Returns [`SnapshotError::PageTooLarge`] for a
    /// length over its limit.
    pub fn new(
        page_sequence: PageSequence,
        shard_id: ShardId,
        commit_version: CommitVersion,
        records: Vec<Record>,
        codec: CodecId,
    ) -> Result<Self, SnapshotError> {
        if !codec.is_identity() {
            return Err(SnapshotError::MalformedHeader {
                offset: 0,
                reason: "the page codec is unknown",
            });
        }
        if records.is_empty() {
            return Err(corrupt(PageId::from_bytes([0; 32]), 0, "the page is empty"));
        }
        if shard_id.as_bytes().len() > MAX_SHARD_ID_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        if records.len() > MAX_RECORD_COUNT as usize {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        let payload = encode_records(&records)?;
        if payload.len() as u64 > MAX_PAGE_SIZE {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        let first_key = records
            .first()
            .map(|record| record.key.clone())
            .unwrap_or_default();
        let last_key = records
            .last()
            .map(|record| record.key.clone())
            .unwrap_or_default();
        let page_id = page_id_for(
            &shard_id,
            commit_version,
            page_sequence,
            &first_key,
            &last_key,
        );
        let record_count = u32::try_from(records.len())
            .map_err(|_| SnapshotError::PageTooLarge { page_id, offset: 0 })?;
        let mut page = Self {
            page_id,
            page_sequence,
            shard_id,
            commit_version,
            first_key,
            last_key,
            record_count,
            codec,
            uncompressed_length: payload.len() as u64,
            integrity: 0,
            records,
        };
        let frame = page.encode_frame()?;
        page.integrity = crc32fast::hash(&frame);
        Ok(page)
    }

    /// Returns the payload byte length.
    #[must_use]
    pub fn payload_len(&self) -> u64 {
        self.uncompressed_length
    }

    /// Encodes the page frame.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::PageTooLarge`] when a length field is over its
    /// limit.
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        let mut frame = self.encode_frame()?;
        let integrity = crc32fast::hash(&frame);
        frame.extend_from_slice(&integrity.to_be_bytes());
        Ok(frame)
    }

    /// Returns the stored page integrity value.
    #[must_use]
    pub const fn integrity(&self) -> u32 {
        self.integrity
    }

    fn encode_frame(&self) -> Result<Vec<u8>, SnapshotError> {
        let payload = encode_records(&self.records)?;
        let mut out = Vec::with_capacity(payload.len() + 128);
        out.extend_from_slice(PAGE_MAGIC);
        out.extend_from_slice(&crate::FORMAT_MAJOR.to_be_bytes());
        out.extend_from_slice(&crate::FORMAT_MINOR.to_be_bytes());
        out.extend_from_slice(self.page_id.as_bytes());
        out.extend_from_slice(&self.page_sequence.get().to_be_bytes());
        push_bytes(&mut out, self.shard_id.as_bytes(), self.page_id, 0)?;
        out.extend_from_slice(&self.commit_version.get().to_be_bytes());
        push_bytes(&mut out, &self.first_key, self.page_id, 0)?;
        push_bytes(&mut out, &self.last_key, self.page_id, 0)?;
        out.extend_from_slice(&self.record_count.to_be_bytes());
        out.extend_from_slice(&self.codec.get().to_be_bytes());
        out.extend_from_slice(&self.uncompressed_length.to_be_bytes());
        out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// Decodes and verifies one page frame.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::CorruptPage`] for a bad magic, a bad CRC, a bad
    /// identity, an unsorted key, or a length mismatch. Returns
    /// [`SnapshotError::PageTooLarge`] for a length over its limit.
    #[allow(clippy::too_many_lines)]
    pub fn decode(bytes: &[u8], offset: u64) -> Result<Self, SnapshotError> {
        if bytes.len() < 4 {
            return Err(corrupt(
                PageId::from_bytes([0; 32]),
                offset,
                "the page is too short",
            ));
        }
        let mut reader = Reader::new(bytes, offset);
        let magic = reader.take(4)?;
        if magic != PAGE_MAGIC {
            return Err(corrupt(
                PageId::from_bytes([0; 32]),
                offset,
                "the page magic is wrong",
            ));
        }
        let major = reader.u16()?;
        let minor = reader.u16()?;
        if major != crate::FORMAT_MAJOR || minor > crate::FORMAT_MINOR {
            return Err(corrupt(
                PageId::from_bytes([0; 32]),
                offset,
                "the page version is not supported",
            ));
        }
        let page_id = PageId::from_bytes(reader.array32()?);
        reader.page_id = page_id;
        let page_sequence = PageSequence::new(reader.u64()?);
        let shard_len = reader.u32_as_usize()?;
        if shard_len > MAX_SHARD_ID_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id,
                offset: reader.absolute(),
            });
        }
        let shard_id = ShardId::new(reader.take(shard_len)?.to_vec());
        let commit_version = CommitVersion::new(reader.u64()?);
        let first_len = reader.u32_as_usize()?;
        if first_len > MAX_KEY_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id,
                offset: reader.absolute(),
            });
        }
        let first_key = reader.take(first_len)?.to_vec();
        let last_len = reader.u32_as_usize()?;
        if last_len > MAX_KEY_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id,
                offset: reader.absolute(),
            });
        }
        let last_key = reader.take(last_len)?.to_vec();
        let record_count = reader.u32()?;
        if record_count > MAX_RECORD_COUNT {
            return Err(SnapshotError::PageTooLarge {
                page_id,
                offset: reader.absolute(),
            });
        }
        let codec = CodecId::new(reader.u16()?);
        let uncompressed_length = reader.u64()?;
        let payload_len = reader.u64()?;
        if payload_len > MAX_PAGE_SIZE {
            return Err(SnapshotError::PageTooLarge {
                page_id,
                offset: reader.absolute(),
            });
        }
        let payload_len =
            usize::try_from(payload_len).map_err(|_| SnapshotError::PageTooLarge {
                page_id,
                offset: reader.absolute(),
            })?;
        let payload = reader.take(payload_len)?;
        let stored_integrity = reader.u32()?;
        if reader.remaining() != 0 {
            return Err(corrupt(
                page_id,
                reader.absolute(),
                "the page has trailing bytes",
            ));
        }
        let crc_start = bytes.len() - 4;
        let computed_integrity = crc32fast::hash(&bytes[..crc_start]);
        if computed_integrity != stored_integrity {
            return Err(corrupt(page_id, offset, "the page CRC does not match"));
        }
        if !codec.is_identity() {
            return Err(corrupt(page_id, offset, "the page codec is unknown"));
        }
        if uncompressed_length != payload_len as u64 {
            return Err(corrupt(
                page_id,
                offset,
                "the uncompressed length does not match",
            ));
        }
        let records = decode_records(payload, page_id, offset)?;
        let record_count_actual = u32::try_from(records.len())
            .map_err(|_| SnapshotError::PageTooLarge { page_id, offset })?;
        if record_count_actual != record_count {
            return Err(corrupt(page_id, offset, "the record count does not match"));
        }
        let actual_first = records.first().map_or(&[][..], |record| &record.key[..]);
        let actual_last = records.last().map_or(&[][..], |record| &record.key[..]);
        if actual_first != first_key.as_slice() || actual_last != last_key.as_slice() {
            return Err(corrupt(page_id, offset, "the key range does not match"));
        }
        let computed_id = page_id_for(
            &shard_id,
            commit_version,
            page_sequence,
            &first_key,
            &last_key,
        );
        if computed_id != page_id {
            return Err(corrupt(page_id, offset, "the page identity does not match"));
        }
        Ok(Self {
            page_id,
            page_sequence,
            shard_id,
            commit_version,
            first_key,
            last_key,
            record_count,
            codec,
            uncompressed_length,
            integrity: stored_integrity,
            records,
        })
    }
}

/// Computes the deterministic page identity.
///
/// The hash input is the page identity domain tag, the shard identifier, the
/// commit version, the page sequence, and the first and last key.
#[must_use]
pub fn page_id_for(
    shard_id: &ShardId,
    commit_version: CommitVersion,
    page_sequence: PageSequence,
    first_key: &[u8],
    last_key: &[u8],
) -> PageId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PAGE_ID_TAG.as_bytes());
    hasher.update(&[0]);
    hasher.update(
        &u64::try_from(shard_id.as_bytes().len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(shard_id.as_bytes());
    hasher.update(&commit_version.get().to_be_bytes());
    hasher.update(&page_sequence.get().to_be_bytes());
    hasher.update(
        &u64::try_from(first_key.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(first_key);
    hasher.update(
        &u64::try_from(last_key.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(last_key);
    PageId::from_bytes(*hasher.finalize().as_bytes())
}

fn encode_records(records: &[Record]) -> Result<Vec<u8>, SnapshotError> {
    let mut out = Vec::new();
    let mut previous: Option<&[u8]> = None;
    for record in records {
        if let Some(previous) = previous
            && previous >= record.key.as_slice()
        {
            return Err(corrupt(
                PageId::from_bytes([0; 32]),
                0,
                "the page keys are not strictly ascending",
            ));
        }
        previous = Some(&record.key);
        push_bytes(&mut out, &record.key, PageId::from_bytes([0; 32]), 0)?;
        push_bytes(&mut out, &record.value, PageId::from_bytes([0; 32]), 0)?;
        if out.len() as u64 > MAX_PAGE_SIZE {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
    }
    Ok(out)
}

fn decode_records(
    payload: &[u8],
    page_id: PageId,
    offset: u64,
) -> Result<Vec<Record>, SnapshotError> {
    let mut reader = Reader::new(payload, offset);
    reader.page_id = page_id;
    let mut records = Vec::new();
    while reader.remaining() > 0 {
        if records.len() >= MAX_RECORD_COUNT as usize {
            return Err(SnapshotError::PageTooLarge {
                page_id,
                offset: offset + reader.relative(),
            });
        }
        let key_len = reader.u32_as_usize()?;
        if key_len > MAX_KEY_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id,
                offset: offset + reader.relative(),
            });
        }
        let key = reader.take(key_len)?.to_vec();
        let value_len = reader.u32_as_usize()?;
        if value_len > MAX_VALUE_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id,
                offset: offset + reader.relative(),
            });
        }
        let value = reader.take(value_len)?.to_vec();
        if let Some(previous) = records.last() {
            let previous: &Record = previous;
            if previous.key >= key {
                return Err(corrupt(
                    page_id,
                    offset + reader.relative(),
                    "the page keys are not strictly ascending",
                ));
            }
        }
        records.push(Record { key, value });
    }
    Ok(records)
}

fn push_bytes(
    out: &mut Vec<u8>,
    bytes: &[u8],
    page_id: PageId,
    offset: u64,
) -> Result<(), SnapshotError> {
    let length =
        u32::try_from(bytes.len()).map_err(|_| SnapshotError::PageTooLarge { page_id, offset })?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn corrupt(page_id: PageId, offset: u64, reason: &'static str) -> SnapshotError {
    SnapshotError::CorruptPage {
        page_id,
        offset,
        reason,
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    offset: u64,
    page_id: PageId,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8], offset: u64) -> Self {
        Self {
            bytes,
            position: 0,
            offset,
            page_id: PageId::from_bytes([0; 32]),
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn relative(&self) -> u64 {
        u64::try_from(self.position).unwrap_or(u64::MAX)
    }

    fn absolute(&self) -> u64 {
        self.offset
            .saturating_add(u64::try_from(self.position).unwrap_or(u64::MAX))
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| corrupt(self.page_id, self.absolute(), "the length overflows"))?;
        if end > self.bytes.len() {
            return Err(corrupt(
                self.page_id,
                self.absolute(),
                "the page ends before a field",
            ));
        }
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn array32(&mut self) -> Result<[u8; 32], SnapshotError> {
        let bytes = self.take(32)?;
        bytes
            .try_into()
            .map_err(|_| corrupt(self.page_id, self.absolute(), "the hash field is short"))
    }

    fn u16(&mut self) -> Result<u16, SnapshotError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, SnapshotError> {
        let bytes = self.take(4)?;
        bytes
            .try_into()
            .map(u32::from_be_bytes)
            .map_err(|_| corrupt(self.page_id, self.absolute(), "the integer is short"))
    }

    fn u32_as_usize(&mut self) -> Result<usize, SnapshotError> {
        let value = self.u32()?;
        usize::try_from(value)
            .map_err(|_| corrupt(self.page_id, self.absolute(), "the length is too large"))
    }

    fn u64(&mut self) -> Result<u64, SnapshotError> {
        let bytes = self.take(8)?;
        bytes
            .try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| corrupt(self.page_id, self.absolute(), "the integer is short"))
    }
}
