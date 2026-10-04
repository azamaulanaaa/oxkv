//! SST (Sorted String Table) builder and reader for `OxKvStore` — L0.
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]
//!
//! Fixed `32 KiB` blocks, `CRC32` per block, `bloom` filter, footer index.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::store::{Result, StoreError};

// ---------------------------------------------------------------------------
// Constants and types
// ---------------------------------------------------------------------------

/// SST magic bytes `OXKV`.
pub(crate) const SST_MAGIC: [u8; 4] = *b"OXKV";
/// Bytes after the JSON footer: 4-byte magic + 4-byte version.
const SST_TRAILER_LEN: usize = 8;

/// SST format version.
pub(crate) const SST_VERSION: u32 = 1;

/// Default block size `32 KiB`.
pub(crate) const DEFAULT_BLOCK_SIZE: usize = 32 * 1024;

/// Tombstone marker: `vlen == u32::MAX` means deleted.
pub(crate) const TOMBSTONE_VLEN: u32 = u32::MAX;

/// Per-block index entry stored in footer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BlockMeta {
    /// Minimum key in block (inclusive).
    pub min_key: String,
    /// Maximum key in block (inclusive).
    pub max_key: String,
    /// Offset of block in file.
    pub offset: u64,
    /// Length of block bytes.
    pub len: u64,
    /// CRC32 of block bytes.
    pub crc: u32,
}

/// Footer stored before magic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SstFooter {
    /// Block index (sorted by `min_key`).
    pub index: Vec<BlockMeta>,
    /// Bloom filter bits (simple, 10 bits per key, k=3).
    pub bloom_bits: Vec<u8>,
    /// Number of hash functions.
    pub bloom_k: u32,
    /// Number of bits `m`.
    pub bloom_m: u64,
    /// `u64` file checksum (`CRC32` zero-extended).
    pub file_crc: u64,
}

/// Simple bloom filter — 10 bits per key, k=3.
#[derive(Debug, Clone)]
pub(crate) struct Bloom {
    bits: Vec<u8>,
    k: u32,
    m: usize,
}

