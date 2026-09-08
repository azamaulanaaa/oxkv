//! LSM store generic over the [`Storage`] trait — native and `wasm32`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::store::cache::{Cache, LruCache};
use crate::store::sleep;
use crate::store::storage::{ObjectPath, PutMode, Storage};

#[cfg(test)]
use crate::store::storage::MemStorage;

use crate::store::{Direction, GetSet, KeyValue, Result, Store, StoreError, Transaction};

mod blob;
mod manifest;
mod ownership;
mod probe;
mod sst;

pub(crate) use blob::{
    encode_blob_pointer, get_blob, is_overflow, put_blob, try_decode_blob_pointer,
};
pub(crate) use manifest::{Manifest, ManifestCache, SstMeta, cas_manifest};
pub(crate) use ownership::{acquire_ownership, cas_backoff, read_ownership, sst_path, wal_path};
pub(crate) use probe::probe_store;
/// Parsed SST file; name it to weigh a custom [`Cache`] (see [`SstFile::size`]).
pub use sst::SstFile;
pub(crate) use sst::{DEFAULT_BLOCK_SIZE, TOMBSTONE_VLEN, build_sst};

// Path helpers referenced only by unit tests in this module.
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use blob::blob_path;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use manifest::manifest_path;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use ownership::{epoch_prefix, ownership_path};

type MemMap = std::collections::BTreeMap<String, Option<Vec<u8>>>;
type MemTable = Arc<async_lock::RwLock<MemMap>>;
type WalBuffer = Arc<async_lock::Mutex<Vec<(String, Option<Vec<u8>>)>>>;

/// Process-unique session suffix for builders without an explicit session.
/// A counter (not wall time): `std::time` clocks panic on `wasm32`, and
/// fencing safety comes from the monotonic epoch, not session uniqueness.
static SESSION_CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// WAL entries that trigger force-flush + GC maintenance.
///
/// Each write CAS-appends one WAL id to `manifest.json`; without a bound the
/// manifest grows without limit on small-write workloads (the 32 MiB SST
/// threshold never fires), making every write pay O(list) scan + serde.
/// Crossing this count force-flushes an SST and GCs covered WALs, keeping
/// per-write manifest cost flat.
const WAL_MAINTENANCE_COUNT: usize = 1_000;

/// L1 files that trigger a bounding compaction merging the smallest
/// adjacent pair (keeps the SST list — and every read's scan — short).
const L1_MERGE_COUNT: usize = 16;

/// Storage-backed LSM store (probe + fencing + WAL gate + SST).
pub struct OxKvStore<C = LruCache<String, Arc<SstFile>>> {
    inner: Arc<dyn Storage>,
    prefix: ObjectPath,
    epoch: u64,
    session: String,
    mem: MemTable,
    wal_seq: Arc<std::sync::atomic::AtomicU64>,
    wal_buffer: WalBuffer,
    sst_seq: Arc<std::sync::atomic::AtomicU64>,
    manifest_cache: Arc<async_lock::Mutex<ManifestCache>>,
    /// Pinned reader versions for WAL GC watermark.
    /// `BTreeMap<version, count>` — `min_key` is the watermark.
    readers: Arc<async_lock::Mutex<std::collections::BTreeMap<u64, usize>>>,
    /// SST file cache — scan-resistant `S3-FIFO` (~256 MB with 32KB blocks).
    sst_cache: C,
}

impl<C> std::fmt::Debug for OxKvStore<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OxKvStore")
            .field("prefix", &self.prefix)
            .field("epoch", &self.epoch)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// Cache-independent entry points (`builder`, `probe`) live on the default
/// `LruCache` instantiation so `OxKvStore::builder()` needs no turbofish.
impl OxKvStore {
    /// Creates a new store builder.
    #[must_use]
    pub fn builder() -> OxKvStoreBuilder {
        OxKvStoreBuilder {
            inner: None,
            prefix: ObjectPath::default(),
            skip_probe: false,
            session: None,
            assume_single_writer: false,
        }
    }

    /// Runs the storage probe against `store` at `prefix/probe/canary`.
    ///
    /// validates `If-None-Match` / `If-Match`
    /// conditional writes. Returns `Ok(())` only on
    /// `ok (create, reject-create, reject-stale)`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if conditional writes are not enforced.
    pub async fn probe(store: Arc<dyn Storage>, prefix: &ObjectPath) -> Result<()> {
        probe_store(store, prefix).await
    }
}

