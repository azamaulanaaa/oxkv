//! S3-backed LSM store — probe + epoch fencing + WAL + SST.
//!
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use moka::future::Cache;
use object_store::path::Path;
use object_store::{ObjectStore, PutMode, PutPayload, PutResult};

use crate::store::{Direction, KeyValue, Result, StoreError};

mod blob;
mod manifest;
mod ownership;
mod probe;
mod sst;

pub(crate) use blob::{BlobPointer, blob_hash, blob_path};
pub(crate) use blob::{
    encode_blob_pointer, get_blob, is_overflow, put_blob, try_decode_blob_pointer,
};
pub(crate) use manifest::{Manifest, ManifestCache, SstMeta, manifest_path};
pub(crate) use manifest::{cas_manifest, read_manifest};
pub(crate) use ownership::{
    OwnershipRecord, epoch_prefix, format_epoch, ownership_path, sst_path, wal_path,
};
pub(crate) use ownership::{acquire_ownership, cas_backoff, read_ownership};
pub(crate) use probe::probe_store;
pub(crate) use sst::{DEFAULT_BLOCK_SIZE, SST_MAGIC, SST_VERSION, build_sst_from_values};
pub(crate) use sst::{SstFile, build_sst};

type MemMap = std::collections::BTreeMap<String, Option<Vec<u8>>>;
type MemTable = Arc<tokio::sync::RwLock<MemMap>>;
type WalBuffer = Arc<tokio::sync::Mutex<Vec<(String, Option<Vec<u8>>)>>>;

/// S3-backed store (incremental — probe + fencing + WAL gate + SST).
pub struct S3Store {
    inner: Arc<dyn ObjectStore>,
    prefix: Path,
    epoch: u64,
    session: String,
    mem: MemTable,
    wal_seq: std::sync::atomic::AtomicU64,
    wal_buffer: WalBuffer,
    sst_seq: std::sync::atomic::AtomicU64,
    manifest_cache: Arc<tokio::sync::Mutex<ManifestCache>>,
    /// Pinned reader versions for WAL GC watermark (G5).
    /// `BTreeMap<version, count>` — `min_key` is the watermark.
    readers: Arc<tokio::sync::Mutex<std::collections::BTreeMap<u64, usize>>>,
    /// SST file cache for `G11` budgets — `moka` LRU, ~8k entries (≈256 MB with 32KB blocks).
    sst_cache: Cache<String, Arc<SstFile>>,
}