impl Bloom {
    /// Creates a bloom for `n` expected items.
    #[must_use]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.max(1);
        let m = n * 10;
        let bytes = (m + 7) / 8;
        Self {
            bits: vec![0u8; bytes],
            k: 3,
            m,
        }
    }

    fn hash(item: &str, seed: u32) -> u64 {
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(item.as_bytes());
        hasher.update(&seed.to_le_bytes());
        u64::from(hasher.finalize())
    }

    /// Inserts `key`.
    pub(crate) fn set(&mut self, key: &str) {
        for i in 0..self.k {
            let h = Self::hash(key, i) % self.m as u64;
            let bit = h as usize;
            self.bits[bit / 8] |= 1 << (bit % 8);
        }
    }

    /// Returns bits for serialization.
    #[must_use]
    pub(crate) fn bits(&self) -> &[u8] {
        &self.bits
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builds an SST file from sorted entries (`None` = tombstone).
pub fn build_sst(
    entries: &BTreeMap<String, Option<Vec<u8>>>,
    block_size: usize,
) -> Result<Vec<u8>> {
    let block_size = block_size.max(64);
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut index: Vec<BlockMeta> = Vec::new();
    let mut bloom = Bloom::new(entries.len());
    for key in entries.keys() {
        bloom.set(key);
    }

    let mut cur_block = Vec::new();
    let mut cur_min: Option<String> = None;
    let mut cur_max: Option<String> = None;
    let mut offset: u64 = 0;

    for (key, value) in entries {
        // Compute record length without allocating, so we can decide to cut
        // the block before writing. Tombstone and empty value both occupy
        // 4+key.len()+4 bytes; non-empty value adds value.len().
        let rec_len = 4 + key.len() + 4 + value.as_ref().map_or(0, Vec::len);
        if !cur_block.is_empty() && cur_block.len() + rec_len > block_size {
            let crc = crc32fast::hash(&cur_block);
            // `cur_min`/`cur_max` are guaranteed `Some` when `cur_block` is non-empty.
            let meta = BlockMeta {
                min_key: cur_min.clone().expect("min"),
                max_key: cur_max.clone().expect("max"),
                offset,
                len: cur_block.len() as u64,
                crc,
            };
            offset += cur_block.len() as u64;
            index.push(meta);
            blocks.push(std::mem::take(&mut cur_block));
            cur_min = None;
        }
        if cur_min.is_none() {
            cur_min = Some(key.clone());
        }
        cur_max = Some(key.clone());
        // Write record directly into cur_block without intermediate Vec.
        let klen = u32::try_from(key.len())
            .map_err(|e| StoreError::Serialization(format!("key too long: {e}")))?;
        cur_block.extend_from_slice(&klen.to_le_bytes());
        cur_block.extend_from_slice(key.as_bytes());
        match value {
            Some(val) => {
                let vlen = u32::try_from(val.len())
                    .map_err(|e| StoreError::Serialization(format!("value too long: {e}")))?;
                cur_block.extend_from_slice(&vlen.to_le_bytes());
                cur_block.extend_from_slice(val);
            }
            None => {
                cur_block.extend_from_slice(&TOMBSTONE_VLEN.to_le_bytes());
            }
        }
    }
    if !cur_block.is_empty() {
        let crc = crc32fast::hash(&cur_block);
        let meta = BlockMeta {
            min_key: cur_min.expect("min"),
            max_key: cur_max.expect("max"),
            offset,
            len: cur_block.len() as u64,
            crc,
        };
        blocks.push(cur_block);
        index.push(meta);
    }

    let file_crc = {
        let mut hasher = crc32fast::Hasher::new();
        for block in &blocks {
            hasher.update(block);
        }
        u64::from(hasher.finalize())
    };

    let footer = SstFooter {
        index,
        bloom_bits: bloom.bits().to_vec(),
        bloom_k: bloom.k,
        bloom_m: bloom.m as u64,
        file_crc,
    };
    let footer_bytes = serde_json::to_vec(&footer)
        .map_err(|e| StoreError::Storage(format!("serialize footer: {e}")))?;

    let mut out = Vec::new();
    for block in blocks {
        out.extend_from_slice(&block);
    }
    out.extend_from_slice(&footer_bytes);
    out.extend_from_slice(&(footer_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&SST_MAGIC);
    out.extend_from_slice(&SST_VERSION.to_le_bytes());
    Ok(out)
}

/// Convenience for `BTreeMap<String, Vec<u8>>` (no tombstones). Tests only.
#[cfg(test)]
pub(crate) fn build_sst_from_values(
    entries: &BTreeMap<String, Vec<u8>>,
    block_size: usize,
) -> Result<Vec<u8>> {
    let opt: BTreeMap<String, Option<Vec<u8>>> = entries
        .iter()
        .map(|(k, v)| (k.clone(), Some(v.clone())))
        .collect();
    build_sst(&opt, block_size)
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Parsed SST file (zero-copy view over bytes).
#[derive(Debug)]
pub struct SstFile {
    /// Raw file bytes.
    data: Vec<u8>,
    /// Footer.
    footer: SstFooter,
    /// Offset where blocks end / footer begins.
    footer_offset: usize,
}

impl SstFile {
    /// Parses `data` as SST, verifying magic and version.
    pub(crate) fn parse(data: Vec<u8>) -> Result<Self> {
        if data.len() < 12 {
            return Err(StoreError::Storage("sst too small".to_string()));
        }
        let magic_offset = data.len() - SST_TRAILER_LEN;
        if data[magic_offset..magic_offset + 4] != SST_MAGIC {
            return Err(StoreError::Storage("sst bad magic".to_string()));
        }
        let ver = u32::from_le_bytes([
            data[magic_offset + 4],
            data[magic_offset + 5],
            data[magic_offset + 6],
            data[magic_offset + 7],
        ]);
        if ver != SST_VERSION {
            return Err(StoreError::Storage(format!(
                "unsupported sst version {ver}"
            )));
        }
        let footer_len_offset = magic_offset - 4;
        let footer_len = u32::from_le_bytes([
            data[footer_len_offset],
            data[footer_len_offset + 1],
            data[footer_len_offset + 2],
            data[footer_len_offset + 3],
        ]) as usize;
        // `footer_len` comes from the object, so it can exceed the offset: a
        // plain subtraction underflows (panics under overflow checks) before
        // the bounds check below could run.
        let Some(footer_start) = footer_len_offset.checked_sub(footer_len) else {
            return Err(StoreError::Storage("sst footer out of bounds".to_string()));
        };
        if footer_start + footer_len != footer_len_offset || footer_start > data.len() {
            return Err(StoreError::Storage("sst footer out of bounds".to_string()));
        }
        let footer_bytes = &data[footer_start..footer_len_offset];
        let footer: SstFooter = serde_json::from_slice(footer_bytes)
            .map_err(|e| StoreError::Storage(format!("parse footer: {e}")))?;
        // `bloom_k` drives a loop on the point-lookup hot path. `build_sst`
        // always writes 3, so anything wild is corruption or a hostile object,
        // and accepting it would turn every read into a CPU denial of service.
        if footer.bloom_k == 0 || footer.bloom_k > 32 {
            return Err(StoreError::Storage(format!(
                "sst implausible bloom_k {}",
                footer.bloom_k
            )));
        }
        Ok(Self {
            data,
            footer,
            footer_offset: footer_start,
        })
    }

    /// File size in bytes, used as the cache weight (`moka` weigher).
    ///
    /// The only part of [`SstFile`] that is public API: the engine
    /// constructs files itself, so everything else is crate-internal.
    #[must_use]
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// Checks bloom (false-positive possible, never false-negative).
    #[must_use]
    pub(crate) fn may_contain(&self, key: &str) -> bool {
        if self.footer.index.is_empty() {
            return false;
        }
        let m = self.footer.bloom_m as usize;
        if m == 0 {
            return true;
        }
        let bits = &self.footer.bloom_bits;
        let k = self.footer.bloom_k;
        // `bloom_m` and `bloom_bits` are deserialized independently from a
        // remote object, so the implied width can exceed the buffer. Every
        // other footer field is bounds-checked; this one is not, and it is on
        // the hot read path.
        if m > bits.len().saturating_mul(8) {
            return false;
        }
        for i in 0..k {
            let h = Bloom::hash(key, i) % m as u64;
            let bit = h as usize;
            if !bits
                .get(bit / 8)
                .is_some_and(|byte| byte & (1 << (bit % 8)) != 0)
            {
                return false;
            }
        }
        true
    }

    /// Returns block bytes for `meta`, verifying `CRC` (zero-copy).
    pub(crate) fn block_bytes(&self, meta: &BlockMeta) -> Result<&[u8]> {
        let start = meta.offset as usize;
        let end = start + meta.len as usize;
        if end > self.footer_offset {
            return Err(StoreError::Storage("block out of bounds".to_string()));
        }
        let bytes = &self.data[start..end];
        let crc = crc32fast::hash(bytes);
        if crc != meta.crc {
            return Err(StoreError::Storage(format!(
                "block crc mismatch for {}..{}: expected {}, got {}",
                meta.min_key, meta.max_key, meta.crc, crc
            )));
        }
        Ok(bytes)
    }

    /// Point lookup with tombstone distinction: `Ok(Some(Some(v)))` = value,
    /// `Ok(Some(None))` = tombstone, `Ok(None)` = not in this SST.
    pub(crate) fn get_option(&self, key: &str) -> Result<Option<Option<Vec<u8>>>> {
        if !self.may_contain(key) {
            return Ok(None);
        }
        for meta in &self.footer.index {
            if key < meta.min_key.as_str() || key > meta.max_key.as_str() {
                continue;
            }
            let bytes = self.block_bytes(meta)?;
            let mut pos = 0usize;
            while pos + 4 <= bytes.len() {
                let klen = u32::from_le_bytes([
                    bytes[pos],
                    bytes[pos + 1],
                    bytes[pos + 2],
                    bytes[pos + 3],
                ]) as usize;
                if pos + 4 + klen + 4 > bytes.len() {
                    break;
                }
                let key_in_block = std::str::from_utf8(&bytes[pos + 4..pos + 4 + klen])
                    .map_err(|e| StoreError::Storage(format!("utf8: {e}")))?;
                let v_start = pos + 4 + klen;
                let vlen = u32::from_le_bytes([
                    bytes[v_start],
                    bytes[v_start + 1],
                    bytes[v_start + 2],
                    bytes[v_start + 3],
                ]) as usize;
                if vlen == TOMBSTONE_VLEN as usize {
                    if key_in_block == key {
                        return Ok(Some(None));
                    }
                    pos = v_start + 4;
                    continue;
                }
                if v_start + 4 + vlen > bytes.len() {
                    break;
                }
                if key_in_block == key {
                    let val = bytes[v_start + 4..v_start + 4 + vlen].to_vec();
                    return Ok(Some(Some(val)));
                }
                pos = v_start + 4 + vlen;
            }
        }
        Ok(None)
    }

    /// Point lookup: returns value if present (`None` for missing or tombstone).
    /// Empty `Some(vec![])` is a valid empty value, not tombstone. Tests
    /// only; the read path uses `get_option`, which reports tombstones.
    #[cfg(test)]
    pub(crate) fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if !self.may_contain(key) {
            return Ok(None);
        }
        for meta in &self.footer.index {
            if key < meta.min_key.as_str() || key > meta.max_key.as_str() {
                continue;
            }
            let bytes = self.block_bytes(meta)?;
            let mut pos = 0usize;
            while pos + 4 <= bytes.len() {
                let klen = u32::from_le_bytes([
                    bytes[pos],
                    bytes[pos + 1],
                    bytes[pos + 2],
                    bytes[pos + 3],
                ]) as usize;
                if pos + 4 + klen + 4 > bytes.len() {
                    break;
                }
                let key_in_block = std::str::from_utf8(&bytes[pos + 4..pos + 4 + klen])
                    .map_err(|e| StoreError::Storage(format!("utf8: {e}")))?;
                let v_start = pos + 4 + klen;
                let vlen = u32::from_le_bytes([
                    bytes[v_start],
                    bytes[v_start + 1],
                    bytes[v_start + 2],
                    bytes[v_start + 3],
                ]) as usize;
                if vlen == TOMBSTONE_VLEN as usize {
                    if key_in_block == key {
                        return Ok(None);
                    }
                    pos = v_start + 4;
                    continue;
                }
                if v_start + 4 + vlen > bytes.len() {
                    break;
                }
                if key_in_block == key {
                    let val = bytes[v_start + 4..v_start + 4 + vlen].to_vec();
                    return Ok(Some(val));
                }
                pos = v_start + 4 + vlen;
            }
        }
        Ok(None)
    }

    /// Range scan: returns sorted KVs in `[start, end]` inclusive, honoring
    /// direction. `limit` caps results. Tombstones are omitted; empty values
    /// are returned.
    #[cfg(test)]
    pub fn scan(
        &self,
        start: Option<&str>,
        end: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let mut out = Vec::new();
        for meta in &self.footer.index {
            if let Some(start_key) = start {
                if meta.max_key.as_str() < start_key {
                    continue;
                }
            }
            if let Some(end_key) = end {
                if meta.min_key.as_str() > end_key {
                    continue;
                }
            }
            let bytes = self.block_bytes(meta)?;
            let mut pos = 0usize;
            while pos + 4 <= bytes.len() {
                let klen = u32::from_le_bytes([
                    bytes[pos],
                    bytes[pos + 1],
                    bytes[pos + 2],
                    bytes[pos + 3],
                ]) as usize;
                if pos + 4 + klen + 4 > bytes.len() {
                    break;
                }
                let key_str = std::str::from_utf8(&bytes[pos + 4..pos + 4 + klen])
                    .map_err(|e| StoreError::Storage(format!("utf8: {e}")))?
                    .to_string();
                let v_start = pos + 4 + klen;
                let vlen = u32::from_le_bytes([
                    bytes[v_start],
                    bytes[v_start + 1],
                    bytes[v_start + 2],
                    bytes[v_start + 3],
                ]) as usize;
                let is_tombstone = vlen == TOMBSTONE_VLEN as usize;
                let value = if is_tombstone {
                    None
                } else {
                    if v_start + 4 + vlen > bytes.len() {
                        break;
                    }
                    Some(bytes[v_start + 4..v_start + 4 + vlen].to_vec())
                };
                let in_range = match (start, end) {
                    (Some(s), Some(e)) => key_str.as_str() >= s && key_str.as_str() <= e,
                    (Some(s), None) => key_str.as_str() >= s,
                    (None, Some(e)) => key_str.as_str() <= e,
                    (None, None) => true,
                };
                if in_range {
                    if let Some(val) = &value {
                        out.push((key_str.clone(), val.clone()));
                        if let Some(lim) = limit {
                            if out.len() >= lim {
                                return Ok(out);
                            }
                        }
                    }
                }
                pos = if is_tombstone {
                    v_start + 4
                } else {
                    v_start + 4 + vlen
                };
            }
        }
        if let Some(lim) = limit {
            out.truncate(lim);
        }
        Ok(out)
    }

    /// Range scan including tombstones: returns `(key, Option<value>)` where
    /// `None` is tombstone.
    pub(crate) fn scan_with_tombstones(
        &self,
        start: Option<&str>,
        end: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<(String, Option<Vec<u8>>)>> {
        let mut out = Vec::new();
        for meta in &self.footer.index {
            if let Some(start_key) = start {
                if meta.max_key.as_str() < start_key {
                    continue;
                }
            }
            if let Some(end_key) = end {
                if meta.min_key.as_str() > end_key {
                    continue;
                }
            }
            let bytes = self.block_bytes(meta)?;
            let mut pos = 0usize;
            while pos + 4 <= bytes.len() {
                let klen = u32::from_le_bytes([
                    bytes[pos],
                    bytes[pos + 1],
                    bytes[pos + 2],
                    bytes[pos + 3],
                ]) as usize;
                if pos + 4 + klen + 4 > bytes.len() {
                    break;
                }
                let key_str = std::str::from_utf8(&bytes[pos + 4..pos + 4 + klen])
                    .map_err(|e| StoreError::Storage(format!("utf8: {e}")))?
                    .to_string();
                let v_start = pos + 4 + klen;
                let vlen = u32::from_le_bytes([
                    bytes[v_start],
                    bytes[v_start + 1],
                    bytes[v_start + 2],
                    bytes[v_start + 3],
                ]) as usize;
                let is_tombstone = vlen == TOMBSTONE_VLEN as usize;
                let value = if is_tombstone {
                    None
                } else {
                    if v_start + 4 + vlen > bytes.len() {
                        break;
                    }
                    Some(bytes[v_start + 4..v_start + 4 + vlen].to_vec())
                };
                let in_range = match (start, end) {
                    (Some(s), Some(e)) => key_str.as_str() >= s && key_str.as_str() <= e,
                    (Some(s), None) => key_str.as_str() >= s,
                    (None, Some(e)) => key_str.as_str() <= e,
                    (None, None) => true,
                };
                if in_range {
                    out.push((key_str.clone(), value));
                    if let Some(lim) = limit {
                        if out.len() >= lim {
                            return Ok(out);
                        }
                    }
                }
                pos = if is_tombstone {
                    v_start + 4
                } else {
                    v_start + 4 + vlen
                };
            }
        }
        if let Some(lim) = limit {
            out.truncate(lim);
        }
        Ok(out)
    }

    /// Lazily scans entries in `[start, end]` inclusive, in file order.
    ///
    /// Yields `(key, value)` with `None` marking tombstones; blocks outside
    /// the range are skipped via the footer index and entries decode on pull,
    /// so a capped consumer never pays for the unread tail. Truncation ends
    /// the stream, matching `scan`; encoding errors surface as `Err` items
    /// and terminate iteration on the next pull.
    #[must_use]
    pub(crate) fn scan_iter<'a>(
        &'a self,
        start: Option<&'a str>,
        end: Option<&'a str>,
    ) -> SstScan<'a> {
        SstScan {
            file: self,
            start,
            end,
            block_idx: 0,
            block: None,
            pos: 0,
            done: false,
        }
    }

    /// Verifies file-level `CRC` (`u64`).
    pub(crate) fn verify_file_crc(&self) -> Result<()> {
        let mut hasher = crc32fast::Hasher::new();
        for meta in &self.footer.index {
            let bytes = self.block_bytes(meta)?;
            hasher.update(&bytes);
        }
        let got = u64::from(hasher.finalize());
        if got != self.footer.file_crc {
            return Err(StoreError::Storage(format!(
                "file crc mismatch: expected {}, got {}",
                self.footer.file_crc, got
            )));
        }
        Ok(())
    }
}

/// Decodes one entry at `pos`: key, value (`None` for tombstones), and the
/// next position. Returns `Ok(None)` on truncation (callers stop, matching
/// [`SstFile::scan`]) and `Err` on invalid UTF-8, also matching `scan`.
fn decode_entry(bytes: &[u8], pos: usize) -> Result<Option<(String, Option<Vec<u8>>, usize)>> {
    if pos + 4 > bytes.len() {
        return Ok(None);
    }
    let klen =
        u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
    if pos + 4 + klen + 4 > bytes.len() {
        return Ok(None);
    }
    let key_str = std::str::from_utf8(&bytes[pos + 4..pos + 4 + klen])
        .map_err(|e| StoreError::Storage(format!("utf8: {e}")))?
        .to_string();
    let v_start = pos + 4 + klen;
    let vlen = u32::from_le_bytes([
        bytes[v_start],
        bytes[v_start + 1],
        bytes[v_start + 2],
        bytes[v_start + 3],
    ]) as usize;
    let is_tombstone = vlen == TOMBSTONE_VLEN as usize;
    let value = if is_tombstone {
        None
    } else {
        if v_start + 4 + vlen > bytes.len() {
            return Ok(None);
        }
        Some(bytes[v_start + 4..v_start + 4 + vlen].to_vec())
    };
    let next = if is_tombstone {
        v_start + 4
    } else {
        v_start + 4 + vlen
    };
    Ok(Some((key_str, value, next)))
}

/// Lazy in-range scan over one [`SstFile`], tombstone-inclusive.
///
/// See [`SstFile::scan_iter`]: entries decode on pull in file order, so a
/// consumer that stops early never pays for the unread tail.
#[derive(Debug)]
pub struct SstScan<'a> {
    file: &'a SstFile,
    start: Option<&'a str>,
    end: Option<&'a str>,
    block_idx: usize,
    block: Option<&'a [u8]>,
    pos: usize,
    done: bool,
}

