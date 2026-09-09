//! Read-only view over the LSM store for multi-reader deployments.
//!
//! [`OxKvReader`](crate::store::OxKvReader) opens a `prefix` without the `ownership.json` epoch CAS, so
//! readers never fence the writer. It implements [`Store`], with every write
//! rejected at runtime, so generic code over [`Store`] accepts both handles.

use std::sync::Arc;

use async_trait::async_trait;

use crate::store::cache::{Cache, CacheStats, LruCache};
use crate::store::storage::{ObjectPath, Storage};
use crate::store::{Direction, GetSet, KeyValue, Result, Store, StoreError, Transaction};

use super::{
    Manifest, ManifestCache, MemTable, MergeSource, SstFile, TOMBSTONE_VLEN, get_blob,
    is_not_found, load_manifest, merged_gets_bytes, pull_merge_next, read_ownership,
    try_decode_blob_pointer,
};

fn read_only_err() -> StoreError {
    StoreError::Other("read-only store: writes are rejected".to_string())
}

fn default_sst_cache() -> LruCache<String, Arc<SstFile>> {
    LruCache::new(256 * 1024 * 1024, |_: &String, v: &Arc<SstFile>| {
        u32::try_from(v.size()).unwrap_or(u32::MAX)
    })
}

/// Read-only view over a [`crate::store::OxKvStore`] prefix, sharing the [`Store`] trait.
#[derive(Clone)]
pub struct OxKvReader<C = LruCache<String, Arc<SstFile>>> {
    inner: Arc<dyn Storage>,
    prefix: ObjectPath,
    epoch: u64,
    manifest_cache: Arc<async_lock::Mutex<ManifestCache>>,
    sst_cache: C,
    overlay: MemTable,
}

impl<C> std::fmt::Debug for OxKvReader<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OxKvReader")
            .field("prefix", &self.prefix)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl OxKvReader {
    /// Opens a read-only view with the default SST cache.
    ///
    /// Never acquires ownership: the observed epoch is recorded for debugging
    /// and no `ownership.json` write is performed.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` when the backing [`Storage`] cannot be read.
    pub async fn open(store: Arc<dyn Storage>, prefix: ObjectPath) -> Result<Self> {
        Self::open_with_cache(store, prefix, default_sst_cache()).await
    }
}

