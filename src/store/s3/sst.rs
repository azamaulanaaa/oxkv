//! SST (Sorted String Table) builder and reader for `S3Store` — L0.
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]
//!
//! Fixed `32 KiB` blocks, `CRC32` per block, `bloom` filter, footer index.
//! See `docs/s3-lsm-design.md` §4, §10.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::store::{Result, StoreError};

// ---------------------------------------------------------------------------
// Constants and types
// ---------------------------------------------------------------------------

/// SST magic bytes `OXKV`.
pub(crate) const SST_MAGIC: [u8; 4] = *b"OXKV";

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
    /// `CRC32` of all block bytes concatenated.
    pub file_crc: u32,
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
        let mut data = Vec::with_capacity(item.len() + 4);
        data.extend_from_slice(item.as_bytes());
        data.extend_from_slice(&seed.to_le_bytes());
        u64::from(crc32fast::hash(&data))
    }

    /// Inserts `key`.
    pub(crate) fn set(&mut self, key: &str) {
        for i in 0..self.k {
            let h = Self::hash(key, i) % self.m as u64;
            let bit = h as usize;
            self.bits[bit / 8] |= 1 << (bit % 8);
        }
    }

    /// Checks membership (may false-positive).
    #[must_use]
    pub(crate) fn check(&self, key: &str) -> bool {
        for i in 0..self.k {
            let h = Self::hash(key, i) % self.m as u64;
            let bit = h as usize;
            if self.bits[bit / 8] & (1 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
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
        let mut rec = Vec::new();
        let klen = u32::try_from(key.len())
            .map_err(|e| StoreError::Serialization(format!("key too long: {e}")))?;
        rec.extend_from_slice(&klen.to_le_bytes());
        rec.extend_from_slice(key.as_bytes());
        match value {
            Some(val) => {
                let vlen = u32::try_from(val.len())
                    .map_err(|e| StoreError::Serialization(format!("value too long: {e}")))?;
                rec.extend_from_slice(&vlen.to_le_bytes());
                rec.extend_from_slice(val);
            }
            None => {
                rec.extend_from_slice(&TOMBSTONE_VLEN.to_le_bytes());
            }
        }
        if !cur_block.is_empty() && cur_block.len() + rec.len() > block_size {
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
        cur_block.extend_from_slice(&rec);
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
        hasher.finalize()
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

/// Convenience for `BTreeMap<String, Vec<u8>>` (no tombstones) — used by tests.
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
pub(crate) struct SstFile {
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
        let magic_offset = data.len() - 8;
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
        let footer_start = footer_len_offset - footer_len;
        if footer_start > data.len() {
            return Err(StoreError::Storage("sst footer out of bounds".to_string()));
        }
        let footer_bytes = &data[footer_start..footer_len_offset];
        let footer: SstFooter = serde_json::from_slice(footer_bytes)
            .map_err(|e| StoreError::Storage(format!("parse footer: {e}")))?;
        Ok(Self {
            data,
            footer,
            footer_offset: footer_start,
        })
    }

    /// Returns footer.
    #[must_use]
    pub(crate) fn footer(&self) -> &SstFooter {
        &self.footer
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
        for i in 0..k {
            let h = Bloom::hash(key, i) % m as u64;
            let bit = h as usize;
            if bits[bit / 8] & (1 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
    }

    /// Returns block bytes for `meta`, verifying `CRC`.
    pub(crate) fn block_bytes(&self, meta: &BlockMeta) -> Result<Vec<u8>> {
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
        Ok(bytes.to_vec())
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
    /// Empty `Some(vec![])` is a valid empty value, not tombstone.
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
    pub fn scan_with_tombstones(
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

    /// Verifies file-level `CRC`.
    pub fn verify_file_crc(&self) -> Result<()> {
        let mut hasher = crc32fast::Hasher::new();
        for meta in &self.footer.index {
            let bytes = self.block_bytes(meta)?;
            hasher.update(&bytes);
        }
        let got = hasher.finalize();
        if got != self.footer.file_crc {
            return Err(StoreError::Storage(format!(
                "file crc mismatch: expected {}, got {}",
                self.footer.file_crc, got
            )));
        }
        Ok(())
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
    fn bloom_false_negative_never() {
        let entries = sample_entries();
        let data = build_sst(&entries, 1024).unwrap();
        let sst = SstFile::parse(data).unwrap();
        for key in entries.keys() {
            assert!(
                sst.may_contain(key),
                "bloom must contain inserted key {key}"
            );
        }
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

    #[test]
    fn sst_from_values_helper() {
        let values = sample_values();
        let data = build_sst_from_values(&values, 1024).unwrap();
        let sst = SstFile::parse(data).unwrap();
        assert_eq!(sst.get("a").unwrap(), Some(b"val-a".to_vec()));
    }
}