impl<'a> SstScan<'a> {
    /// Range overlap for a block: kept outside `next` so block pruning
    /// reads as one predicate.
    fn overlaps(&self, meta: &BlockMeta) -> bool {
        if let Some(start_key) = self.start {
            if meta.max_key.as_str() < start_key {
                return false;
            }
        }
        if let Some(end_key) = self.end {
            if meta.min_key.as_str() > end_key {
                return false;
            }
        }
        true
    }

    /// In-range check for a decoded key, mirroring [`SstFile::scan`].
    fn in_range(&self, key: &str) -> bool {
        match (self.start, self.end) {
            (Some(s), Some(e)) => key >= s && key <= e,
            (Some(s), None) => key >= s,
            (None, Some(e)) => key <= e,
            (None, None) => true,
        }
    }
}

impl Iterator for SstScan<'_> {
    type Item = Result<(String, Option<Vec<u8>>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            if self.block.is_none() {
                let Some(meta) = self.file.footer.index.get(self.block_idx) else {
                    return None;
                };
                self.block_idx += 1;
                if !self.overlaps(meta) {
                    continue;
                }
                let meta = &self.file.footer.index[self.block_idx - 1];
                match self.file.block_bytes(meta) {
                    Ok(bytes) => {
                        self.block = Some(bytes);
                        self.pos = 0;
                    }
                    Err(e) => {
                        self.done = true;
                        return Some(Err(e));
                    }
                }
            }
            let bytes = self.block.unwrap_or(&[]);
            match decode_entry(bytes, self.pos) {
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
                Ok(None) => {
                    self.block = None;
                }
                Ok(Some((key, value, next))) => {
                    self.pos = next;
                    if self.in_range(&key) {
                        return Some(Ok((key, value)));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sample_entries() -> BTreeMap<String, Option<Vec<u8>>> {
        let mut map = BTreeMap::new();
        map.insert("a".to_string(), Some(b"val-a".to_vec()));
        map.insert("b".to_string(), Some(b"val-b".to_vec()));
        map.insert("c".to_string(), Some(b"val-c".to_vec()));
        map
    }

    fn sample_values() -> BTreeMap<String, Vec<u8>> {
        let mut map = BTreeMap::new();
        map.insert("a".to_string(), b"val-a".to_vec());
        map.insert("b".to_string(), b"val-b".to_vec());
        map.insert("c".to_string(), b"val-c".to_vec());
        map
    }

    #[test]
    fn sst_create_and_read() {
        let entries = sample_entries();
        let data = build_sst(&entries, 64).expect("build");
        let sst = SstFile::parse(data).expect("parse");
        assert_eq!(sst.footer.index.len(), 1);
        assert!(sst.may_contain("a"));
        assert!(!sst.may_contain("z"));
        assert_eq!(sst.get("b").unwrap(), Some(b"val-b".to_vec()));
        assert_eq!(sst.get("z").unwrap(), None);
        sst.verify_file_crc().unwrap();
    }

    #[test]
    fn sst_single_block() {
        let entries = sample_entries();
        let data = build_sst(&entries, 1024).expect("build");
        let sst = SstFile::parse(data).unwrap();
        assert_eq!(sst.footer.index.len(), 1);
        let scan = sst.scan(None, None, None).unwrap();
        assert_eq!(scan.len(), 3);
        assert_eq!(scan[0].0, "a");
    }

    #[test]
    fn sst_empty() {
        let entries: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();
        let data = build_sst(&entries, 1024).unwrap();
        let sst = SstFile::parse(data).unwrap();
        assert_eq!(sst.footer.index.len(), 0);
        assert!(!sst.may_contain("a"));
        assert_eq!(sst.scan(None, None, None).unwrap().len(), 0);
    }

    #[test]
    fn crc_detects_corruption() {
        let entries = sample_entries();
        let mut data = build_sst(&entries, 1024).unwrap();
        data[0] ^= 0xFF;
        let sst = SstFile::parse(data).unwrap();
        let err = sst.get("a").unwrap_err();
        assert!(err.to_string().contains("crc mismatch"));
    }

    #[test]
    fn sst_range_scan() {
        let mut entries = BTreeMap::new();
        for ch in 'a'..='z' {
            entries.insert(ch.to_string(), Some(vec![ch as u8]));
        }
        let data = build_sst(&entries, 64).unwrap();
        let sst = SstFile::parse(data).unwrap();
        let scan = sst.scan(Some("m"), Some("p"), None).unwrap();
        assert_eq!(scan.len(), 4);
        assert_eq!(scan[0].0, "m");
        assert_eq!(scan[3].0, "p");
    }

    #[test]
    fn sst_tombstone_not_returned() {
        let mut entries = BTreeMap::new();
        entries.insert("a".to_string(), Some(b"v".to_vec()));
        entries.insert("b".to_string(), None);
        let data = build_sst(&entries, 1024).unwrap();
        let sst = SstFile::parse(data).unwrap();
        assert_eq!(sst.get("b").unwrap(), None);
        let scan = sst.scan(None, None, None).unwrap();
        assert_eq!(scan.len(), 1);
        assert_eq!(scan[0].0, "a");
    }

    #[test]
    fn sst_empty_value_vs_tombstone() {
        let mut entries = BTreeMap::new();
        entries.insert("a".to_string(), Some(Vec::new()));
        entries.insert("b".to_string(), None);
        let data = build_sst(&entries, 1024).unwrap();
        let sst = SstFile::parse(data).unwrap();
        assert_eq!(sst.get("a").unwrap(), Some(Vec::new()));
        assert_eq!(sst.get("b").unwrap(), None);
        let scan = sst.scan(None, None, None).unwrap();
        assert_eq!(scan.len(), 1);
        assert_eq!(scan[0].0, "a");
        assert_eq!(scan[0].1, Vec::<u8>::new());
    }

    /// A corrupt footer length must be an error, never a panic. Regression:
    /// `footer_len_offset - footer_len` underflowed before the bounds check,
    /// so a garbage length panicked under overflow checks — on the one path
    /// that is supposed to tolerate corrupt objects.
    #[test]
    fn parse_rejects_footer_length_past_the_offset() {
        let data = build_sst(&sample_entries(), 1024).unwrap();
        let magic_offset = data.len() - SST_TRAILER_LEN;
        let footer_len_offset = magic_offset - 4;
        let mut bad = data.clone();
        bad[footer_len_offset..footer_len_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            SstFile::parse(bad).is_err(),
            "footer length beyond the offset must be rejected"
        );
        // And a length that lands exactly one byte before the offset.
        let mut bad = data;
        let overshoot = u32::try_from(footer_len_offset + 1).unwrap();
        bad[footer_len_offset..footer_len_offset + 4].copy_from_slice(&overshoot.to_le_bytes());
        assert!(SstFile::parse(bad).is_err());
    }

    /// Truncated objects must be errors, not partial parses. Raw storage bytes
    /// go straight into `SstFile::parse`, so a partially uploaded SST lands
    /// here.
    #[test]
    fn parse_rejects_truncated_object() {
        let data = build_sst(&sample_entries(), 1024).unwrap();
        for cut in [1usize, 8, 32, data.len() / 2, data.len() - 1] {
            assert!(
                SstFile::parse(data[..data.len() - cut].to_vec()).is_err(),
                "truncating {cut} bytes must be rejected"
            );
        }
    }

    /// The per-read file CRC gate must actually reject a corrupted block.
    /// Only `get` was covered before, so the gate every read calls had zero
    /// negative coverage.
    #[test]
    fn verify_file_crc_detects_block_corruption() {
        let entries = sample_entries();
        let data = build_sst(&entries, 64).unwrap();
        let sst = SstFile::parse(data.clone()).unwrap();
        sst.verify_file_crc().expect("untouched file passes");
        for offset in [8usize, data.len() / 3, data.len() - SST_TRAILER_LEN - 8] {
            let mut bad = data.clone();
            bad[offset] ^= 0xFF;
            let err = SstFile::parse(bad)
                .and_then(|s| s.verify_file_crc())
                .expect_err("corrupted byte must fail the file crc");
            assert!(!err.to_string().is_empty());
        }
    }

    /// A footer claiming more bloom bits than it carries must not index out of
    /// bounds — `bloom_m` and `bloom_bits` deserialize independently.
    #[test]
    fn may_contain_rejects_oversized_bloom_width() {
        let data = build_sst(&sample_entries(), 1024).unwrap();
        let sst = SstFile::parse(data).unwrap();
        let mut footer = sst.footer.clone();
        footer.bloom_m = u64::try_from(footer.bloom_bits.len() * 8 + 64).unwrap();
        let mut skewed = SstFile {
            data: sst.data.clone(),
            footer,
            footer_offset: sst.footer_offset,
        };
        assert!(!skewed.may_contain("a"), "must not read past bloom_bits");
        // Sanity: the unskewed footer still answers.
        skewed = SstFile::parse(sst.data.clone()).unwrap();
        assert!(skewed.may_contain("a"));
    }

    /// Bloom behaviour that can actually fail: present keys must be reported,
    /// and absent keys must mostly be rejected (not always — false positives
    /// are allowed). The old test asserted only positives, so a `check` that
    /// always returned `true` passed it.
    #[test]
    fn bloom_rejects_most_absent_keys() {
        let entries = sample_entries();
        let data = build_sst(&entries, 1024).unwrap();
        let sst = SstFile::parse(data).unwrap();
        for key in entries.keys() {
            assert!(sst.may_contain(key), "{key} is present");
        }
        let absent_hits = (0..200)
            .filter(|i| sst.may_contain(&format!("absent-{i}")))
            .count();
        assert!(
            absent_hits < 40,
            "bloom accepted {absent_hits}/200 absent keys — filter is not working"
        );
    }

    /// The no-tombstone convenience builder round-trips through the reader.
    #[test]
    fn sst_from_values_helper() {
        let values = sample_values();
        let data = build_sst_from_values(&values, 1024).unwrap();
        let sst = SstFile::parse(data).unwrap();
        for (k, v) in &values {
            assert_eq!(sst.get(k).unwrap().as_deref(), Some(&v[..]), "{k}");
        }
        let scan = sst.scan(None, None, None).unwrap();
        assert_eq!(scan.len(), values.len());
        assert_eq!(scan[0].0, "a");
        assert_eq!(scan[0].1, b"val-a".to_vec());
    }
}
