//! The page directory.
//!
//! The directory lists every page with its offset, length, first key, last
//! key, `page_id`, `page_sequence`, and integrity value. It carries its own
//! `BLAKE3` integrity value over the directory bytes.

use oxkv_contracts::version::{Hash32, PageId, PageSequence};

use crate::error::SnapshotError;
use crate::{MAX_DIRECTORY_BYTES, MAX_KEY_BYTES, MAX_PAGE_SIZE, MAX_PAGES};

/// The directory magic bytes.
pub const DIRECTORY_MAGIC: &[u8; 4] = b"OXDR";

const DIRECTORY_FIXED_BYTES: usize = 12 + 32;
const MIN_ENTRY_BYTES: usize = 32 + 8 + 8 + 8 + 4 + 4 + 4;

/// One page entry in the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// The deterministic page identity.
    pub page_id: PageId,
    /// The page sequence.
    pub page_sequence: PageSequence,
    /// The absolute page offset from the start of the snapshot.
    pub offset: u64,
    /// The page length in bytes.
    pub length: u64,
    /// The first key in the page.
    pub first_key: Vec<u8>,
    /// The last key in the page.
    pub last_key: Vec<u8>,
    /// The CRC32C page integrity value.
    pub integrity: u32,
}

/// The page directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    /// The page entries in `page_sequence` order.
    pub entries: Vec<DirectoryEntry>,
    /// The BLAKE3 integrity value over the directory bytes.
    pub integrity: Hash32,
}

impl Directory {
    /// Builds a directory from page entries.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::CorruptPage`] for a sequence that is not
    /// strictly increasing or a key range that is inverted. Returns
    /// [`SnapshotError::PageTooLarge`] for a length over its limit.
    pub fn new(entries: Vec<DirectoryEntry>) -> Result<Self, SnapshotError> {
        validate_entries(&entries)?;
        let body = encode_entries(&entries)?;
        if body.len() as u64 > MAX_DIRECTORY_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            });
        }
        let integrity = Hash32::of(&body);
        Ok(Self { entries, integrity })
    }

    /// Returns the encoded directory length.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::PageTooLarge`] when a length field is over its
    /// limit.
    pub fn encoded_len(&self) -> Result<u64, SnapshotError> {
        let body = encode_entries(&self.entries)?;
        u64::try_from(body.len())
            .map(|length| length.saturating_add(32))
            .map_err(|_| SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: 0,
            })
    }

    /// Encodes the directory.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::PageTooLarge`] when a length field is over its
    /// limit.
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        let mut out = encode_entries(&self.entries)?;
        out.extend_from_slice(self.integrity.as_bytes());
        Ok(out)
    }

    /// Decodes and verifies one directory.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::CorruptPage`] for a bad magic, a bad integrity
    /// value, a malformed entry, or a sequence that is not strictly
    /// increasing. Returns [`SnapshotError::PageTooLarge`] for a length over
    /// its limit.
    #[allow(clippy::too_many_lines)]
    pub fn decode(bytes: &[u8], offset: u64) -> Result<Self, SnapshotError> {
        if bytes.len() < DIRECTORY_FIXED_BYTES {
            return Err(corrupt(offset, "the directory is too short"));
        }
        let mut cursor = Cursor::new(bytes, offset);
        let magic = cursor.take(4)?;
        if magic != DIRECTORY_MAGIC {
            return Err(corrupt(offset, "the directory magic is wrong"));
        }
        let major = cursor.u16()?;
        let minor = cursor.u16()?;
        if major != crate::FORMAT_MAJOR || minor > crate::FORMAT_MINOR {
            return Err(corrupt(offset, "the directory version is not supported"));
        }
        let count = cursor.u32()?;
        if u64::from(count) > MAX_PAGES {
            return Err(SnapshotError::PageTooLarge {
                page_id: PageId::from_bytes([0; 32]),
                offset: cursor.absolute(),
            });
        }
        let count = usize::try_from(count).map_err(|_| SnapshotError::PageTooLarge {
            page_id: PageId::from_bytes([0; 32]),
            offset: cursor.absolute(),
        })?;
        let minimum = count
            .checked_mul(MIN_ENTRY_BYTES)
            .and_then(|length| length.checked_add(DIRECTORY_FIXED_BYTES))
            .ok_or_else(|| corrupt(offset, "the directory entry count overflows"))?;
        if bytes.len() < minimum {
            return Err(corrupt(
                offset,
                "the directory is too short for its entry count",
            ));
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let page_id = PageId::from_bytes(cursor.array32()?);
            let page_sequence = PageSequence::new(cursor.u64()?);
            let entry_offset = cursor.u64()?;
            let length = cursor.u64()?;
            if length > MAX_PAGE_SIZE {
                return Err(SnapshotError::PageTooLarge {
                    page_id,
                    offset: cursor.absolute(),
                });
            }
            let first_len = cursor.u32_as_usize()?;
            if first_len > MAX_KEY_BYTES {
                return Err(SnapshotError::PageTooLarge {
                    page_id,
                    offset: cursor.absolute(),
                });
            }
            let first_key = cursor.take(first_len)?.to_vec();
            let last_len = cursor.u32_as_usize()?;
            if last_len > MAX_KEY_BYTES {
                return Err(SnapshotError::PageTooLarge {
                    page_id,
                    offset: cursor.absolute(),
                });
            }
            let last_key = cursor.take(last_len)?.to_vec();
            let integrity = cursor.u32()?;
            entries.push(DirectoryEntry {
                page_id,
                page_sequence,
                offset: entry_offset,
                length,
                first_key,
                last_key,
                integrity,
            });
        }
        let stored_integrity = cursor.array32()?;
        if cursor.remaining() != 0 {
            return Err(corrupt(offset, "the directory has trailing bytes"));
        }
        let integrity_start = bytes.len() - 32;
        let computed = Hash32::of(&bytes[..integrity_start]);
        if computed.as_bytes() != &stored_integrity {
            return Err(corrupt(offset, "the directory integrity does not match"));
        }
        validate_entries(&entries)?;
        Ok(Self {
            entries,
            integrity: Hash32::from_bytes(stored_integrity),
        })
    }
}