impl<C> OxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    /// Returns the underlying object store (for tests).
    #[cfg(test)]
    #[must_use]
    pub fn inner_store(&self) -> Arc<dyn Storage> {
        Arc::clone(&self.inner)
    }

    /// Returns the prefix.
    #[cfg(test)]
    #[must_use]
    pub fn prefix(&self) -> &ObjectPath {
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

    /// Stages `set` into `MemTable` + WAL buffer (commit = mem).
    ///
    /// Does not hit storage — use [`Self::flush`] or [`Self::commit_durable_set`] for RPO=0.
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

    /// Flushes buffered WAL ops to `e{epoch}/wal/{seq:08}.log` via
    /// `PutMode::Create` (`If-None-Match:"*"`), then gates on ownership.
    ///
    /// Implements `commit_durable` RPO=0.
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
            if let Some(val) = value {
                crate::store::encode_record(&mut payload_buf, key, val)
                    .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?;
            } else {
                let klen = u32::try_from(key.len())
                    .map_err(|e| StoreError::Storage(format!("key too long: {e}")))?;
                payload_buf.extend_from_slice(&klen.to_le_bytes());
                payload_buf.extend_from_slice(key.as_bytes());
                payload_buf.extend_from_slice(&TOMBSTONE_VLEN.to_le_bytes());
            }
        }

        let seq = self
            .wal_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = wal_path(&self.prefix, self.epoch, seq);

        let put_res = self
            .inner
            .put_opts(&path, payload_buf, PutMode::Create)
            .await;

        match put_res {
            Ok(_) => {}
            Err(e) if e.to_string().contains("CAS conflict") => {}
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
            let (manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            // Owned copy for mutation; readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
            if manifest.wal.iter().any(|w| w == &wal_id) {
                return Ok(());
            }
            manifest.wal.push(wal_id.clone());
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    let wal_len = manifest.wal.len();
                    cache.update(manifest, new_etag);
                    drop(cache);
                    self.maintain_wal(wal_len).await;
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
                    sleep(backoff).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Best-effort WAL maintenance after a manifest CAS carrying `wal_len`
    /// entries: force-flush an SST and GC covered WALs once the list reaches
    /// `WAL_MAINTENANCE_COUNT`, keeping per-write manifest cost flat.
    /// Failures are swallowed (fencing/conflicts/reader pins).
    async fn maintain_wal(&self, wal_len: usize) {
        if wal_len < WAL_MAINTENANCE_COUNT {
            return;
        }
        let _ = self.flush_mem_to_sst_force().await;
        let _ = self.gc_wal().await;
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
                .map(|(k, v)| k.len() + v.as_ref().map_or(0, Vec::len) + 8)
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
        // Fully prefixed id (same layout as `wal_path`) so `fetch_sst` can
        // resolve it with `ObjectPath::from(id)` and manifests stay prefix-safe.
        let sst_id = sst_path(&self.prefix, self.epoch, 0, seq).to_string();
        let sst_path = ObjectPath::from(sst_id.as_str());
        let put_res = self
            .inner
            .put_opts(&sst_path, sst_bytes.clone(), PutMode::Create)
            .await;
        match put_res {
            Ok(_) => {}
            Err(e) if e.to_string().contains("CAS conflict") => {}
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
        let (manifest, etag) = cache
            .load(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                std::time::Duration::from_secs(1),
            )
            .await?;
        // Owned copy for mutation; readers share the cached `Arc`.
        let mut manifest = (*manifest).clone();
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
                sleep(backoff).await;
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
                    return Ok(reloaded.sst.iter().find(|m| m.id == sst_id).cloned());
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

    /// Reads and parses `id` straight from storage, bypassing the SST cache.
    ///
    /// Window scans (`gets`) use this so a wide range never evicts hot
    /// point-lookup entries.
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

    /// Cache-through SST read for point lookups.
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
        // Normalize cursor to [lower, upper] for range filtering; Prev stores
        // upper in cursor.0 and lower in cursor.1.
        let (scan_start, scan_end) = match direction {
            Direction::Next => (cursor.0.as_deref(), cursor.1.as_deref()),
            Direction::Prev => (cursor.1.as_deref(), cursor.0.as_deref()),
        };
        if direction == Direction::Prev && cursor.0.is_none() {
            return Ok(Vec::new());
        }
        let mut sources: Vec<Vec<(String, Option<Vec<u8>>)>> = Vec::new();
        {
            let mem = self.mem.read().await;
            // Filter MemTable by range upfront — page_fetch_100 at LARGE would
            // otherwise clone 1M entries to return 100.
            let mem_vec: Vec<(String, Option<Vec<u8>>)> =
                if scan_start.is_none() && scan_end.is_none() {
                    mem.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                } else {
                    mem.iter()
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
                let min = meta.min_key.as_str();
                let max = meta.max_key.as_str();
                let after_lower = scan_start.is_none_or(|s| max >= s);
                let before_upper = scan_end.is_none_or(|e| min <= e);
                after_lower && before_upper
            };
            if !overlaps {
                continue;
            }
            // Bypass the SST cache: scans must not evict hot entries.
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

    /// Registers a pinned reader at `version` for watermark.
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

    /// Returns the minimum pinned version, if any (watermark).
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

    /// GCs `WAL` entries that are covered by an `SST` and not pinned.
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
            let (manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            // Owned copy for mutation; readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
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
                        let path = ObjectPath::from(wal.as_str());
                        if let Err(e) = self.inner.delete(&path).await {
                            return Err(StoreError::Storage(format!(
                                "delete wal {wal} failed: {e}"
                            )));
                        }
                        deleted += 1;
                    }
                    return Ok(deleted);
                }
                Err(e) if e.to_string().contains("CAS conflict") => {
                    cache.clear();
                    drop(cache);
                    sleep(cas_backoff(0)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(0)
    }

    /// Compacts `L0` into `L1` when `L0 files >=4` or `>128MB`.
    ///
    /// Optimistic: multiple compactors may race; loser reuses `If-None-Match`
    /// SST via idempotency and retries `CAS`. `DELETE` old objects only after
    /// replacement is manifest-visible. Returns new `L1` meta if compacted.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O or `Fenced`.
    #[allow(clippy::too_many_lines)]
    pub async fn compact(&self) -> Result<Option<SstMeta>> {
        // Load manifest and check trigger.
        let (manifest_snapshot, _) = {
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
        let l0_count = manifest_snapshot
            .sst
            .iter()
            .filter(|m| m.level == 0)
            .count();
        let l0_bytes: u64 = manifest_snapshot
            .sst
            .iter()
            .filter(|m| m.level == 0)
            .map(|m| m.size)
            .sum();
        let l1_count = manifest_snapshot
            .sst
            .iter()
            .filter(|m| m.level == 1)
            .count();
        if l0_count < 4 && l0_bytes <= 128 * 1024 * 1024 && l1_count < L1_MERGE_COUNT {
            return Ok(None);
        }
        // Collect L0 range.
        let l0_metas: Vec<SstMeta> = manifest_snapshot
            .sst
            .iter()
            .filter(|m| m.level == 0)
            .cloned()
            .collect();
        let l0_min = l0_metas
            .iter()
            .map(|m| m.min_key.as_str())
            .min()
            .unwrap_or("");
        let l0_max = l0_metas
            .iter()
            .map(|m| m.max_key.as_str())
            .max()
            .unwrap_or("");
        // Overlapping L1.
        let mut l1_overlapping: Vec<SstMeta> = manifest_snapshot
            .sst
            .iter()
            .filter(|m| m.level == 1)
            .filter(|m| !(m.max_key.as_str() < l0_min || m.min_key.as_str() > l0_max))
            .cloned()
            .collect();
        // Bound L1 file count: fold the smallest adjacent pair (plus anything
        // overlapping its range) into this compaction. Adjacent-only keeps L1
        // sorted runs non-overlapping, so L1 tombstone-drop stays sound.
        if l1_count >= L1_MERGE_COUNT {
            let mut by_min: Vec<&SstMeta> = manifest_snapshot
                .sst
                .iter()
                .filter(|m| m.level == 1)
                .collect();
            by_min.sort_by(|a, b| a.min_key.cmp(&b.min_key));
            if let Some(pair) = by_min.windows(2).min_by_key(|w| w[0].size + w[1].size) {
                let lo = pair[0].min_key.as_str().min(pair[1].min_key.as_str());
                let hi = pair[0].max_key.as_str().max(pair[1].max_key.as_str());
                for m in manifest_snapshot.sst.iter().filter(|m| m.level == 1) {
                    if m.max_key.as_str() >= lo
                        && m.min_key.as_str() <= hi
                        && !l1_overlapping.iter().any(|x| x.id == m.id)
                    {
                        l1_overlapping.push(m.clone());
                    }
                }
            }
        }
        if l0_metas.is_empty() && l1_overlapping.is_empty() {
            return Ok(None);
        }
        // Read all overlapping SSTs via heap merge (newest wins, tombstones suppressed in final L1 except needed).
        let mut sources: Vec<Vec<(String, Option<Vec<u8>>)>> = Vec::new();
        for meta in l0_metas.iter().rev().chain(l1_overlapping.iter().rev()) {
            let sst = self.fetch_sst(&meta.id).await?;
            let scan = sst.scan_with_tombstones(None, None, None)?;
            let mut resolved: Vec<(String, Option<Vec<u8>>)> = Vec::with_capacity(scan.len());
            for (k, v) in scan {
                match v {
                    Some(raw) => {
                        // Resolve blob pointers if any (L0 may contain pointers).
                        let val = if let Some(ptr) = try_decode_blob_pointer(&raw) {
                            let blob_path = ObjectPath::from(ptr.blob.as_str());
                            get_blob(Arc::clone(&self.inner), &blob_path).await?
                        } else {
                            raw
                        };
                        resolved.push((k, Some(val)));
                    }
                    None => resolved.push((k, None)),
                }
            }
            sources.push(resolved);
        }
        // Merge newest wins, tombstones kept for now but will be dropped if shadowed at L1 non-overlapping.
        let merged = merge_sources(sources);
        // Build L1 entries: drop tombstones where no older shadow (L1 is non-overlapping, so drop all tombstones).
        let mut l1_entries: std::collections::BTreeMap<String, Option<Vec<u8>>> =
            std::collections::BTreeMap::new();
        for kv in merged {
            // For L1 compacted, tombstones have already been suppressed by merge_sources, so only live keys remain.
            // If we still have tombstone (None) in merged, it would have been filtered, so we only insert live.
            l1_entries.insert(kv.key, Some(kv.value));
        }
        // Handle large values overflow for L1 as well.
        let mut final_entries: std::collections::BTreeMap<String, Option<Vec<u8>>> =
            std::collections::BTreeMap::new();
        for (k, v) in &l1_entries {
            if let Some(val) = v {
                if is_overflow(k, val, 64 * 1024) {
                    let blob_path =
                        put_blob(Arc::clone(&self.inner), &self.prefix, self.epoch, val).await?;
                    let crc = crc32fast::hash(val);
                    let ptr = encode_blob_pointer(&blob_path, val.len(), crc);
                    final_entries.insert(k.clone(), Some(ptr));
                } else {
                    final_entries.insert(k.clone(), Some(val.clone()));
                }
            }
        }
        if final_entries.is_empty() {
            // No live keys — just CAS remove old files.
            for _ in 0..4 {
                let mut cache = self.manifest_cache.lock().await;
                let (manifest, etag) = cache
                    .load(
                        Arc::clone(&self.inner),
                        &self.prefix,
                        self.epoch,
                        std::time::Duration::from_secs(1),
                    )
                    .await?;
                // Owned copy for mutation; readers share the cached `Arc`.
                let mut manifest = (*manifest).clone();
                let before_len = manifest.sst.len();
                manifest.sst.retain(|m| {
                    !(l0_metas.iter().any(|x| x.id == m.id)
                        || l1_overlapping.iter().any(|x| x.id == m.id))
                });
                if manifest.sst.len() == before_len {
                    return Ok(None);
                }
                manifest.version = manifest.version.wrapping_add(1);
                let etag_opt = if etag.is_empty() { None } else { Some(etag) };
                match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await
                {
                    Ok(new_etag) => {
                        cache.update(manifest.clone(), new_etag);
                        drop(cache);
                        for m in l0_metas.iter().chain(l1_overlapping.iter()) {
                            let p = ObjectPath::from(m.id.as_str());
                            let _ = self.inner.delete(&p).await;
                        }
                        return Ok(None);
                    }
                    Err(e) if e.to_string().contains("CAS conflict") => {
                        cache.clear();
                        drop(cache);
                        sleep(cas_backoff(0)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
            return Ok(None);
        }
        let sst_bytes = build_sst(&final_entries, 64 * 1024)?;
        let seq = self
            .sst_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let l1_id = sst_path(&self.prefix, self.epoch, 1, seq).to_string();
        let l1_path = ObjectPath::from(l1_id.as_str());
        let put_res = self
            .inner
            .put_opts(&l1_path, sst_bytes.clone(), PutMode::Create)
            .await;
        match put_res {
            Ok(_) => {}
            Err(e) if e.to_string().contains("CAS conflict") => {}
            Err(e) => return Err(StoreError::Storage(format!("put L1 sst failed: {e}"))),
        }
        // Verify still owner.
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
                    "fenced: ownership missing before compaction CAS".to_string(),
                ));
            }
        }
        // CAS manifest: remove old L0/L1 overlapping, add new L1.
        for _ in 0..4 {
            let mut cache = self.manifest_cache.lock().await;
            let (manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            // Owned copy for mutation; readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
            // Idempotency: if new L1 already present, reuse.
            if manifest.sst.iter().any(|m| m.id == l1_id) {
                let existing = manifest.sst.iter().find(|m| m.id == l1_id).cloned();
                return Ok(existing);
            }
            let mut new_sst_list: Vec<SstMeta> = manifest
                .sst
                .iter()
                .filter(|m| {
                    !(l0_metas.iter().any(|x| x.id == m.id)
                        || l1_overlapping.iter().any(|x| x.id == m.id))
                })
                .cloned()
                .collect();
            let new_meta = SstMeta {
                id: l1_id.clone(),
                level: 1,
                min_key: final_entries.keys().next().cloned().unwrap_or_default(),
                max_key: final_entries
                    .keys()
                    .next_back()
                    .cloned()
                    .unwrap_or_default(),
                size: sst_bytes.len() as u64,
            };
            new_sst_list.push(new_meta.clone());
            // Keep L1 non-overlapping sorted by min_key for future.
            new_sst_list.sort_by(|a, b| a.min_key.cmp(&b.min_key));
            manifest.sst = new_sst_list;
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    cache.update(manifest, new_etag);
                    drop(cache);
                    // Invalidate sst_cache for deleted, keep new.
                    for m in l0_metas.iter().chain(l1_overlapping.iter()) {
                        self.sst_cache.remove(&m.id).await;
                        let p = ObjectPath::from(m.id.as_str());
                        let _ = self.inner.delete(&p).await;
                    }
                    return Ok(Some(new_meta));
                }
                Err(e) if e.to_string().contains("CAS conflict") => {
                    cache.clear();
                    drop(cache);
                    sleep(cas_backoff(0)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Store trait impl — persistent GetSet + transactional (deferred commit)
// ---------------------------------------------------------------------------

/// Transaction for `OxKvStore` — staged overlay, durable only on `commit`.
///
/// `stage_set`/`stage_delete` are buffered in `overlay` and invisible to
/// the parent `OxKvStore` until `commit` applies them to the shared
/// `MemTable`/`WalBuffer` and `flush`es the WAL to S3 (RPO=0).
/// `get`/`has`/`gets` see `overlay` first (read-your-writes) then the
/// parent's `MemTable` + `SST`s via the same heap-merge.
pub struct OxKvTx<C = LruCache<String, Arc<SstFile>>> {
    inner: Arc<dyn Storage>,
    prefix: ObjectPath,
    epoch: u64,
    session: String,
    mem: MemTable,
    wal_seq: Arc<std::sync::atomic::AtomicU64>,
    sst_seq: Arc<std::sync::atomic::AtomicU64>,
    manifest_cache: Arc<async_lock::Mutex<ManifestCache>>,
    readers: Arc<async_lock::Mutex<std::collections::BTreeMap<u64, usize>>>,
    sst_cache: C,
    overlay: std::collections::BTreeMap<String, Option<Vec<u8>>>,
}

impl<C> OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
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

    /// Reads and parses `id` straight from storage, bypassing the SST cache.
    ///
    /// Window scans (`gets`) use this so a wide range never evicts hot
    /// point-lookup entries.
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

    /// Cache-through SST read for point lookups.
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

    /// Store view sharing all mutable state, for running maintenance
    /// (SST flush, GC, compaction) from tx-only workloads.
    ///
    /// Its private WAL buffer is never touched by those paths; fencing,
    /// pinning, and CAS discipline all operate on the shared state, so
    /// failures behave exactly like store-side maintenance.
    fn maintenance_view(&self) -> OxKvStore<C> {
        OxKvStore {
            inner: Arc::clone(&self.inner),
            prefix: self.prefix.clone(),
            epoch: self.epoch,
            session: self.session.clone(),
            mem: Arc::clone(&self.mem),
            wal_seq: Arc::clone(&self.wal_seq),
            wal_buffer: Arc::new(async_lock::Mutex::new(Vec::new())),
            sst_seq: Arc::clone(&self.sst_seq),
            manifest_cache: Arc::clone(&self.manifest_cache),
            readers: Arc::clone(&self.readers),
            sst_cache: self.sst_cache.clone(),
        }
    }
}

#[async_trait]
impl<C> GetSet for OxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        OxKvStore::get_bytes(self, key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        OxKvStore::has(self, key).await
    }

    async fn delete(&mut self, key: &str) -> Result<bool> {
        let prev = OxKvStore::get_bytes(self, key).await?;
        let existed = prev.is_some();
        if !existed {
            return Ok(false);
        }
        // Atomic: encode tombstone and flush WAL before mutating MemTable
        let mut payload_buf = Vec::new();
        let klen = u32::try_from(key.len())
            .map_err(|e| StoreError::Storage(format!("key too long: {e}")))?;
        payload_buf.extend_from_slice(&klen.to_le_bytes());
        payload_buf.extend_from_slice(key.as_bytes());
        payload_buf.extend_from_slice(&TOMBSTONE_VLEN.to_le_bytes());
        let seq = self
            .wal_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = wal_path(&self.prefix, self.epoch, seq);
        let put_res = self
            .inner
            .put_opts(&path, payload_buf, PutMode::Create)
            .await;
        match put_res {
            Ok(_) => {}
            Err(e) if e.to_string().contains("CAS conflict") => {}
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
            let (manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            // Owned copy for mutation; readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
            if manifest.wal.iter().any(|w| w == &wal_id) {
                self.mem.write().await.insert(key.to_string(), None);
                return Ok(true);
            }
            manifest.wal.push(wal_id.clone());
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    let wal_len = manifest.wal.len();
                    cache.update(manifest, new_etag);
                    drop(cache);
                    self.mem.write().await.insert(key.to_string(), None);
                    // Best-effort auto maintenance — ignore fencing/conflicts
                    let _ = self.flush_mem_to_sst().await;
                    let _ = self.compact().await;
                    self.maintain_wal(wal_len).await;
                    return Ok(true);
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
                    sleep(backoff).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = OxKvStore::get_bytes(self, key).await?;
        self.put_bytes(key, value).await?;
        Ok(prev)
    }

    async fn put_bytes(&mut self, key: &str, value: &[u8]) -> Result<()> {
        // Atomic: encode and flush before mutating MemTable
        let mut payload_buf = Vec::new();
        crate::store::encode_record(&mut payload_buf, key, value)
            .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?;
        let seq = self
            .wal_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = wal_path(&self.prefix, self.epoch, seq);
        let put_res = self
            .inner
            .put_opts(&path, payload_buf, PutMode::Create)
            .await;
        match put_res {
            Ok(_) => {}
            Err(e) if e.to_string().contains("CAS conflict") => {}
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
            let (manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            // Owned copy for mutation; readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
            if manifest.wal.iter().any(|w| w == &wal_id) {
                drop(cache);
                self.mem
                    .write()
                    .await
                    .insert(key.to_string(), Some(value.to_vec()));
                let _ = self.flush_mem_to_sst().await;
                let _ = self.compact().await;
                return Ok(());
            }
            manifest.wal.push(wal_id.clone());
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    let wal_len = manifest.wal.len();
                    cache.update(manifest, new_etag);
                    drop(cache);
                    self.mem
                        .write()
                        .await
                        .insert(key.to_string(), Some(value.to_vec()));
                    let _ = self.flush_mem_to_sst().await;
                    let _ = self.compact().await;
                    self.maintain_wal(wal_len).await;
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
                    sleep(backoff).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        OxKvStore::gets_bytes(self, limit, direction, cursor).await
    }
}

#[async_trait]
impl<C> GetSet for OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if let Some(v) = self.overlay.get(key) {
            return match v {
                Some(raw) => Ok(Some(self.resolve_value(raw.clone()).await?)),
                None => Ok(None),
            };
        }
        // Check shared MemTable
        {
            let mem = self.mem.read().await;
            if let Some(v) = mem.get(key) {
                return match v {
                    Some(raw) => Ok(Some(self.resolve_value(raw.clone()).await?)),
                    None => Ok(None),
                };
            }
        }
        // Scan SSTs
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
                None => {}
            }
        }
        Ok(None)
    }

    async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get_bytes(key).await?.is_some())
    }

    async fn delete(&mut self, key: &str) -> Result<bool> {
        let existed = self.get_bytes(key).await?.is_some();
        if existed {
            self.overlay.insert(key.to_string(), None);
        }
        Ok(existed)
    }

    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = self.get_bytes(key).await?;
        self.overlay.insert(key.to_string(), Some(value.to_vec()));
        Ok(prev)
    }

    async fn put_bytes(&mut self, key: &str, value: &[u8]) -> Result<()> {
        self.overlay.insert(key.to_string(), Some(value.to_vec()));
        Ok(())
    }

    async fn gets_bytes(
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
        let mut sources: Vec<Vec<(String, Option<Vec<u8>>)>> = Vec::new();
        // Overlay newest — filter by range to avoid cloning entire overlay
        // when only a page is needed.
        let overlay_vec: Vec<(String, Option<Vec<u8>>)> =
            if scan_start.is_none() && scan_end.is_none() {
                self.overlay
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            } else {
                self.overlay
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
        sources.push(overlay_vec);
        // Shared MemTable
        {
            let mem = self.mem.read().await;
            let mem_vec: Vec<(String, Option<Vec<u8>>)> =
                if scan_start.is_none() && scan_end.is_none() {
                    mem.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                } else {
                    mem.iter()
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
            sources.push(mem_vec);
        }
        // SSTs
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
                let min = meta.min_key.as_str();
                let max = meta.max_key.as_str();
                let after_lower = scan_start.is_none_or(|s| max >= s);
                let before_upper = scan_end.is_none_or(|e| min <= e);
                after_lower && before_upper
            };
            if !overlaps {
                continue;
            }
            // Bypass the SST cache: scans must not evict hot entries.
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

#[async_trait]
impl<C> Transaction for OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    #[allow(clippy::too_many_lines)]
    async fn commit(mut self) -> Result<()> {
        if self.overlay.is_empty() {
            return Ok(());
        }
        // Encode directly from overlay — don't mutate shared mem until WAL is durable (atomic)
        let mut payload_buf = Vec::new();
        for (key, value) in &self.overlay {
            if let Some(val) = value {
                crate::store::encode_record(&mut payload_buf, key, val)
                    .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?;
            } else {
                let klen = u32::try_from(key.len())
                    .map_err(|e| StoreError::Storage(format!("key too long: {e}")))?;
                payload_buf.extend_from_slice(&klen.to_le_bytes());
                payload_buf.extend_from_slice(key.as_bytes());
                payload_buf.extend_from_slice(&TOMBSTONE_VLEN.to_le_bytes());
            }
        }
        let seq = self
            .wal_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = wal_path(&self.prefix, self.epoch, seq);
        let put_res = self
            .inner
            .put_opts(&path, payload_buf, PutMode::Create)
            .await;
        match put_res {
            Ok(_) => {}
            Err(e) if e.to_string().contains("CAS conflict") => {}
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
            let (manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            // Owned copy for mutation; readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
            if manifest.wal.iter().any(|w| w == &wal_id) {
                // Idempotent retry — WAL already durable, apply overlay to MemTable
                {
                    let mut mem = self.mem.write().await;
                    for (k, v) in self.overlay.clone() {
                        mem.insert(k, v);
                    }
                }
                return Ok(());
            }
            manifest.wal.push(wal_id.clone());
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    let wal_len = manifest.wal.len();
                    cache.update(manifest, new_etag);
                    drop(cache);
                    // Now durable — apply overlay to MemTable.
                    let overlay = std::mem::take(&mut self.overlay);
                    {
                        let mut mem = self.mem.write().await;
                        for (k, v) in overlay {
                            mem.insert(k, v);
                        }
                    }
                    // Store-side writes maintain on every op; tx-only workloads
                    // would otherwise grow mem and the WAL list without bound.
                    let view = self.maintenance_view();
                    let _ = view.flush_mem_to_sst().await;
                    let _ = view.compact().await;
                    if wal_len >= WAL_MAINTENANCE_COUNT {
                        let _ = view.flush_mem_to_sst_force().await;
                        let _ = view.gc_wal().await;
                    }
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
                    sleep(backoff).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn rollback(self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl<C> Store for OxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    type Transaction = OxKvTx<C>;

    fn begin_tx(&mut self) -> Result<Self::Transaction> {
        Ok(OxKvTx {
            inner: Arc::clone(&self.inner),
            prefix: self.prefix.clone(),
            epoch: self.epoch,
            session: self.session.clone(),
            mem: Arc::clone(&self.mem),
            wal_seq: Arc::clone(&self.wal_seq),
            sst_seq: Arc::clone(&self.sst_seq),
            manifest_cache: Arc::clone(&self.manifest_cache),
            readers: Arc::clone(&self.readers),
            sst_cache: self.sst_cache.clone(),
            overlay: std::collections::BTreeMap::new(),
        })
    }
}

/// Builder for [`OxKvStore`].
#[derive(Default)]
pub struct OxKvStoreBuilder {
    inner: Option<Arc<dyn Storage>>,
    prefix: ObjectPath,
    skip_probe: bool,
    session: Option<String>,
    assume_single_writer: bool,
}

impl std::fmt::Debug for OxKvStoreBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OxKvStoreBuilder")
            .field("prefix", &self.prefix)
            .field("skip_probe", &self.skip_probe)
            .field("assume_single_writer", &self.assume_single_writer)
            .field("has_store", &self.inner.is_some())
            .finish_non_exhaustive()
    }
}

impl OxKvStoreBuilder {
    /// Sets the backing [`Storage`] (use `MemStorage` in tests and wasm;
    /// on native, an `object_store` backend also works via `with_object_store`).
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn Storage>) -> Self {
        self.inner = Some(store);
        self
    }

    /// Sets the backing store from an `object_store` backend (native-only,
    /// requires the `oxkv-s3` feature).
    #[cfg(all(not(target_arch = "wasm32"), feature = "oxkv-s3"))]
    #[must_use]
    pub fn with_object_store(mut self, store: Arc<dyn object_store::ObjectStore>) -> Self {
        self.inner = Some(Arc::new(store));
        self
    }

    /// Sets the key prefix inside the bucket (e.g. `ObjectPath::from("oxkv")`).
    #[must_use]
    pub fn with_prefix(mut self, prefix: ObjectPath) -> Self {
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

    /// Skips the startup storage probe.
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

    /// Asserts no other writer touches the prefix: `TTL`-fresh manifests
    /// return from cache without a revalidation poll (one roundtrip saved
    /// per operation).
    ///
    /// Only enable when a single writer owns the prefix. Takeover is still
    /// detected via ownership checks and manifest CAS conflicts.
    #[must_use]
    pub fn assume_single_writer(mut self, assume: bool) -> Self {
        self.assume_single_writer = assume;
        self
    }

    /// Builds the store with the default SST cache (256 MB scan-resistant
    /// `S3-FIFO`, see [`LruCache`]),
    /// running the probe unless skipped, then CAS-acquires `ownership.json`
    /// epoch. The returned store is fenced to that epoch.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails or `StoreError::Fenced`
    /// if `ownership.json` CAS loses the race.
    pub async fn build(self) -> Result<OxKvStore> {
        self.build_with_cache(LruCache::new(
            256 * 1024 * 1024,
            |_: &String, v: &Arc<SstFile>| u32::try_from(v.size()).unwrap_or(u32::MAX),
        ))
        .await
    }

    /// Builds the store with a caller-supplied SST cache, running the probe
    /// unless skipped, then CAS-acquires `ownership.json` epoch.
    ///
    /// Pass the default [`LruCache`] (`S3-FIFO`, see [`Self::build`]), a
    /// `moka::future::Cache` (native-only, `moka` feature), or any custom
    /// [`Cache`] implementation.
    ///
    /// ```rust,ignore
    /// let cache = moka::future::Cache::builder()
    ///     .max_capacity(256 * 1024 * 1024)
    ///     .weigher(|_: &String, v: &Arc<SstFile>| {
    ///         u32::try_from(v.size()).unwrap_or(u32::MAX)
    ///     })
    ///     .build();
    /// let store = OxKvStoreBuilder::new()
    ///     .with_store(Arc::new(MemStorage::new()))
    ///     .build_with_cache(cache)
    ///     .await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails or `StoreError::Fenced`
    /// if `ownership.json` CAS loses the race.
    #[allow(clippy::too_many_lines)]
    pub async fn build_with_cache<C>(self, cache: C) -> Result<OxKvStore<C>>
    where
        C: Cache<String, Arc<SstFile>>,
    {
        let store = self.inner.ok_or_else(|| {
            StoreError::Storage("OxKvStore requires a Storage via with_store()".to_string())
        })?;

        if !self.skip_probe {
            probe_store(Arc::clone(&store), &self.prefix).await?;
        }

        let session = self.session.unwrap_or_else(|| {
            let n = SESSION_CTR.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            format!("sess-{n}")
        });
        let rec = acquire_ownership(Arc::clone(&store), &self.prefix, &session).await?;

        let mut manifest_cache = ManifestCache::new();
        manifest_cache.set_skip_revalidation(self.assume_single_writer);
        let manifest_cache = Arc::new(async_lock::Mutex::new(manifest_cache));

        let s3store = OxKvStore {
            inner: Arc::clone(&store),
            prefix: self.prefix.clone(),
            epoch: rec.epoch,
            session: session.clone(),
            mem: Arc::new(async_lock::RwLock::new(std::collections::BTreeMap::new())),
            wal_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            wal_buffer: Arc::new(async_lock::Mutex::new(Vec::new())),
            sst_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            manifest_cache,
            readers: Arc::new(async_lock::Mutex::new(std::collections::BTreeMap::new())),
            sst_cache: cache,
        };
        // WAL replay for restart/f fencing — make not-yet-SSTed WAL visible
        {
            let mut cache = s3store.manifest_cache.lock().await;
            let (manifest, _etag) = match cache
                .load(
                    Arc::clone(&s3store.inner),
                    &s3store.prefix,
                    s3store.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await
            {
                Ok(v) => v,
                Err(_) => (Arc::new(Manifest::empty(s3store.epoch)), String::new()),
            };
            for wal_id in &manifest.wal {
                let path = ObjectPath::from(wal_id.clone());
                let Ok(out) = s3store.inner.get(&path).await else {
                    continue;
                };
                let data = out.bytes;
                let mut pos = 0usize;
                let mut mem = s3store.mem.write().await;
                while pos + 4 <= data.len() {
                    let klen = u32::from_le_bytes([
                        data[pos],
                        data[pos + 1],
                        data[pos + 2],
                        data[pos + 3],
                    ]) as usize;
                    if pos + 4 + klen + 4 > data.len() {
                        break;
                    }
                    let key = match std::str::from_utf8(&data[pos + 4..pos + 4 + klen]) {
                        Ok(k) => k.to_string(),
                        Err(_) => break,
                    };
                    let v_start = pos + 4 + klen;
                    let vlen = u32::from_le_bytes([
                        data[v_start],
                        data[v_start + 1],
                        data[v_start + 2],
                        data[v_start + 3],
                    ]) as usize;
                    if vlen == TOMBSTONE_VLEN as usize {
                        mem.insert(key, None);
                        pos = v_start + 4;
                    } else {
                        if v_start + 4 + vlen > data.len() {
                            break;
                        }
                        let val = data[v_start + 4..v_start + 4 + vlen].to_vec();
                        mem.insert(key, Some(val));
                        pos = v_start + 4 + vlen;
                    }
                }
            }
            let cur_epoch = s3store.epoch;
            let wal_prefix = format!("e{cur_epoch:06}/wal/");
            let wal_count = manifest
                .wal
                .iter()
                .filter(|id| id.starts_with(&wal_prefix))
                .count() as u64;
            s3store
                .wal_seq
                .store(wal_count, std::sync::atomic::Ordering::SeqCst);
            let sst_prefix = format!("e{cur_epoch:06}/sst/");
            let mut max_sst: u64 = 0;
            for meta in &manifest.sst {
                if meta.id.starts_with(&sst_prefix)
                    && let Some(fname) = meta.id.rsplit('/').next()
                    && let Some(num) = fname
                        .strip_suffix(".sst")
                        .and_then(|s| s.parse::<u64>().ok())
                {
                    max_sst = max_sst.max(num + 1);
                }
            }
            s3store
                .sst_seq
                .store(max_sst, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(s3store)
    }
}

// ---------------------------------------------------------------------------
// Heap merge for gets_bytes over MemTable + SSTs
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
pub(crate) fn new_in_memory() -> Arc<dyn Storage> {
    Arc::new(MemStorage::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::storage::{GetOptions, GetOutput, MemStorage, ObjectVersion, PutOutcome};

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn probe_rejects_b2_like_store_via_builder() {
        #[derive(Clone)]
        struct NoConditionStore {
            inner: MemStorage,
        }

        #[async_trait::async_trait]
        impl Storage for NoConditionStore {
            async fn get(&self, path: &ObjectPath) -> Result<GetOutput> {
                self.inner.get(path).await
            }

            async fn get_opts(&self, path: &ObjectPath, options: GetOptions) -> Result<GetOutput> {
                self.inner.get_opts(path, options).await
            }

            async fn put_opts(
                &self,
                path: &ObjectPath,
                payload: Vec<u8>,
                _mode: PutMode,
            ) -> Result<PutOutcome> {
                // Ignore conditional modes: unconditional overwrite (no fencing support).
                self.inner.delete(path).await?;
                self.inner.put_opts(path, payload, PutMode::Create).await
            }

            async fn delete(&self, path: &ObjectPath) -> Result<()> {
                self.inner.delete(path).await
            }
        }

        let bad: Arc<dyn Storage> = Arc::new(NoConditionStore {
            inner: MemStorage::new(),
        });
        let err = probe_store(Arc::clone(&bad), &ObjectPath::default())
            .await
            .expect_err("must reject");
        assert!(
            err.to_string().contains("conditional writes not enforced")
                || err.to_string().contains("stale If-Match"),
            "unexpected error: {err}"
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn builder_runs_probe_by_default() {
        let store = new_in_memory();
        let built = OxKvStore::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(ObjectPath::from("oxkv"))
            .build()
            .await
            .expect("builder with InMemory must pass probe");
        assert_eq!(built.prefix().as_str(), "oxkv");
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn builder_skip_probe_flag() {
        assert!(!OxKvStore::builder().is_skip_probe());
        assert!(OxKvStore::builder().skip_probe(true).is_skip_probe());
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn builder_skip_probe_allows_b2_like_store() {
        #[derive(Clone)]
        struct NoConditionStore {
            inner: MemStorage,
        }
        #[async_trait::async_trait]
        impl Storage for NoConditionStore {
            async fn get(&self, path: &ObjectPath) -> Result<GetOutput> {
                self.inner.get(path).await
            }
            async fn get_opts(&self, path: &ObjectPath, options: GetOptions) -> Result<GetOutput> {
                self.inner.get_opts(path, options).await
            }
            async fn put_opts(
                &self,
                path: &ObjectPath,
                payload: Vec<u8>,
                _mode: PutMode,
            ) -> Result<PutOutcome> {
                // Ignore conditional modes: unconditional overwrite (no fencing support).
                self.inner.delete(path).await?;
                self.inner.put_opts(path, payload, PutMode::Create).await
            }
            async fn delete(&self, path: &ObjectPath) -> Result<()> {
                self.inner.delete(path).await
            }
        }

        let bad: Arc<dyn Storage> = Arc::new(NoConditionStore {
            inner: MemStorage::new(),
        });
        let err = OxKvStore::builder()
            .with_store(Arc::clone(&bad))
            .build()
            .await
            .expect_err("must reject without skip_probe");
        assert!(err.to_string().contains("conditional writes"));

        OxKvStore::builder()
            .with_store(bad)
            .skip_probe(true)
            .build()
            .await
            .expect("skip_probe must allow bad store");
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn probe_static_entry_point() {
        let store = new_in_memory();
        OxKvStore::probe(store, &ObjectPath::default())
            .await
            .expect("static probe must pass");
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn ownership_path_no_prefix() {
        assert_eq!(
            ownership_path(&ObjectPath::default()).as_str(),
            "ownership.json"
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn ownership_path_with_prefix() {
        assert_eq!(
            ownership_path(&ObjectPath::from("oxkv")).as_str(),
            "oxkv/ownership.json"
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn manifest_path_and_epoch_prefix_formatting() {
        assert_eq!(
            manifest_path(&ObjectPath::default()).as_str(),
            "manifest.json"
        );
        assert_eq!(epoch_prefix(&ObjectPath::default(), 7).as_str(), "e000007");
        assert_eq!(
            epoch_prefix(&ObjectPath::from("oxkv"), 7).as_str(),
            "oxkv/e000007"
        );
        assert_eq!(
            wal_path(&ObjectPath::from("oxkv"), 7, 42).as_str(),
            "oxkv/e000007/wal/00000042.log"
        );
        assert_eq!(
            sst_path(&ObjectPath::from("oxkv"), 7, 0, 123).as_str(),
            "oxkv/e000007/sst/L0/000000123.sst"
        );
        assert_eq!(
            blob_path(&ObjectPath::from("oxkv"), 7, "abc").as_str(),
            "oxkv/e000007/blob/abc"
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn fencing_acquire_increments_epoch() {
        let store = new_in_memory();
        let prefix = ObjectPath::from("oxkv");
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

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn fencing_stale_writer_superseded_prefix_invisible() {
        let store = new_in_memory();
        let prefix = ObjectPath::from("oxkv");
        let r1 = acquire_ownership(Arc::clone(&store), &prefix, "node-a")
            .await
            .unwrap();
        let wal1 = wal_path(&prefix, r1.epoch, 1);
        store
            .put_opts(&wal1, b"wal1".to_vec(), PutMode::Create)
            .await
            .unwrap();

        let r2 = acquire_ownership(Arc::clone(&store), &prefix, "node-b")
            .await
            .unwrap();
        assert_eq!(r2.epoch, 2);
        let wal2 = wal_path(&prefix, r2.epoch, 1);
        store
            .put_opts(&wal2, b"wal2".to_vec(), PutMode::Create)
            .await
            .unwrap();

        let stale_ver = ObjectVersion {
            e_tag: Some("\"stale-etag-r1\"".to_string()),
            version: None,
        };
        let stale_path = ownership_path(&prefix);
        let err = store
            .put_opts(&stale_path, b"stale".to_vec(), PutMode::Update(stale_ver))
            .await
            .expect_err("stale If-Match must be rejected");
        assert!(
            err.to_string().contains("CAS conflict"),
            "unexpected error: {err}"
        );

        let got1 = store.get(&wal1).await.expect("old epoch wal isolated");
        assert_eq!(got1.bytes, b"wal1");
        let got2 = store.get(&wal2).await.expect("new epoch wal");
        assert_eq!(got2.bytes, b"wal2");
        let cur = read_ownership(store, &prefix).await.unwrap().unwrap();
        assert_eq!(cur.epoch, 2);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn wal_durable_and_sst_with_overflow() {
        let store = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(ObjectPath::from("oxkv"))
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

    #[cfg(all(feature = "moka", not(target_arch = "wasm32")))]
    #[tokio::test]
    async fn build_with_cache_accepts_moka() {
        let cache = moka::future::Cache::builder()
            .max_capacity(64 * 1024 * 1024)
            .weigher(|_: &String, v: &Arc<SstFile>| u32::try_from(v.size()).unwrap_or(u32::MAX))
            .build();
        let s3 = OxKvStore::builder()
            .with_store(new_in_memory())
            .with_prefix(ObjectPath::from("oxkv-moka"))
            .with_session("sess-moka")
            .build_with_cache(cache.clone())
            .await
            .unwrap();

        s3.stage_set("k1", b"v1").await;
        s3.flush_mem_to_sst_force()
            .await
            .expect("sst flush")
            .expect("some sst");

        // Point path is cache-through: miss inserts, hit serves.
        let got = s3.get_bytes("k1").await.unwrap().expect("k1");
        assert_eq!(got, b"v1");
        cache.run_pending_tasks().await;
        assert_eq!(cache.entry_count(), 1);
        let got = s3.get_bytes("k1").await.unwrap().expect("k1 again");
        assert_eq!(got, b"v1");

        // Scan path bypasses the cache but stays correct.
        let rows = s3
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        cache.run_pending_tasks().await;
        assert_eq!(cache.entry_count(), 1);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn manifest_wal_list_stays_bounded() {
        let inner = new_in_memory();
        let mut s3 = OxKvStore::builder()
            .with_store(Arc::clone(&inner))
            .with_prefix(ObjectPath::from("oxkv-wal-bound"))
            .with_session("sess-wal-bound")
            .skip_probe(true)
            .build()
            .await
            .unwrap();

        // 2.5x the maintenance threshold: without the force-flush + GC drain
        // the list would hold every WAL id (quadratic manifest cost).
        for i in 0..2_500 {
            s3.set_bytes(&format!("k{i:05}"), b"v").await.unwrap();
        }

        let path = ObjectPath::from("oxkv-wal-bound").child("manifest.json");
        let out = inner.get(&path).await.expect("manifest readable");
        let manifest: Manifest = serde_json::from_slice(&out.bytes).expect("manifest parses");
        assert!(
            manifest.wal.len() <= WAL_MAINTENANCE_COUNT,
            "wal list len {}",
            manifest.wal.len()
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn compact_bounds_l1_file_count() {
        let inner = new_in_memory();
        let mut s3 = OxKvStore::builder()
            .with_store(Arc::clone(&inner))
            .with_prefix(ObjectPath::from("oxkv-l1-bound"))
            .with_session("sess-l1-bound")
            .skip_probe(true)
            .build()
            .await
            .unwrap();

        // 100 rounds of disjoint ranges: each L0-triggered compact adds one
        // L1, so without pair-collapse the count would reach 25. With it,
        // the count oscillates around the threshold once crossed (~64 rounds).
        for i in 0..100 {
            for j in 0..4 {
                s3.put_bytes(&format!("c{i:03}/k{j}"), b"v").await.unwrap();
            }
            s3.flush_mem_to_sst_force()
                .await
                .expect("sst flush")
                .expect("some sst");
            s3.compact().await.unwrap();
        }

        let path = ObjectPath::from("oxkv-l1-bound").child("manifest.json");
        let out = inner.get(&path).await.expect("manifest readable");
        let manifest: Manifest = serde_json::from_slice(&out.bytes).expect("manifest parses");
        let l1 = manifest.sst.iter().filter(|m| m.level == 1).count();
        // Lower bound proves compactions actually ran (not a vacuous zero);
        // upper bound proves pair-collapse engaged (unfixed would be 25).
        assert!((10..=L1_MERGE_COUNT + 2).contains(&l1), "l1 files {l1}");
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn tx_only_workload_stays_bounded() {
        let inner = new_in_memory();
        let mut s3 = OxKvStore::builder()
            .with_store(Arc::clone(&inner))
            .with_prefix(ObjectPath::from("oxkv-tx-bound"))
            .with_session("sess-tx-bound")
            .skip_probe(true)
            .build()
            .await
            .unwrap();

        // 2.5x the maintenance threshold in single-write commits: without
        // tx-side maintenance the WAL list would hold every commit's id and
        // mem would never reach an SST.
        for i in 0..2_500 {
            let mut tx = s3.begin_tx().unwrap();
            tx.set_bytes(&format!("t{i:05}"), b"v").await.unwrap();
            tx.commit().await.unwrap();
        }

        assert_eq!(
            s3.get_bytes("t00042").await.unwrap().as_deref(),
            Some(b"v".as_slice())
        );

        let path = ObjectPath::from("oxkv-tx-bound").child("manifest.json");
        let out = inner.get(&path).await.expect("manifest readable");
        let manifest: Manifest = serde_json::from_slice(&out.bytes).expect("manifest parses");
        assert!(
            manifest.wal.len() <= WAL_MAINTENANCE_COUNT,
            "wal list len {}",
            manifest.wal.len()
        );
        assert!(!manifest.sst.is_empty(), "tx flushes created SSTs");
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn put_bytes_matches_set_bytes() {
        let mut s3 = OxKvStore::builder()
            .with_store(new_in_memory())
            .with_prefix(ObjectPath::from("oxkv-put"))
            .with_session("sess-put")
            .skip_probe(true)
            .build()
            .await
            .unwrap();

        // Fresh key: blind write lands, overwrite replaces.
        s3.put_bytes("k1", b"v1").await.unwrap();
        assert_eq!(
            s3.get_bytes("k1").await.unwrap().as_deref(),
            Some(b"v1".as_slice())
        );
        s3.put_bytes("k1", b"v2").await.unwrap();
        assert_eq!(
            s3.get_bytes("k1").await.unwrap().as_deref(),
            Some(b"v2".as_slice())
        );
        // Parity: set_bytes on the same key reports the put value as prev.
        let prev = s3.set_bytes("k1", b"v3").await.unwrap();
        assert_eq!(prev.as_deref(), Some(b"v2".as_slice()));

        // Tx staging stays invisible until commit, like set_bytes.
        let mut tx = s3.begin_tx().unwrap();
        tx.put_bytes("tk", b"tv").await.unwrap();
        assert_eq!(s3.get_bytes("tk").await.unwrap(), None);
        tx.commit().await.unwrap();
        assert_eq!(
            s3.get_bytes("tk").await.unwrap().as_deref(),
            Some(b"tv".as_slice())
        );
    }

    /// Counts `get_opts` calls (manifest polls) while delegating everything.
    #[derive(Clone)]
    struct CountingStore {
        inner: MemStorage,
        get_opts_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Storage for CountingStore {
        async fn get(&self, path: &ObjectPath) -> Result<GetOutput> {
            self.inner.get(path).await
        }

        async fn get_opts(&self, path: &ObjectPath, options: GetOptions) -> Result<GetOutput> {
            self.get_opts_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.get_opts(path, options).await
        }

        async fn put_opts(
            &self,
            path: &ObjectPath,
            payload: Vec<u8>,
            mode: PutMode,
        ) -> Result<PutOutcome> {
            self.inner.put_opts(path, payload, mode).await
        }

        async fn delete(&self, path: &ObjectPath) -> Result<()> {
            self.inner.delete(path).await
        }
    }

    async fn poll_count_fixture(
        single_writer: bool,
    ) -> (OxKvStore, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = CountingStore {
            inner: MemStorage::new(),
            get_opts_calls: Arc::clone(&calls),
        };
        let mut builder = OxKvStore::builder()
            .with_store(Arc::new(counting))
            .with_prefix(ObjectPath::from(format!("oxkv-poll-{single_writer}")))
            .with_session(format!("sess-poll-{single_writer}"))
            .skip_probe(true);
        if single_writer {
            builder = builder.assume_single_writer(true);
        }
        let mut s3 = builder.build().await.unwrap();
        s3.put_bytes("k1", b"v1").await.unwrap();
        assert_eq!(
            s3.get_bytes("k1").await.unwrap().as_deref(),
            Some(b"v1".as_slice())
        );
        (s3, calls)
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn single_writer_skips_manifest_polls() {
        let (s3, calls) = poll_count_fixture(true).await;
        let baseline = calls.load(std::sync::atomic::Ordering::SeqCst);
        // Miss MemTable so every read reaches the manifest load.
        for _ in 0..10 {
            assert_eq!(s3.get_bytes("missing").await.unwrap(), None);
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), baseline);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn default_mode_still_polls_manifest() {
        let (s3, calls) = poll_count_fixture(false).await;
        let baseline = calls.load(std::sync::atomic::Ordering::SeqCst);
        // Miss MemTable so every read reaches the manifest load.
        for _ in 0..10 {
            assert_eq!(s3.get_bytes("missing").await.unwrap(), None);
        }
        assert!(calls.load(std::sync::atomic::Ordering::SeqCst) > baseline);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn read_path_heap_merge_tombstone() {
        let store = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(ObjectPath::from("oxkv2"))
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

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
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

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
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

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn wal_gc_pinned_reader_holds_log() {
        let store = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(ObjectPath::from("gc-test"))
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
                    &ObjectPath::from("gc-test"),
                    s3.epoch(),
                    std::time::Duration::from_secs(0),
                )
                .await
                .unwrap()
        };
        assert!(!manifest_held.wal.is_empty(), "WAL retained while pinned");
        // WAL objects still exist.
        for wal in &manifest_held.wal {
            let p = ObjectPath::from(wal.clone());
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
            let p = ObjectPath::from(wal.clone());
            let err = store
                .get(&p)
                .await
                .expect_err("WAL must be deleted after GC");
            assert!(
                err.to_string().contains("not found"),
                "WAL {wal} must be deleted after GC: {err}"
            );
        }
        let (manifest_gc, _) = {
            let mut cache = s3.manifest_cache.lock().await;
            cache
                .load(
                    Arc::clone(&store),
                    &ObjectPath::from("gc-test"),
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

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn compaction_l0_to_l1_idempotent() {
        let store = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(ObjectPath::from("compact-test"))
            .with_session("compact-sess")
            .build()
            .await
            .unwrap();
        // Create 4 L0 SSTs to trigger compaction (>=4).
        for i in 0..4 {
            s3.stage_set(&format!("k{i:02}"), format!("v{i}").as_bytes())
                .await;
            // Mix deletes and overwrites to exercise tombstone handling.
            if i == 1 {
                s3.stage_set("common", b"1").await;
            }
            if i == 2 {
                s3.stage_set("common", b"2").await;
            }
            s3.flush_mem_to_sst_force().await.unwrap();
        }
        let (m_before, _) = {
            let mut cache = s3.manifest_cache.lock().await;
            cache
                .load(
                    Arc::clone(&store),
                    &ObjectPath::from("compact-test"),
                    s3.epoch(),
                    std::time::Duration::from_secs(0),
                )
                .await
                .unwrap()
        };
        let l0_before = m_before.sst.iter().filter(|m| m.level == 0).count();
        assert!(l0_before >= 4, "need >=4 L0 for trigger, got {l0_before}");
        // First compaction should produce L1.
        let new_l1 = s3.compact().await.unwrap().expect("should compact");
        assert_eq!(new_l1.level, 1);
        // Verify manifest now has no L0 (or fewer) and one L1.
        let (m_after, _) = {
            let mut cache = s3.manifest_cache.lock().await;
            cache
                .load(
                    Arc::clone(&store),
                    &ObjectPath::from("compact-test"),
                    s3.epoch(),
                    std::time::Duration::from_secs(0),
                )
                .await
                .unwrap()
        };
        assert_eq!(
            m_after.sst.iter().filter(|m| m.level == 0).count(),
            0,
            "L0 should be cleared"
        );
        assert!(
            m_after
                .sst
                .iter()
                .any(|m| m.level == 1 && m.id == new_l1.id),
            "new L1 must be present"
        );
        // Old L0 objects must be deleted.
        for m in m_before.sst.iter().filter(|m| m.level == 0) {
            let p = ObjectPath::from(m.id.clone());
            let err = store.get(&p).await.expect_err("old L0 must be deleted");
            assert!(
                err.to_string().contains("not found"),
                "old L0 {} must be deleted: {err}",
                m.id
            );
        }
        // Data still readable after compaction (newest wins).
        assert_eq!(
            s3.get_bytes("k00").await.unwrap().as_deref(),
            Some(b"v0".as_slice())
        );
        assert_eq!(
            s3.get_bytes("common").await.unwrap().as_deref(),
            Some(b"2".as_slice())
        );
        // Second compaction should be no-op (idempotent, no extra L0).
        let second = s3.compact().await.unwrap();
        assert!(second.is_none(), "second compact should be no-op");
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn s3store_store_trait_harness() {
        use crate::store::{GetSet, Store, Transaction};
        let store = new_in_memory();
        let mut s3 = OxKvStore::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(ObjectPath::from("store-harness"))
            .with_session("harness-sess")
            .build()
            .await
            .unwrap();
        // Store::set/get/delete direct (persistent)
        assert_eq!(s3.set_bytes("k1", b"v1").await.unwrap(), None);
        assert_eq!(
            s3.get_bytes("k1").await.unwrap().as_deref(),
            Some(b"v1".as_slice())
        );
        assert!(s3.has("k1").await.unwrap());
        assert_eq!(
            s3.set_bytes("k1", b"v2").await.unwrap().as_deref(),
            Some(b"v1".as_slice())
        );
        assert_eq!(
            s3.get_bytes("k1").await.unwrap().as_deref(),
            Some(b"v2".as_slice())
        );
        assert!(s3.delete("k1").await.unwrap());
        assert!(!s3.has("k1").await.unwrap());
        assert!(!s3.delete("k1").await.unwrap());
        // Transaction is staged until commit
        let mut tx = s3.begin_tx().unwrap();
        tx.set_bytes("tx-k", b"tx-v").await.unwrap();
        assert_eq!(
            tx.get_bytes("tx-k").await.unwrap().as_deref(),
            Some(b"tx-v".as_slice())
        );
        // not visible outside before commit
        assert_eq!(s3.get_bytes("tx-k").await.unwrap(), None);
        tx.commit().await.unwrap();
        assert_eq!(
            s3.get_bytes("tx-k").await.unwrap().as_deref(),
            Some(b"tx-v".as_slice())
        );
        // gets with limits/directions still works via heap-merge
        s3.set_bytes("a", b"1").await.unwrap();
        s3.set_bytes("b", b"2").await.unwrap();
        s3.set_bytes("c", b"3").await.unwrap();
        let got = s3
            .gets_bytes(Some(2), crate::store::Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].key, "a");
    }
}