impl std::fmt::Debug for S3Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Store")
            .field("prefix", &self.prefix)
            .field("epoch", &self.epoch)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl S3Store {
    /// Creates a new store builder.
    #[must_use]
    pub fn builder() -> S3StoreBuilder {
        S3StoreBuilder {
            inner: None,
            prefix: Path::default(),
            skip_probe: false,
            session: None,
        }
    }

    /// Runs the storage probe against `store` at `prefix/probe/canary`.
    ///
    /// Mirrors `celld diagnose` — validates `If-None-Match` / `If-Match`
    /// conditional writes. Returns `Ok(())` only on
    /// `ok (create, reject-create, reject-stale)`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if conditional writes are not enforced.
    pub async fn probe(store: Arc<dyn ObjectStore>, prefix: &Path) -> Result<()> {
        probe_store(store, prefix).await
    }

    /// Returns the underlying object store (for tests).
    #[cfg(test)]
    #[must_use]
    pub fn inner_store(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.inner)
    }

    /// Returns the prefix.
    #[cfg(test)]
    #[must_use]
    pub fn prefix(&self) -> &Path {
        &self.prefix
    }

    /// Returns the current epoch.
    #[cfg(test)]
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns the session id.
    #[cfg(test)]
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Stages `set` into `MemTable` + WAL buffer (commit = mem, §5).
    ///
    /// Does not hit S3 — use [`Self::flush`] or [`Self::commit_durable`] for RPO=0.
    pub async fn stage_set(&self, key: &str, value: &[u8]) {
        self.mem
            .write()
            .await
            .insert(key.to_string(), Some(value.to_vec()));
        self.wal_buffer
            .lock()
            .await
            .push((key.to_string(), Some(value.to_vec())));
    }

    /// Stages `delete` into `MemTable` + WAL buffer.
    pub async fn stage_delete(&self, key: &str) {
        self.mem.write().await.insert(key.to_string(), None);
        self.wal_buffer.lock().await.push((key.to_string(), None));
    }

    /// Reads from `MemTable` (hot path, no S3).
    pub async fn mem_get(&self, key: &str) -> Option<Option<Vec<u8>>> {
        self.mem.read().await.get(key).cloned()
    }

    /// Flushes buffered WAL ops to `e{epoch}/wal/{seq:08}.log.zst` via
    /// `PutMode::Create` (`If-None-Match:"*"`), then gates on ownership.
    ///
    /// Implements `commit_durable` RPO=0 (G1).
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` on `PUT` failure and `StoreError::Fenced`
    /// if `ownership.json` no longer names this epoch/session after the `PUT`.
    pub async fn flush(&self) -> Result<()> {
        let ops: Vec<(String, Option<Vec<u8>>)> = {
            let mut buf = self.wal_buffer.lock().await;
            if buf.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut *buf)
        };

        let mut payload_buf = Vec::new();
        for (key, value) in &ops {
            match value {
                Some(val) => crate::store::encode_record(&mut payload_buf, key, val)
                    .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?,
                None => {
                    crate::store::encode_record(&mut payload_buf, key, &[])
                        .map_err(|e| StoreError::Storage(format!("encode wal tombstone: {e}")))?;
                }
            }
        }

        let seq = self
            .wal_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = wal_path(&self.prefix, self.epoch, seq);

        let put_res = self
            .inner
            .put_opts(&path, PutPayload::from(payload_buf), PutMode::Create.into())
            .await;

        match put_res {
            Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
            Err(e) => return Err(StoreError::Storage(format!("put wal failed: {e}"))),
        }

        let cur = read_ownership(Arc::clone(&self.inner), &self.prefix).await?;
        match cur {
            Some(rec) if rec.epoch == self.epoch && rec.owner_session == self.session => {}
            Some(rec) => {
                return Err(StoreError::Fenced(format!(
                    "fenced: epoch {} session {} superseded by epoch {} session {}",
                    self.epoch, self.session, rec.epoch, rec.owner_session
                )));
            }
            None => {
                return Err(StoreError::Fenced(
                    "fenced: ownership missing after wal put".to_string(),
                ));
            }
        }

        let wal_id = path.to_string();
        for attempt in 0..4 {
            let mut cache = self.manifest_cache.lock().await;
            let (mut manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            if manifest.wal.iter().any(|w| w == &wal_id) {
                return Ok(());
            }
            manifest.wal.push(wal_id.clone());
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    cache.update(manifest, new_etag);
                    return Ok(());
                }
                Err(e) if e.to_string().contains("CAS conflict") => {
                    cache.clear();
                    if attempt == 3 {
                        return Err(StoreError::Storage(format!(
                            "wal manifest CAS conflict after retries: {e}"
                        )));
                    }
                    let backoff = cas_backoff(attempt);
                    drop(cache);
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Convenience: stage + flush (RPO=0) — mirrors `commit_durable`.
    ///
    /// # Errors
    ///
    /// Propagates `StoreError` from [`Self::flush`].
    pub async fn commit_durable_set(&self, key: &str, value: &[u8]) -> Result<()> {
        self.stage_set(key, value).await;
        self.flush().await
    }

    /// Flushes `MemTable` to `L0` SST if above `32 MiB` or `force`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` on `PUT`/`CAS` failure or `StoreError::Fenced`
    /// if `ownership` no longer matches.
    pub async fn flush_mem_to_sst(&self) -> Result<Option<SstMeta>> {
        self.flush_mem_to_sst_inner(false).await
    }

    /// Forces `MemTable` flush to `L0` regardless of size.
    ///
    /// # Errors
    ///
    /// Same as [`Self::flush_mem_to_sst`].
    pub async fn flush_mem_to_sst_force(&self) -> Result<Option<SstMeta>> {
        self.flush_mem_to_sst_inner(true).await
    }

    #[allow(clippy::too_many_lines)]
    async fn flush_mem_to_sst_inner(&self, force: bool) -> Result<Option<SstMeta>> {
        let snapshot: std::collections::BTreeMap<String, Option<Vec<u8>>> = {
            let mem = self.mem.read().await;
            if mem.is_empty() {
                return Ok(None);
            }
            let est: usize = mem
                .iter()
                .map(|(k, v)| k.len() + v.as_ref().map_or(0, |b| b.len()) + 8)
                .sum();
            if !force && est < 32 * 1024 * 1024 {
                return Ok(None);
            }
            mem.clone()
        };

        let mut sst_entries: std::collections::BTreeMap<String, Option<Vec<u8>>> =
            std::collections::BTreeMap::new();
        for (key, value) in &snapshot {
            match value {
                Some(val) if is_overflow(key, val, DEFAULT_BLOCK_SIZE) => {
                    let blob_path =
                        put_blob(Arc::clone(&self.inner), &self.prefix, self.epoch, val).await?;
                    let crc = crc32fast::hash(val);
                    let ptr = encode_blob_pointer(&blob_path, val.len(), crc);
                    sst_entries.insert(key.clone(), Some(ptr));
                }
                other => {
                    sst_entries.insert(key.clone(), other.clone());
                }
            }
        }

        let sst_bytes = build_sst(&sst_entries, DEFAULT_BLOCK_SIZE)?;
        if sst_bytes.is_empty() {
            return Ok(None);
        }
        let seq = self
            .sst_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let sst_id = format!("e{:06}/sst/L0/{:09}.sst.zst", self.epoch, seq);
        let sst_path = Path::from(sst_id.clone());
        let put_res = self
            .inner
            .put_opts(
                &sst_path,
                PutPayload::from(sst_bytes.clone()),
                PutMode::Create.into(),
            )
            .await;
        match put_res {
            Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
            Err(e) => return Err(StoreError::Storage(format!("put sst failed: {e}"))),
        }

        let cur_owner = read_ownership(Arc::clone(&self.inner), &self.prefix).await?;
        match cur_owner {
            Some(rec) if rec.epoch == self.epoch && rec.owner_session == self.session => {}
            Some(rec) => {
                return Err(StoreError::Fenced(format!(
                    "fenced: epoch {} session {} superseded by epoch {} session {}",
                    self.epoch, self.session, rec.epoch, rec.owner_session
                )));
            }
            None => {
                return Err(StoreError::Fenced(
                    "fenced: ownership missing before manifest CAS".to_string(),
                ));
            }
        }

        let mut cache = self.manifest_cache.lock().await;
        let (mut manifest, etag) = cache
            .load(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                std::time::Duration::from_secs(1),
            )
            .await?;
        if manifest.sst.iter().any(|m| m.id == sst_id) {
            let existing = manifest.sst.iter().find(|m| m.id == sst_id).cloned();
            {
                let mut mem = self.mem.write().await;
                for key in snapshot.keys() {
                    mem.remove(key);
                }
            }
            return Ok(existing);
        }
        let sst_meta = SstMeta {
            id: sst_id.clone(),
            level: 0,
            min_key: sst_entries.keys().next().cloned().unwrap_or_default(),
            max_key: sst_entries.keys().next_back().cloned().unwrap_or_default(),
            size: sst_bytes.len() as u64,
        };
        manifest.sst.push(sst_meta.clone());
        manifest.version = manifest.version.wrapping_add(1);
        let etag_opt = if etag.is_empty() { None } else { Some(etag) };
        match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
            Ok(new_etag) => {
                cache.update(manifest, new_etag);
                {
                    let mut mem = self.mem.write().await;
                    for key in snapshot.keys() {
                        mem.remove(key);
                    }
                }
                Ok(Some(sst_meta))
            }
            Err(e) if e.to_string().contains("CAS conflict") => {
                let backoff = cas_backoff(0);
                tokio::time::sleep(backoff).await;
                cache.clear();
                let (reloaded, _) = cache
                    .load(
                        Arc::clone(&self.inner),
                        &self.prefix,
                        self.epoch,
                        std::time::Duration::from_secs(0),
                    )
                    .await?;
                if reloaded.sst.iter().any(|m| m.id == sst_id) {
                    let mut mem = self.mem.write().await;
                    for key in snapshot.keys() {
                        mem.remove(key);
                    }
                    return Ok(reloaded.sst.into_iter().find(|m| m.id == sst_id));
                }
                Err(StoreError::Storage(format!(
                    "manifest CAS conflict after backoff retry: {e}"
                )))
            }
            Err(e) => Err(e),
        }
    }

    async fn resolve_value(&self, raw: Vec<u8>) -> Result<Vec<u8>> {
        if let Some(ptr) = try_decode_blob_pointer(&raw) {
            let blob_path = Path::from(ptr.blob.clone());
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

    async fn fetch_sst(&self, id: &str) -> Result<Arc<SstFile>> {
        if let Some(cached) = self.sst_cache.get(id).await {
            return Ok(cached);
        }
        let path = Path::from(id.to_string());
        let res = self
            .inner
            .get(&path)
            .await
            .map_err(|e| StoreError::Storage(format!("get sst {id} failed: {e}")))?;
        let bytes = res
            .bytes()
            .await
            .map_err(|e| StoreError::Storage(format!("read sst {id} failed: {e}")))?;
        let sst = Arc::new(SstFile::parse(bytes.to_vec())?);
        sst.verify_file_crc()?;
        self.sst_cache
            .insert(id.to_string(), Arc::clone(&sst))
            .await;
        Ok(sst)
    }

    /// Reads `key` via `MemTable` → SSTs (newest first) → blob deref.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O or CRC failure.
    pub async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        {
            let mem = self.mem.read().await;
            if let Some(val) = mem.get(key) {
                match val {
                    Some(v) => return Ok(Some(self.resolve_value(v.clone()).await?)),
                    None => return Ok(None),
                }
            }
        }
        let (manifest, _etag) = {
            let mut cache = self.manifest_cache.lock().await;
            cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?
        };
        for meta in manifest.sst.iter().rev() {
            if key < meta.min_key.as_str() || key > meta.max_key.as_str() {
                continue;
            }
            let sst = self.fetch_sst(&meta.id).await?;
            match sst.get_option(key)? {
                Some(Some(raw)) => return Ok(Some(self.resolve_value(raw).await?)),
                Some(None) => return Ok(None),
                None => continue,
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

    /// Range scan merging `MemTable` + SSTs with tombstone suppression and blob deref.
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
        let mut sources: Vec<Vec<(String, Option<Vec<u8>>)>> = Vec::new();
        {
            let mem = self.mem.read().await;
            let mem_vec: Vec<(String, Option<Vec<u8>>)> =
                mem.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            sources.push(mem_vec);
        }
        let (manifest, _etag) = {
            let mut cache = self.manifest_cache.lock().await;
            cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?
        };
        for meta in manifest.sst.iter().rev() {
            let overlaps = {
                let start = cursor.0.as_deref();
                let end = cursor.1.as_deref();
                let min = meta.min_key.as_str();
                let max = meta.max_key.as_str();
                let after_start = start.is_none_or(|s| max >= s);
                let before_end = end.is_none_or(|e| min <= e);
                after_start && before_end
            };
            if !overlaps && cursor.0.is_some() {
                continue;
            }
            let sst = self.fetch_sst(&meta.id).await?;
            let scan = sst.scan_with_tombstones(None, None, None)?;
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

    /// Registers a pinned reader at `version` for `G5` watermark.
    ///
    /// While pinned, `gc_wal` will retain `WAL` needed for that snapshot.
    pub async fn register_reader(&self, version: u64) {
        let mut readers = self.readers.lock().await;
        *readers.entry(version).or_insert(0) += 1;
    }

    /// Unregisters a pinned reader.
    pub async fn unregister_reader(&self, version: u64) {
        let mut readers = self.readers.lock().await;
        if let Some(count) = readers.get_mut(&version) {
            *count -= 1;
            if *count == 0 {
                readers.remove(&version);
            }
        }
    }

    /// Returns the minimum pinned version, if any (watermark for `G5`).
    pub async fn min_reader_version(&self) -> Option<u64> {
        let readers = self.readers.lock().await;
        readers.keys().next().copied()
    }

    /// Returns the current manifest version (for tests).
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O.
    pub async fn manifest_version(&self) -> Result<u64> {
        let mut cache = self.manifest_cache.lock().await;
        let (manifest, _) = cache
            .load(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                std::time::Duration::from_secs(1),
            )
            .await?;
        Ok(manifest.version)
    }

    /// GCs `WAL` entries that are covered by an `SST` and not pinned (G5).
    ///
    /// An entry is eligible only when an `SST` is manifest-visible and
    /// its version is `< min_reader_version`. With no pinned readers, all
    /// `WAL` covered by `L0` is eligible. Returns number of files deleted.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on `GET`/`CAS`/`DELETE` failure.
    pub async fn gc_wal(&self) -> Result<usize> {
        let min_version = self.min_reader_version().await;
        for _ in 0..4 {
            let mut cache = self.manifest_cache.lock().await;
            let (mut manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            if manifest.wal.is_empty() || manifest.sst.is_empty() {
                return Ok(0);
            }
            // If a reader pins an old version, retain WAL.
            if let Some(min) = min_version
                && min < manifest.version
            {
                return Ok(0);
            }
            let to_delete = manifest.wal.clone();
            manifest.wal.clear();
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    cache.update(manifest, new_etag);
                    drop(cache);
                    let mut deleted = 0usize;
                    for wal in &to_delete {
                        let path = Path::from(wal.clone());
                        match self.inner.delete(&path).await {
                            Ok(()) | Err(object_store::Error::NotFound { .. }) => deleted += 1,
                            Err(e) => {
                                return Err(StoreError::Storage(format!(
                                    "delete wal {wal} failed: {e}"
                                )));
                            }
                        }
                    }
                    return Ok(deleted);
                }
                Err(e) if e.to_string().contains("CAS conflict") => {
                    cache.clear();
                    drop(cache);
                    tokio::time::sleep(cas_backoff(0)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(0)
    }
}

/// Builder for [`S3Store`].
#[derive(Default)]
pub struct S3StoreBuilder {
    inner: Option<Arc<dyn ObjectStore>>,
    prefix: Path,
    skip_probe: bool,
    session: Option<String>,
}

impl std::fmt::Debug for S3StoreBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3StoreBuilder")
            .field("prefix", &self.prefix)
            .field("skip_probe", &self.skip_probe)
            .field("has_store", &self.inner.is_some())
            .finish_non_exhaustive()
    }
}

impl S3StoreBuilder {
    /// Sets the backing [`ObjectStore`] (use `Arc::new(InMemory::new())` in tests,
    /// `AmazonS3Builder` / `parse_url` in prod).
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn ObjectStore>) -> Self {
        self.inner = Some(store);
        self
    }

    /// Sets the key prefix inside the bucket (e.g. `Path::from("oxkv")`).
    #[must_use]
    pub fn with_prefix(mut self, prefix: Path) -> Self {
        self.prefix = prefix;
        self
    }

    /// Sets the owner session id (unique per builder). If not set, a
    /// deterministic fallback is generated.
    #[must_use]
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    /// Skips the startup storage probe (`CELLD_STORAGE_PROBE=0` equivalent).
    #[must_use]
    pub fn skip_probe(mut self, skip: bool) -> Self {
        self.skip_probe = skip;
        self
    }

    /// Whether the probe will be skipped.
    #[must_use]
    pub fn is_skip_probe(&self) -> bool {
        self.skip_probe
    }

    /// Builds the store, running the probe unless skipped, then CAS-acquires
    /// `ownership.json` epoch. The returned store is fenced to that epoch.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails or `StoreError::Fenced`
    /// if `ownership.json` CAS loses the race.
    pub async fn build(self) -> Result<S3Store> {
        let store = self.inner.ok_or_else(|| {
            StoreError::Storage("S3Store requires an ObjectStore via with_store()".to_string())
        })?;

        if !self.skip_probe {
            probe_store(Arc::clone(&store), &self.prefix).await?;
        }

        let session = self.session.unwrap_or_else(|| {
            format!(
                "sess-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            )
        });
        let rec = acquire_ownership(Arc::clone(&store), &self.prefix, &session).await?;

        Ok(S3Store {
            inner: store,
            prefix: self.prefix,
            epoch: rec.epoch,
            session,
            mem: Arc::new(tokio::sync::RwLock::new(std::collections::BTreeMap::new())),
            wal_seq: std::sync::atomic::AtomicU64::new(0),
            wal_buffer: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            sst_seq: std::sync::atomic::AtomicU64::new(0),
            manifest_cache: Arc::new(tokio::sync::Mutex::new(ManifestCache::new())),
            readers: Arc::new(tokio::sync::Mutex::new(std::collections::BTreeMap::new())),
            sst_cache: Cache::builder().max_capacity(8192).build(),
        })
    }
}

// ---------------------------------------------------------------------------
// Heap merge for gets_bytes over MemTable + SSTs (G8/G12)
// ---------------------------------------------------------------------------

/// Merges sorted sources (newest first) with newest-wins dedup and tombstone suppression.
///
/// Each source is `Vec<(key, Option<value>)>` sorted ascending; `None` is tombstone.
/// Returns deduplicated, sorted `KeyValue` (tombstones removed).
#[must_use]
pub(crate) fn merge_sources(sources: Vec<Vec<(String, Option<Vec<u8>>)>>) -> Vec<KeyValue> {
    let mut map = std::collections::BTreeMap::new();
    for src in sources {
        for (key, value) in src {
            map.entry(key).or_insert(value);
        }
    }
    map.into_iter()
        .filter_map(|(key, value)| value.map(|v| KeyValue { key, value: v }))
        .collect()
}

/// Range-filtered, direction-aware scan over merged sources.
///
/// Mirrors `GetSet::gets_bytes` cursor semantics.
#[must_use]
pub(crate) fn merged_gets_bytes(
    sources: Vec<Vec<(String, Option<Vec<u8>>)>>,
    limit: Option<u32>,
    direction: Direction,
    cursor: (Option<String>, Option<String>),
) -> Vec<KeyValue> {
    let merged = merge_sources(sources);
    let (start, end) = cursor;
    let mut filtered: Vec<KeyValue> = match direction {
        Direction::Next => merged
            .into_iter()
            .filter(|kv| {
                if let Some(ref start_key) = start
                    && kv.key < *start_key
                {
                    return false;
                }
                if let Some(ref end_key) = end
                    && kv.key > *end_key
                {
                    return false;
                }
                true
            })
            .collect(),
        Direction::Prev => {
            if start.is_none() {
                return Vec::new();
            }
            let mut vec = merged
                .into_iter()
                .filter(|kv| {
                    if let Some(ref start_key) = start
                        && kv.key > *start_key
                    {
                        return false;
                    }
                    if let Some(ref end_key) = end
                        && kv.key < *end_key
                    {
                        return false;
                    }
                    true
                })
                .collect::<Vec<_>>();
            vec.reverse();
            vec
        }
    };
    if let Some(lim) = limit {
        let lim = lim as usize;
        if filtered.len() > lim {
            filtered.truncate(lim);
        }
    }
    filtered
}

/// In-memory store helper for tests.
#[cfg(test)]
pub(crate) fn new_in_memory() -> Arc<dyn ObjectStore> {
    Arc::new(object_store::memory::InMemory::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::ObjectStore;

    #[tokio::test]
    async fn probe_rejects_b2_like_store_via_builder() {
        #[derive(Debug)]
        struct NoConditionStore {
            inner: Arc<dyn ObjectStore>,
        }

        impl std::fmt::Display for NoConditionStore {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "NoConditionStore")
            }
        }

        #[async_trait::async_trait]
        impl ObjectStore for NoConditionStore {
            async fn put_opts(
                &self,
                location: &Path,
                payload: PutPayload,
                _opts: object_store::PutOptions,
            ) -> object_store::Result<PutResult> {
                self.inner.put(location, payload).await
            }

            async fn put_multipart_opts(
                &self,
                _location: &Path,
                _opts: object_store::PutMultipartOptions,
            ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
                unimplemented!()
            }

            async fn get_opts(
                &self,
                location: &Path,
                options: object_store::GetOptions,
            ) -> object_store::Result<object_store::GetResult> {
                self.inner.get_opts(location, options).await
            }

            async fn delete(&self, location: &Path) -> object_store::Result<()> {
                self.inner.delete(location).await
            }

            fn list(
                &self,
                prefix: Option<&Path>,
            ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
            {
                self.inner.list(prefix)
            }

            async fn list_with_delimiter(
                &self,
                prefix: Option<&Path>,
            ) -> object_store::Result<object_store::ListResult> {
                self.inner.list_with_delimiter(prefix).await
            }

            async fn copy(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy(from, to).await
            }

            async fn copy_if_not_exists(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy_if_not_exists(from, to).await
            }
        }

        let inner = new_in_memory();
        let bad: Arc<dyn ObjectStore> = Arc::new(NoConditionStore { inner });
        let err = probe_store(Arc::clone(&bad), &Path::default())
            .await
            .expect_err("must reject");
        assert!(
            err.to_string().contains("conditional writes not enforced")
                || err.to_string().contains("stale If-Match"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn builder_runs_probe_by_default() {
        let store = new_in_memory();
        let built = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv"))
            .build()
            .await
            .expect("builder with InMemory must pass probe");
        assert_eq!(built.prefix().as_ref(), "oxkv");
    }

    #[tokio::test]
    async fn builder_skip_probe_flag() {
        assert!(!S3Store::builder().is_skip_probe());
        assert!(S3Store::builder().skip_probe(true).is_skip_probe());
    }

    #[tokio::test]
    async fn builder_skip_probe_allows_b2_like_store() {
        #[derive(Debug)]
        struct NoConditionStore {
            inner: Arc<dyn ObjectStore>,
        }
        impl std::fmt::Display for NoConditionStore {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "NoConditionStore")
            }
        }
        #[async_trait::async_trait]
        impl ObjectStore for NoConditionStore {
            async fn put_opts(
                &self,
                location: &Path,
                payload: PutPayload,
                _opts: object_store::PutOptions,
            ) -> object_store::Result<PutResult> {
                self.inner.put(location, payload).await
            }
            async fn put_multipart_opts(
                &self,
                _location: &Path,
                _opts: object_store::PutMultipartOptions,
            ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
                unimplemented!()
            }
            async fn get_opts(
                &self,
                location: &Path,
                options: object_store::GetOptions,
            ) -> object_store::Result<object_store::GetResult> {
                self.inner.get_opts(location, options).await
            }
            async fn delete(&self, location: &Path) -> object_store::Result<()> {
                self.inner.delete(location).await
            }
            fn list(
                &self,
                prefix: Option<&Path>,
            ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
            {
                self.inner.list(prefix)
            }
            async fn list_with_delimiter(
                &self,
                prefix: Option<&Path>,
            ) -> object_store::Result<object_store::ListResult> {
                self.inner.list_with_delimiter(prefix).await
            }
            async fn copy(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy(from, to).await
            }
            async fn copy_if_not_exists(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy_if_not_exists(from, to).await
            }
        }

        let bad: Arc<dyn ObjectStore> = Arc::new(NoConditionStore {
            inner: new_in_memory(),
        });
        let err = S3Store::builder()
            .with_store(Arc::clone(&bad))
            .build()
            .await
            .expect_err("must reject without skip_probe");
        assert!(err.to_string().contains("conditional writes"));

        S3Store::builder()
            .with_store(bad)
            .skip_probe(true)
            .build()
            .await
            .expect("skip_probe must allow bad store");
    }

    #[tokio::test]
    async fn probe_static_entry_point() {
        let store = new_in_memory();
        S3Store::probe(store, &Path::default())
            .await
            .expect("static probe must pass");
    }

    #[test]
    fn ownership_path_no_prefix() {
        assert_eq!(ownership_path(&Path::default()).as_ref(), "ownership.json");
    }

    #[test]
    fn ownership_path_with_prefix() {
        assert_eq!(
            ownership_path(&Path::from("oxkv")).as_ref(),
            "oxkv/ownership.json"
        );
    }

    #[test]
    fn manifest_path_and_epoch_prefix_formatting() {
        assert_eq!(manifest_path(&Path::default()).as_ref(), "manifest.json");
        assert_eq!(epoch_prefix(&Path::default(), 7).as_ref(), "e000007");
        assert_eq!(
            epoch_prefix(&Path::from("oxkv"), 7).as_ref(),
            "oxkv/e000007"
        );
        assert_eq!(
            wal_path(&Path::from("oxkv"), 7, 42).as_ref(),
            "oxkv/e000007/wal/00000042.log.zst"
        );
        assert_eq!(
            sst_path(&Path::from("oxkv"), 7, 0, 123).as_ref(),
            "oxkv/e000007/sst/L0/000000123.sst.zst"
        );
        assert_eq!(
            blob_path(&Path::from("oxkv"), 7, "abc").as_ref(),
            "oxkv/e000007/blob/abc.zst"
        );
    }

    #[tokio::test]
    async fn fencing_acquire_increments_epoch() {
        let store = new_in_memory();
        let prefix = Path::from("oxkv");
        let r1 = acquire_ownership(Arc::clone(&store), &prefix, "node-a")
            .await
            .expect("first acquire");
        assert_eq!(r1.epoch, 1);
        assert_eq!(r1.owner_session, "node-a");
        let r2 = acquire_ownership(Arc::clone(&store), &prefix, "node-b")
            .await
            .expect("second acquire");
        assert_eq!(r2.epoch, 2);
        assert_eq!(r2.owner_session, "node-b");
        let cur = read_ownership(Arc::clone(&store), &prefix)
            .await
            .expect("read")
            .expect("some");
        assert_eq!(cur.epoch, 2);
    }

    #[tokio::test]
    async fn fencing_stale_writer_superseded_prefix_invisible() {
        let store = new_in_memory();
        let prefix = Path::from("oxkv");
        let r1 = acquire_ownership(Arc::clone(&store), &prefix, "node-a")
            .await
            .unwrap();
        let wal1 = wal_path(&prefix, r1.epoch, 1);
        store
            .put(&wal1, PutPayload::from_static(b"wal1"))
            .await
            .unwrap();

        let r2 = acquire_ownership(Arc::clone(&store), &prefix, "node-b")
            .await
            .unwrap();
        assert_eq!(r2.epoch, 2);
        let wal2 = wal_path(&prefix, r2.epoch, 1);
        store
            .put(&wal2, PutPayload::from_static(b"wal2"))
            .await
            .unwrap();

        let stale_ver = object_store::UpdateVersion {
            e_tag: Some("\"stale-etag-r1\"".to_string()),
            version: None,
        };
        let stale_path = ownership_path(&prefix);
        let stale_put = store
            .put_opts(
                &stale_path,
                PutPayload::from_static(b"stale"),
                PutMode::Update(stale_ver).into(),
            )
            .await;
        assert!(
            matches!(stale_put, Err(object_store::Error::Precondition { .. })),
            "stale If-Match must be rejected"
        );

        let got1 = store.get(&wal1).await.expect("old epoch wal isolated");
        assert_eq!(got1.bytes().await.unwrap().as_ref(), b"wal1");
        let got2 = store.get(&wal2).await.expect("new epoch wal");
        assert_eq!(got2.bytes().await.unwrap().as_ref(), b"wal2");
        let cur = read_ownership(store, &prefix).await.unwrap().unwrap();
        assert_eq!(cur.epoch, 2);
    }

    #[tokio::test]
    async fn wal_durable_and_sst_with_overflow() {
        let store = new_in_memory();
        let s3 = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv"))
            .with_session("sess-1")
            .build()
            .await
            .unwrap();

        s3.stage_set("k1", b"v1").await;
        s3.stage_set("k2", b"v2").await;
        s3.flush().await.expect("wal flush");

        let large = vec![b'x'; DEFAULT_BLOCK_SIZE];
        s3.stage_set("large", &large).await;
        s3.flush_mem_to_sst_force()
            .await
            .expect("sst flush")
            .expect("some sst");

        let got = s3.get_bytes("large").await.unwrap().expect("large");
        assert_eq!(got, large);
        let got2 = s3.get_bytes("k1").await.unwrap().expect("k1");
        assert_eq!(got2, b"v1");
    }

    #[tokio::test]
    async fn read_path_heap_merge_tombstone() {
        let store = new_in_memory();
        let s3 = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv2"))
            .with_session("sess-2")
            .build()
            .await
            .unwrap();

        s3.stage_set("a", b"1").await;
        s3.stage_set("b", b"2").await;
        s3.flush_mem_to_sst_force().await.unwrap();

        s3.stage_set("b", b"22").await;
        s3.stage_delete("a").await;
        s3.flush_mem_to_sst_force().await.unwrap();

        assert_eq!(s3.get_bytes("a").await.unwrap(), None);
        assert_eq!(
            s3.get_bytes("b").await.unwrap().as_deref(),
            Some(b"22".as_slice())
        );

        let scanned = s3
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].key, "b");
    }

    #[test]
    fn merge_sources_newest_wins_and_tombstone_suppressed() {
        let sources = vec![
            vec![
                ("a".to_string(), Some(b"new-a".to_vec())),
                ("b".to_string(), None),
            ],
            vec![
                ("a".to_string(), Some(b"old-a".to_vec())),
                ("b".to_string(), Some(b"old-b".to_vec())),
                ("c".to_string(), Some(b"c".to_vec())),
            ],
        ];
        let merged = super::merge_sources(sources);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].key, "a");
        assert_eq!(merged[0].value, b"new-a");
        assert_eq!(merged[1].key, "c");
    }

    #[test]
    fn merged_gets_respects_direction_and_limit() {
        let sources = vec![vec![
            ("a".to_string(), Some(b"1".to_vec())),
            ("b".to_string(), Some(b"2".to_vec())),
            ("c".to_string(), Some(b"3".to_vec())),
        ]];
        let next =
            super::merged_gets_bytes(sources.clone(), Some(2), Direction::Next, (None, None));
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].key, "a");

        let prev = super::merged_gets_bytes(
            sources,
            None,
            Direction::Prev,
            (Some("c".to_string()), None),
        );
        assert_eq!(prev.len(), 3);
        assert_eq!(prev[0].key, "c");
        assert_eq!(prev[2].key, "a");
    }

    #[tokio::test]
    async fn wal_gc_pinned_reader_holds_log() {
        let store = new_in_memory();
        let s3 = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("gc-test"))
            .with_session("gc-sess")
            .build()
            .await
            .unwrap();

        // Create WAL + SST so WAL is eligible for GC (covered by SST).
        s3.stage_set("k1", b"v1").await;
        s3.flush().await.unwrap();
        let v1 = s3.manifest_version().await.unwrap();
        s3.stage_set("k2", b"v2").await;
        s3.flush_mem_to_sst_force().await.unwrap().expect("sst");
        let v2 = s3.manifest_version().await.unwrap();
        assert!(v2 > v1);

        // Pin reader at old version v1 — GC must hold WAL.
        s3.register_reader(v1).await;
        let held = s3.gc_wal().await.unwrap();
        assert_eq!(held, 0, "pinned reader must hold WAL");
        let (manifest_held, _) = {
            let mut cache = s3.manifest_cache.lock().await;
            cache
                .load(
                    Arc::clone(&store),
                    &Path::from("gc-test"),
                    s3.epoch(),
                    std::time::Duration::from_secs(0),
                )
                .await
                .unwrap()
        };
        assert!(!manifest_held.wal.is_empty(), "WAL retained while pinned");
        // WAL objects still exist.
        for wal in &manifest_held.wal {
            let p = Path::from(wal.clone());
            assert!(
                store.get(&p).await.is_ok(),
                "WAL {wal} must exist while pinned"
            );
        }

        // Unpin — GC must now delete WAL and clear manifest.wal.
        s3.unregister_reader(v1).await;
        let deleted = s3.gc_wal().await.unwrap();
        assert!(deleted > 0, "WAL should be GC'd after unpin");
        // Verify WAL objects deleted and manifest cleared.
        for wal in &manifest_held.wal {
            let p = Path::from(wal.clone());
            assert!(
                matches!(
                    store.get(&p).await,
                    Err(object_store::Error::NotFound { .. })
                ),
                "WAL {wal} must be deleted after GC"
            );
        }
        let (manifest_gc, _) = {
            let mut cache = s3.manifest_cache.lock().await;
            cache
                .load(
                    Arc::clone(&store),
                    &Path::from("gc-test"),
                    s3.epoch(),
                    std::time::Duration::from_secs(0),
                )
                .await
                .unwrap()
        };
        assert!(
            manifest_gc.wal.is_empty(),
            "manifest.wal must be empty after GC"
        );
        // Data still readable via SST after WAL GC.
        assert_eq!(
            s3.get_bytes("k1").await.unwrap().as_deref(),
            Some(b"v1".as_slice())
        );
        assert_eq!(
            s3.get_bytes("k2").await.unwrap().as_deref(),
            Some(b"v2".as_slice())
        );
    }
}