fn validate_entries(entries: &[DirectoryEntry]) -> Result<(), SnapshotError> {
    if u64::try_from(entries.len()).unwrap_or(u64::MAX) > MAX_PAGES {
        return Err(SnapshotError::PageTooLarge {
            page_id: PageId::from_bytes([0; 32]),
            offset: 0,
        });
    }
    let mut previous: Option<PageSequence> = None;
    for entry in entries {
        if let Some(previous) = previous
            && entry.page_sequence <= previous
        {
            return Err(corrupt(
                entry.offset,
                "the page sequence is not strictly increasing",
            ));
        }
        previous = Some(entry.page_sequence);
        if entry.first_key > entry.last_key {
            return Err(corrupt(entry.offset, "the key range is inverted"));
        }
        if entry.first_key.len() > MAX_KEY_BYTES || entry.last_key.len() > MAX_KEY_BYTES {
            return Err(SnapshotError::PageTooLarge {
                page_id: entry.page_id,
                offset: entry.offset,
            });
        }
        if entry.length == 0 || entry.length > MAX_PAGE_SIZE {
            return Err(SnapshotError::PageTooLarge {
                page_id: entry.page_id,
                offset: entry.offset,
            });
        }
    }
    Ok(())
}

fn encode_entries(entries: &[DirectoryEntry]) -> Result<Vec<u8>, SnapshotError> {
    let mut out = Vec::new();
    out.extend_from_slice(DIRECTORY_MAGIC);
    out.extend_from_slice(&crate::FORMAT_MAJOR.to_be_bytes());
    out.extend_from_slice(&crate::FORMAT_MINOR.to_be_bytes());
    let count = u32::try_from(entries.len()).map_err(|_| SnapshotError::PageTooLarge {
        page_id: PageId::from_bytes([0; 32]),
        offset: 0,
    })?;
    out.extend_from_slice(&count.to_be_bytes());
    for entry in entries {
        out.extend_from_slice(entry.page_id.as_bytes());
        out.extend_from_slice(&entry.page_sequence.get().to_be_bytes());
        out.extend_from_slice(&entry.offset.to_be_bytes());
        out.extend_from_slice(&entry.length.to_be_bytes());
        push_bytes(&mut out, &entry.first_key, entry.page_id, entry.offset)?;
        push_bytes(&mut out, &entry.last_key, entry.page_id, entry.offset)?;
        out.extend_from_slice(&entry.integrity.to_be_bytes());
    }
    Ok(out)
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

fn corrupt(offset: u64, reason: &'static str) -> SnapshotError {
    SnapshotError::CorruptPage {
        page_id: PageId::from_bytes([0; 32]),
        offset,
        reason,
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
    offset: u64,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8], offset: u64) -> Self {
        Self {
            bytes,
            position: 0,
            offset,
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn absolute(&self) -> u64 {
        self.offset
            .saturating_add(u64::try_from(self.position).unwrap_or(u64::MAX))
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| corrupt(self.absolute(), "the length overflows"))?;
        if end > self.bytes.len() {
            return Err(corrupt(
                self.absolute(),
                "the directory ends before a field",
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
            .map_err(|_| corrupt(self.absolute(), "the hash field is short"))
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
            .map_err(|_| corrupt(self.absolute(), "the integer is short"))
    }

    fn u32_as_usize(&mut self) -> Result<usize, SnapshotError> {
        let value = self.u32()?;
        usize::try_from(value).map_err(|_| corrupt(self.absolute(), "the length is too large"))
    }

    fn u64(&mut self) -> Result<u64, SnapshotError> {
        let bytes = self.take(8)?;
        bytes
            .try_into()
            .map(u64::from_be_bytes)
            .map_err(|_| corrupt(self.absolute(), "the integer is short"))
    }
}