impl<C> OxKvReader<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    /// Opens a read-only view with a caller-supplied SST cache.
    ///
    /// See [`OxKvReader::open`] for the ownership semantics.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` when the backing [`Storage`] cannot be read.
    pub(crate) async fn open_with_cache(
        store: Arc<dyn Storage>,
        prefix: ObjectPath,
        cache: C,
    ) -> Result<Self> {
        let epoch = read_ownership(Arc::clone(&store), &prefix)
            .await?
            .map_or(0, |record| record.epoch);
        let reader = Self {
            inner: store,
            prefix,
            epoch,
            manifest_cache: Arc::new(async_lock::Mutex::new(ManifestCache::new())),
            sst_cache: cache,
            overlay: Arc::new(async_lock::RwLock::new(std::collections::BTreeMap::new())),
        };
        reader.replay_wal().await;
        Ok(reader)
    }

    /// Returns the prefix.
    #[must_use]
    pub fn prefix(&self) -> &ObjectPath {
        &self.prefix
    }

    /// Returns the epoch observed at open time.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns SST-cache hit/miss statistics, or `None` for cache backends
    /// that do not track them (e.g. `moka`).
    #[must_use]
    pub fn sst_cache_stats(&self) -> Option<CacheStats> {
        self.sst_cache.stats()
    }

    /// Returns the current manifest version.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O.
    pub async fn manifest_version(&self) -> Result<u64> {
        let (manifest, _) = load_manifest(
            Arc::clone(&self.inner),
            &self.prefix,
            self.epoch,
            &self.manifest_cache,
            std::time::Duration::from_secs(1),
        )
        .await?;
        Ok(manifest.version)
    }

    /// Replays every listed WAL file into the replay overlay.
    ///
    /// Best-effort like the writer startup path: missing files are skipped so
    /// a concurrent `gc_wal` cannot fail the open.
    async fn replay_wal(&self) {
        let (manifest, _etag) = match load_manifest(
            Arc::clone(&self.inner),
            &self.prefix,
            self.epoch,
            &self.manifest_cache,
            std::time::Duration::from_secs(1),
        )
        .await
        {
            Ok(found) => found,
            Err(_) => (Arc::new(Manifest::empty(self.epoch)), String::new()),
        };
        for wal_id in &manifest.wal {
            let path = ObjectPath::from(wal_id.as_str());
            let Ok(out) = self.inner.get(&path).await else {
                continue;
            };
            let mut overlay = self.overlay.write().await;
            let mut pos = 0usize;
            let data = out.bytes;
            while pos + 4 <= data.len() {
                let klen =
                    u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                        as usize;
                if pos + 4 + klen + 4 > data.len() {
                    break;
                }
                let key = match std::str::from_utf8(&data[pos + 4..pos + 4 + klen]) {
                    Ok(key) => key.to_string(),
                    Err(_) => break,
                };
                let value_start = pos + 4 + klen;
                let vlen = u32::from_le_bytes([
                    data[value_start],
                    data[value_start + 1],
                    data[value_start + 2],
                    data[value_start + 3],
                ]) as usize;
                if vlen == TOMBSTONE_VLEN as usize {
                    overlay.insert(key, None);
                    pos = value_start + 4;
                } else {
                    if value_start + 4 + vlen > data.len() {
                        break;
                    }
                    overlay.insert(
                        key,
                        Some(data[value_start + 4..value_start + 4 + vlen].to_vec()),
                    );
                    pos = value_start + 4 + vlen;
                }
            }
        }
    }

    async fn resolve_value(&self, raw: Vec<u8>) -> Result<Vec<u8>> {
        if let Some(ptr) = try_decode_blob_pointer(&raw) {
            let blob_path = ObjectPath::from(ptr.blob.as_str());
            let bytes = get_blob(Arc::clone(&self.inner), &blob_path).await?;
            if bytes.len() != ptr.len {
                return Err(StoreError::Storage(format!(
                    "blob len mismatch for {}: expected {}, got {}",
                    blob_path,
                    ptr.len,
                    bytes.len()
                )));
            }
            let crc = crc32fast::hash(&bytes);
            if crc != ptr.crc {
                return Err(StoreError::Storage(format!(
                    "blob crc mismatch for {}: expected {}, got {}",
                    blob_path, ptr.crc, crc
                )));
            }
            Ok(bytes)
        } else {
            Ok(raw)
        }
    }

    async fn read_sst(&self, id: &str) -> Result<Arc<SstFile>> {
        let path = ObjectPath::from(id);
        let out = self
            .inner
            .get(&path)
            .await
            .map_err(|e| StoreError::Storage(format!("get sst {id} failed: {e}")))?;
        let sst = Arc::new(SstFile::parse(out.bytes)?);
        sst.verify_file_crc()?;
        Ok(sst)
    }

    async fn fetch_sst(&self, id: &str) -> Result<Arc<SstFile>> {
        if let Some(cached) = self.sst_cache.get(&id.to_string()).await {
            return Ok(cached);
        }
        let sst = self.read_sst(id).await?;
        self.sst_cache
            .insert(id.to_string(), Arc::clone(&sst))
            .await;
        Ok(sst)
    }

    /// Reads `key` via the replay overlay → SSTs (newest first) → blob deref.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O or CRC failure.
    pub async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self.point_get(key).await {
            Err(e) if is_not_found(&e) => {
                self.manifest_cache.lock().await.clear();
                self.point_get(key).await
            }
            other => other,
        }
    }

    async fn point_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        {
            let overlay = self.overlay.read().await;
            if let Some(value) = overlay.get(key) {
                return match value {
                    Some(raw) => Ok(Some(self.resolve_value(raw.clone()).await?)),
                    None => Ok(None),
                };
            }
        }
        let (manifest, _etag) = load_manifest(
            Arc::clone(&self.inner),
            &self.prefix,
            self.epoch,
            &self.manifest_cache,
            std::time::Duration::from_secs(1),
        )
        .await?;
        for meta in manifest.sst.iter().rev() {
            if key < meta.min_key.as_str() || key > meta.max_key.as_str() {
                continue;
            }
            let sst = self.fetch_sst(&meta.id).await?;
            match sst.get_option(key)? {
                Some(Some(raw)) => return Ok(Some(self.resolve_value(raw).await?)),
                Some(None) => return Ok(None),
                None => {}
            }
        }
        Ok(None)
    }

    /// Checks existence via [`Self::get_bytes`].
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O failure.
    pub async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get_bytes(key).await?.is_some())
    }

    /// Range scan merging the replay overlay + SSTs with tombstone suppression.
    ///
    /// Restarts once against a fresh manifest when an SST read hits `not found`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O or CRC failure.
    pub async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        match self
            .range_scan(limit, direction, (cursor.0.clone(), cursor.1.clone()))
            .await
        {
            Err(e) if is_not_found(&e) => {
                self.manifest_cache.lock().await.clear();
                self.range_scan(limit, direction, cursor).await
            }
            other => other,
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn range_scan(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        let (scan_start, scan_end) = match direction {
            Direction::Next => (cursor.0.as_deref(), cursor.1.as_deref()),
            Direction::Prev => (cursor.1.as_deref(), cursor.0.as_deref()),
        };
        if direction == Direction::Prev && cursor.0.is_none() {
            return Ok(Vec::new());
        }
        let overlay_rows: Vec<(String, Option<Vec<u8>>)> = {
            let overlay = self.overlay.read().await;
            let rows: Vec<(String, Option<Vec<u8>>)> = if scan_start.is_none() && scan_end.is_none()
            {
                overlay
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            } else {
                overlay
                    .iter()
                    .filter(|(k, _)| {
                        if let Some(lo) = scan_start
                            && k.as_str() < lo
                        {
                            return false;
                        }
                        if let Some(hi) = scan_end
                            && k.as_str() > hi
                        {
                            return false;
                        }
                        true
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            };
            rows
        };
        let (manifest, _etag) = load_manifest(
            Arc::clone(&self.inner),
            &self.prefix,
            self.epoch,
            &self.manifest_cache,
            std::time::Duration::from_secs(1),
        )
        .await?;
        if direction == Direction::Next {
            let mut files: Vec<Arc<SstFile>> = Vec::new();
            for meta in manifest.sst.iter().rev() {
                let overlaps = {
                    let min = meta.min_key.as_str();
                    let max = meta.max_key.as_str();
                    let after_lower = scan_start.is_none_or(|s| max >= s);
                    let before_upper = scan_end.is_none_or(|e| min <= e);
                    after_lower && before_upper
                };
                if !overlaps {
                    continue;
                }
                files.push(self.fetch_sst(&meta.id).await?);
            }
            let mut pull: Vec<MergeSource<'_>> = Vec::with_capacity(files.len() + 1);
            pull.push(MergeSource::Mem(overlay_rows.into_iter()));
            for file in &files {
                pull.push(MergeSource::File(file.scan_iter(scan_start, scan_end)));
            }
            let merged = pull_merge_next(&mut pull, limit.map(|l| l as usize))?;
            let mut out = Vec::with_capacity(merged.len());
            for (key, raw) in merged {
                out.push(KeyValue {
                    key,
                    value: self.resolve_value(raw).await?,
                });
            }
            return Ok(out);
        }
        let mut sources = vec![overlay_rows];
        for meta in manifest.sst.iter().rev() {
            let overlaps = {
                let min = meta.min_key.as_str();
                let max = meta.max_key.as_str();
                let after_lower = scan_start.is_none_or(|s| max >= s);
                let before_upper = scan_end.is_none_or(|e| min <= e);
                after_lower && before_upper
            };
            if !overlaps {
                continue;
            }
            let sst = self.read_sst(&meta.id).await?;
            let scan = sst.scan_with_tombstones(scan_start, scan_end, None)?;
            let mut resolved: Vec<(String, Option<Vec<u8>>)> = Vec::with_capacity(scan.len());
            for (key, value) in scan {
                match value {
                    Some(raw) => {
                        let val = self.resolve_value(raw).await?;
                        resolved.push((key, Some(val)));
                    }
                    None => resolved.push((key, None)),
                }
            }
            sources.push(resolved);
        }
        Ok(merged_gets_bytes(sources, limit, direction, cursor))
    }
}

/// Read-only transaction over an [`OxKvReader`].
///
/// Reads behave like the parent reader; every write is rejected and `commit`
/// on an empty overlay succeeds as a no-op.
#[derive(Clone)]
pub struct OxKvRoTx<C = LruCache<String, Arc<SstFile>>> {
    reader: OxKvReader<C>,
}

#[async_trait]
impl<C> GetSet for OxKvReader<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        OxKvReader::get_bytes(self, key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        OxKvReader::has(self, key).await
    }

    async fn delete(&self, _key: &str) -> Result<bool> {
        Err(read_only_err())
    }

    async fn set_bytes(&self, _key: &str, _value: &[u8]) -> Result<Option<Vec<u8>>> {
        Err(read_only_err())
    }

    async fn put_bytes(&self, _key: &str, _value: &[u8]) -> Result<()> {
        Err(read_only_err())
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        OxKvReader::gets_bytes(self, limit, direction, cursor).await
    }
}

#[async_trait]
impl<C> GetSet for OxKvRoTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.reader.get_bytes(key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        self.reader.has(key).await
    }

    async fn delete(&self, _key: &str) -> Result<bool> {
        Err(read_only_err())
    }

    async fn set_bytes(&self, _key: &str, _value: &[u8]) -> Result<Option<Vec<u8>>> {
        Err(read_only_err())
    }

    async fn put_bytes(&self, _key: &str, _value: &[u8]) -> Result<()> {
        Err(read_only_err())
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.reader.gets_bytes(limit, direction, cursor).await
    }
}

#[async_trait]
impl<C> Transaction for OxKvRoTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn commit(self) -> Result<()> {
        Ok(())
    }

    async fn rollback(self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl<C> Store for OxKvReader<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    type Transaction = OxKvRoTx<C>;

    fn begin_tx(&self) -> Result<Self::Transaction> {
        Ok(OxKvRoTx {
            reader: self.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStorage;
    use crate::store::OxKvStore;

    async fn writer() -> (OxKvStore, Arc<dyn Storage>, ObjectPath) {
        let backend: Arc<dyn Storage> = Arc::new(MemStorage::new());
        let prefix = ObjectPath::from("reader");
        let store = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .with_prefix(prefix.clone())
            .skip_probe(true)
            .build()
            .await
            .expect("build");
        (store, backend, prefix)
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn open_never_fences_writer() {
        let (store, backend, prefix) = writer().await;
        let before = read_ownership(Arc::clone(&backend), &prefix)
            .await
            .expect("ownership")
            .expect("present")
            .epoch;
        let reader = OxKvReader::open(Arc::clone(&backend), prefix.clone())
            .await
            .expect("open");
        assert_eq!(reader.epoch(), before);
        let after = read_ownership(Arc::clone(&backend), &prefix)
            .await
            .expect("ownership")
            .expect("present")
            .epoch;
        assert_eq!(before, after);
        store.put_bytes("k", b"v").await.expect("writer alive");
        assert_eq!(reader.get_bytes("missing").await.expect("read"), None);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn writes_are_rejected_without_io() {
        let (store, backend, prefix) = writer().await;
        store.put_bytes("k", b"v").await.expect("put");
        let reader = OxKvReader::open(Arc::clone(&backend), prefix)
            .await
            .expect("open");
        for result in [
            reader.delete("k").await.map(|_| ()),
            reader.put_bytes("k", b"x").await,
            reader.set_bytes("k", b"x").await.map(|_| ()),
        ] {
            assert!(result.is_err());
            assert!(result.expect_err("err").to_string().contains("read-only"));
        }
        let tx = reader.begin_tx().expect("tx");
        assert!(tx.put_bytes("k", b"x").await.is_err());
        tx.commit().await.expect("empty commit");
        assert_eq!(
            store.get_bytes("k").await.expect("get"),
            Some(b"v".to_vec())
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn sees_wal_and_sst_data() {
        let (store, backend, prefix) = writer().await;
        store.put_bytes("wal", b"1").await.expect("put");
        store.put_bytes("sst", b"2").await.expect("put");
        store.flush_mem_to_sst_force().await.expect("flush");
        let reader = OxKvReader::open(Arc::clone(&backend), prefix)
            .await
            .expect("open");
        assert_eq!(
            reader.get_bytes("sst").await.expect("get"),
            Some(b"2".to_vec())
        );
        store.put_bytes("late", b"3").await.expect("put");
        let reopened = OxKvReader::open(Arc::clone(&backend), reader.prefix().clone())
            .await
            .expect("open");
        assert_eq!(
            reopened.get_bytes("late").await.expect("get"),
            Some(b"3".to_vec())
        );
    }
}
