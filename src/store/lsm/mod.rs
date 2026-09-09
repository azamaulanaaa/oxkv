//! LSM store generic over the [`Storage`] trait — native and `wasm32`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::store::cache::{Cache, CacheStats, LruCache};
use crate::store::sleep;
use crate::store::storage::{ObjectPath, PutMode, Storage};

#[cfg(test)]
use crate::store::storage::MemStorage;

use crate::store::{
    Direction, GetSet, KeyValue, Result, Store, StoreError, Transaction, lock_ignore_poison,
};

mod blob;
mod manifest;
mod ownership;
mod probe;
mod reader;
mod sst;

pub(crate) use blob::{
    encode_blob_pointer, get_blob, is_overflow, put_blob, try_decode_blob_pointer,
};
pub(crate) use manifest::{Manifest, ManifestCache, SstMeta, cas_manifest, load_manifest};
pub(crate) use ownership::{acquire_ownership, cas_backoff, read_ownership, sst_path, wal_path};
pub(crate) use probe::probe_store;
pub use reader::{OxKvReader, OxKvRoTx};
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

#[must_use]
fn is_not_found(err: &StoreError) -> bool {
    matches!(err, StoreError::Storage(msg) if msg.contains("not found"))
}

/// WAL entries that trigger force-flush + GC maintenance.
///
/// Each write CAS-appends one WAL id to `manifest.json`; without a bound the
/// manifest grows without limit on small-write workloads (the 32 MiB SST
/// threshold never fires), making every write pay O(list) scan + serde.
/// Crossing this count force-flushes an SST and GCs covered WALs, keeping
/// per-write manifest cost flat.
const WAL_MAINTENANCE_COUNT: usize = 200;

/// L1 files that trigger a bounding compaction merging the smallest
/// adjacent pair (keeps the SST list — and every read's scan — short).
const L1_MERGE_COUNT: usize = 16;

/// Maximum single-key writes fused into one group-commit batch.
///
/// Bounds the WAL file a leader assembles and the time followers wait:
/// overflow stays queued for the next gate holder instead of stalling
/// behind one giant batch.
const MAX_GROUP_WRITES: usize = 256;

/// One staged blind write waiting for (or riding) a group commit.
///
/// Payload is encoded once at staging, so a failure there fails only its
/// own write; the leader concatenates `encoded` buffers verbatim into a
/// single WAL file that replays with no format changes.
struct PendingWrite {
    key: String,
    value: Vec<u8>,
    encoded: Vec<u8>,
    responder: futures::channel::oneshot::Sender<Result<()>>,
}

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
    /// Fair gate serializing the durable write paths (`put_bytes` and tx
    /// `commit`): concurrent writers queue here instead of CAS-retry-storming
    /// the manifest. Always acquired outermost, never while holding
    /// `manifest_cache`, so lock ordering stays acyclic.
    write_gate: Arc<async_lock::Mutex<()>>,
    /// Staged blind writes awaiting group commit. Whoever acquires
    /// `write_gate` drains the queue (capped by [`MAX_GROUP_WRITES`]) into
    /// one WAL file plus one manifest CAS; `delete` and tx commits take the
    /// gate without staging and ride it exclusively.
    pending: Arc<std::sync::Mutex<Vec<PendingWrite>>>,
    /// SST file cache - scan-resistant `S3-FIFO` (~256 MB with 32KB blocks).
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

    /// Returns SST-cache hit/miss statistics, or `None` for cache backends
    /// that do not track them (e.g. `moka`).
    #[must_use]
    pub fn sst_cache_stats(&self) -> Option<CacheStats> {
        self.sst_cache.stats()
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
            Ok(_) | Err(StoreError::CasConflict(_)) => {}
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
            let (manifest, etag) = load_manifest(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                &self.manifest_cache,
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
                    self.manifest_cache.lock().await.update(manifest, new_etag);
                    self.maintain_wal(wal_len).await;
                    return Ok(());
                }
                Err(StoreError::CasConflict(detail)) => {
                    self.manifest_cache.lock().await.clear();
                    if attempt == 3 {
                        return Err(StoreError::Storage(format!(
                            "wal manifest CAS conflict after retries: {detail}"
                        )));
                    }
                    let backoff = cas_backoff(attempt);
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
            Ok(_) | Err(StoreError::CasConflict(_)) => {}
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

        let (manifest, etag) = load_manifest(
            Arc::clone(&self.inner),
            &self.prefix,
            self.epoch,
            &self.manifest_cache,
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
                self.manifest_cache.lock().await.update(manifest, new_etag);
                {
                    let mut mem = self.mem.write().await;
                    for key in snapshot.keys() {
                        mem.remove(key);
                    }
                }
                Ok(Some(sst_meta))
            }
            Err(StoreError::CasConflict(detail)) => {
                let backoff = cas_backoff(0);
                sleep(backoff).await;
                self.manifest_cache.lock().await.clear();
                let (reloaded, _) = load_manifest(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    &self.manifest_cache,
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
                    "manifest CAS conflict after backoff retry: {detail}"
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
    /// Restarts once against a fresh manifest when an SST or blob read hits
    /// `not found`: the snapshot may predate a compaction that deleted the file.
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
            let mem = self.mem.read().await;
            if let Some(val) = mem.get(key) {
                match val {
                    Some(v) => return Ok(Some(self.resolve_value(v.clone()).await?)),
                    None => return Ok(None),
                }
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

    /// Range scan merging `MemTable` + SSTs with tombstone suppression and blob deref.
    ///
    /// Restarts once against a fresh manifest when an SST or blob read hits `not found`.
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
        // Normalize cursor to [lower, upper] for range filtering; Prev stores
        // upper in cursor.0 and lower in cursor.1.
        let (scan_start, scan_end) = match direction {
            Direction::Next => (cursor.0.as_deref(), cursor.1.as_deref()),
            Direction::Prev => (cursor.1.as_deref(), cursor.0.as_deref()),
        };
        if direction == Direction::Prev && cursor.0.is_none() {
            return Ok(Vec::new());
        }
        let mem_rows: Vec<(String, Option<Vec<u8>>)> = {
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
            mem_vec
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
                // Admit scans to the SST cache: rotating pages reuse the same
                // files, and S3-FIFO absorbs one-hit entries without evicting
                // hot point lookups (zipf_get guards the ratio).
                files.push(self.fetch_sst(&meta.id).await?);
            }
            let mut pull: Vec<MergeSource<'_>> = Vec::with_capacity(files.len() + 1);
            pull.push(MergeSource::Mem(mem_rows.into_iter()));
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
        let mut sources = vec![mem_rows];
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
            let (manifest, etag) = load_manifest(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                &self.manifest_cache,
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
                    self.manifest_cache.lock().await.update(manifest, new_etag);
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
                Err(StoreError::CasConflict(_)) => {
                    self.manifest_cache.lock().await.clear();
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
        let (manifest_snapshot, _) = load_manifest(
            Arc::clone(&self.inner),
            &self.prefix,
            self.epoch,
            &self.manifest_cache,
            std::time::Duration::from_secs(1),
        )
        .await?;
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
                let (manifest, etag) = load_manifest(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    &self.manifest_cache,
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
                        self.manifest_cache
                            .lock()
                            .await
                            .update(manifest.clone(), new_etag);
                        for m in l0_metas.iter().chain(l1_overlapping.iter()) {
                            let p = ObjectPath::from(m.id.as_str());
                            let _ = self.inner.delete(&p).await;
                        }
                        return Ok(None);
                    }
                    Err(StoreError::CasConflict(_)) => {
                        self.manifest_cache.lock().await.clear();
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
            Ok(_) | Err(StoreError::CasConflict(_)) => {}
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
            let (manifest, etag) = load_manifest(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                &self.manifest_cache,
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
                    self.manifest_cache.lock().await.update(manifest, new_etag);
                    // Invalidate sst_cache for deleted, keep new.
                    for m in l0_metas.iter().chain(l1_overlapping.iter()) {
                        self.sst_cache.remove(&m.id).await;
                        let p = ObjectPath::from(m.id.as_str());
                        let _ = self.inner.delete(&p).await;
                    }
                    return Ok(Some(new_meta));
                }
                Err(StoreError::CasConflict(_)) => {
                    self.manifest_cache.lock().await.clear();
                    sleep(cas_backoff(0)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Persists one group-commit batch: concatenated records in a single WAL
    /// file, one ownership check, one manifest CAS append, one `MemTable`
    /// application, then the usual maintenance. Shared fate: every entry
    /// succeeds or fails together, exactly as sequential `put_bytes` calls
    /// would if the process crashed between them.
    async fn persist_batch(&self, batch: &[PendingWrite]) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        // Atomic: flush the whole batch before mutating MemTable
        let mut payload_buf = Vec::new();
        for entry in batch {
            payload_buf.extend_from_slice(&entry.encoded);
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
            Ok(_) | Err(StoreError::CasConflict(_)) => {}
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
            let (manifest, etag) = load_manifest(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                &self.manifest_cache,
                std::time::Duration::from_secs(1),
            )
            .await?;
            // Owned copy for mutation; readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
            if manifest.wal.iter().any(|w| w == &wal_id) {
                {
                    let mut mem = self.mem.write().await;
                    for entry in batch {
                        mem.insert(entry.key.clone(), Some(entry.value.clone()));
                    }
                }
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
                    self.manifest_cache.lock().await.update(manifest, new_etag);
                    {
                        let mut mem = self.mem.write().await;
                        for entry in batch {
                            mem.insert(entry.key.clone(), Some(entry.value.clone()));
                        }
                    }
                    let _ = self.flush_mem_to_sst().await;
                    let _ = self.compact().await;
                    self.maintain_wal(wal_len).await;
                    return Ok(());
                }
                Err(StoreError::CasConflict(detail)) => {
                    self.manifest_cache.lock().await.clear();
                    if attempt == 3 {
                        return Err(StoreError::Storage(format!(
                            "wal manifest CAS conflict after retries: {detail}"
                        )));
                    }
                    let backoff = cas_backoff(attempt);
                    sleep(backoff).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
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
    /// Fair gate serializing the durable write paths (`put_bytes` and tx
    /// `commit`): concurrent writers queue here instead of CAS-retry-storming
    /// the manifest. Always acquired outermost, never while holding
    /// `manifest_cache`, so lock ordering stays acyclic.
    write_gate: Arc<async_lock::Mutex<()>>,
    /// Group-commit queue shared with the parent store: blind writes staged
    /// anywhere drain through whoever holds `write_gate`.
    pending: Arc<std::sync::Mutex<Vec<PendingWrite>>>,
    sst_cache: C,
    overlay: std::sync::Mutex<std::collections::BTreeMap<String, Option<Vec<u8>>>>,
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

    async fn point_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let staged = lock_ignore_poison(&self.overlay).get(key).cloned();
        if let Some(v) = staged {
            return match v {
                Some(raw) => Ok(Some(self.resolve_value(raw).await?)),
                None => Ok(None),
            };
        }
        {
            let mem = self.mem.read().await;
            if let Some(v) = mem.get(key) {
                return match v {
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
        let overlay_vec: Vec<(String, Option<Vec<u8>>)> = {
            let overlay_guard = lock_ignore_poison(&self.overlay);
            if scan_start.is_none() && scan_end.is_none() {
                overlay_guard
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            } else {
                overlay_guard
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
            }
        };
        let mem_rows: Vec<(String, Option<Vec<u8>>)> = {
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
            mem_vec
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
            let mut pull: Vec<MergeSource<'_>> = Vec::with_capacity(files.len() + 2);
            pull.push(MergeSource::Mem(overlay_vec.into_iter()));
            pull.push(MergeSource::Mem(mem_rows.into_iter()));
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
        let mut sources = vec![overlay_vec, mem_rows];
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
            write_gate: Arc::clone(&self.write_gate),
            pending: Arc::clone(&self.pending),
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

    async fn delete(&self, key: &str) -> Result<bool> {
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
            Ok(_) | Err(StoreError::CasConflict(_)) => {}
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
            let (manifest, etag) = load_manifest(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                &self.manifest_cache,
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
                    self.manifest_cache.lock().await.update(manifest, new_etag);
                    self.mem.write().await.insert(key.to_string(), None);
                    // Best-effort auto maintenance — ignore fencing/conflicts
                    let _ = self.flush_mem_to_sst().await;
                    let _ = self.compact().await;
                    self.maintain_wal(wal_len).await;
                    return Ok(true);
                }
                Err(StoreError::CasConflict(detail)) => {
                    self.manifest_cache.lock().await.clear();
                    if attempt == 3 {
                        return Err(StoreError::Storage(format!(
                            "wal manifest CAS conflict after retries: {detail}"
                        )));
                    }
                    let backoff = cas_backoff(attempt);
                    sleep(backoff).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = OxKvStore::get_bytes(self, key).await?;
        self.put_bytes(key, value).await?;
        Ok(prev)
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        // Stage into the group-commit queue with encode-once semantics: a
        // failure here fails only this write, before any shared state moves.
        let mut encoded = Vec::new();
        crate::store::encode_record(&mut encoded, key, value)
            .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?;
        let (responder, mut waiter) = futures::channel::oneshot::channel::<Result<()>>();
        lock_ignore_poison(&self.pending).push(PendingWrite {
            key: key.to_string(),
            value: value.to_vec(),
            encoded,
            responder,
        });
        let _gate = self.write_gate.lock().await;
        // Served while queued behind the gate: return the recorded outcome.
        // `Closed` means the serving leader vanished mid-batch; the write's
        // fate is unknowable, so report it instead of silently dropping it.
        match waiter.try_recv() {
            Ok(Some(result)) => return result,
            Ok(None) => {}
            Err(futures::channel::oneshot::Canceled) => {
                return Err(StoreError::Storage(
                    "group commit leader lost; write fate unknown — read back to confirm"
                        .to_string(),
                ));
            }
        }
        // Leader: drain everything pending (at least our own entry) into one
        // WAL file plus one manifest CAS; overflow rides the next holder.
        let batch: Vec<PendingWrite> = {
            let mut pending = lock_ignore_poison(&self.pending);
            let take = pending.len().min(MAX_GROUP_WRITES);
            pending.drain(..take).collect()
        };
        let result = self.persist_batch(&batch).await;
        for entry in batch {
            let _ = entry.responder.send(result.clone());
        }
        result
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
#[allow(clippy::too_many_lines)]
impl<C> GetSet for OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self.point_get(key).await {
            Err(e) if is_not_found(&e) => {
                self.manifest_cache.lock().await.clear();
                self.point_get(key).await
            }
            other => other,
        }
    }

    async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get_bytes(key).await?.is_some())
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        let existed = self.get_bytes(key).await?.is_some();
        if existed {
            lock_ignore_poison(&self.overlay).insert(key.to_string(), None);
        }
        Ok(existed)
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = self.get_bytes(key).await?;
        lock_ignore_poison(&self.overlay).insert(key.to_string(), Some(value.to_vec()));
        Ok(prev)
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        lock_ignore_poison(&self.overlay).insert(key.to_string(), Some(value.to_vec()));
        Ok(())
    }

    async fn gets_bytes(
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
}

#[async_trait]
impl<C> Transaction for OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    #[allow(clippy::too_many_lines)]
    async fn commit(self) -> Result<()> {
        // Serialize concurrent writers; see `write_gate`.
        let _gate = self.write_gate.lock().await;
        // Drain under the lock: `commit` consumes the transaction, so taking
        // ownership up front is equivalent and keeps no guard across awaits.
        let overlay = std::mem::take(&mut *lock_ignore_poison(&self.overlay));
        if overlay.is_empty() {
            return Ok(());
        }
        // Encode directly from overlay — don't mutate shared mem until WAL is durable (atomic)
        let mut payload_buf = Vec::new();
        for (key, value) in &overlay {
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
            Ok(_) | Err(StoreError::CasConflict(_)) => {}
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
            let (manifest, etag) = load_manifest(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                &self.manifest_cache,
                std::time::Duration::from_secs(1),
            )
            .await?;
            // Owned copy for mutation; readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
            if manifest.wal.iter().any(|w| w == &wal_id) {
                // Idempotent retry — WAL already durable, apply overlay to MemTable
                {
                    let mut mem = self.mem.write().await;
                    for (k, v) in overlay.clone() {
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
                    self.manifest_cache.lock().await.update(manifest, new_etag);
                    // Now durable — apply the drained overlay to MemTable.
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
                Err(StoreError::CasConflict(detail)) => {
                    self.manifest_cache.lock().await.clear();
                    if attempt == 3 {
                        return Err(StoreError::Storage(format!(
                            "wal manifest CAS conflict after retries: {detail}"
                        )));
                    }
                    let backoff = cas_backoff(attempt);
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

    fn begin_tx(&self) -> Result<Self::Transaction> {
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
            write_gate: Arc::clone(&self.write_gate),
            pending: Arc::clone(&self.pending),
            sst_cache: self.sst_cache.clone(),
            overlay: std::sync::Mutex::new(std::collections::BTreeMap::new()),
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

    /// Builds a read-only view over the prefix without acquiring ownership.
    ///
    /// Skips the storage probe and the `ownership.json` epoch CAS, so opening
    /// a reader never fences the current writer. `with_session` and
    /// `assume_single_writer` are ignored; the manifest is always revalidated
    /// by TTL.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` when no [`Storage`] was configured.
    pub async fn build_reader(self) -> Result<OxKvReader> {
        let store = self.inner.ok_or_else(|| {
            StoreError::Storage("OxKvStore requires a Storage via with_store()".to_string())
        })?;
        OxKvReader::open(store, self.prefix).await
    }

    /// Builds a read-only view with a caller-supplied SST cache.
    ///
    /// See [`Self::build_reader`] for the ownership and probe semantics.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` when no [`Storage`] was configured.
    pub async fn build_reader_with_cache<C>(self, cache: C) -> Result<OxKvReader<C>>
    where
        C: Cache<String, Arc<SstFile>>,
    {
        let store = self.inner.ok_or_else(|| {
            StoreError::Storage("OxKvStore requires a Storage via with_store()".to_string())
        })?;
        OxKvReader::open_with_cache(store, self.prefix, cache).await
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
    /// ```rust
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// // Live only under `--all-features`: doctests cannot be `cfg`d out
    /// // wholesale (stripping `main` breaks the build), so the `moka`
    /// // setup hides inside a feature gate and this compiles to an empty
    /// // `main` without it.
    /// # #[cfg(all(feature = "oxkv", feature = "moka"))]
    /// # {
    /// # use std::sync::Arc;
    /// # use oxkv::{MemStorage, OxKvStore, SstFile};
    /// let cache = moka::future::Cache::builder()
    ///     .max_capacity(256 * 1024 * 1024)
    ///     .weigher(|_: &String, v: &Arc<SstFile>| u32::try_from(v.size()).unwrap_or(u32::MAX))
    ///     .build();
    /// let store = OxKvStore::builder()
    ///     .with_store(Arc::new(MemStorage::new()))
    ///     .build_with_cache(cache)
    ///     .await
    ///     .unwrap();
    /// # }
    /// # }
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
            write_gate: Arc::new(async_lock::Mutex::new(())),
            pending: Arc::new(std::sync::Mutex::new(Vec::new())),
            sst_cache: cache,
        };
        // WAL replay for restart/f fencing — make not-yet-SSTed WAL visible
        {
            let (manifest, _etag) = match load_manifest(
                Arc::clone(&s3store.inner),
                &s3store.prefix,
                s3store.epoch,
                &s3store.manifest_cache,
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

/// One pull source for [`pull_merge_next`]: owned mem rows or a borrowing
/// file scan. File scans borrow their SST, so sources never outlive the
/// handle vector built alongside them in `gets_bytes`.
enum MergeSource<'a> {
    Mem(std::vec::IntoIter<(String, Option<Vec<u8>>)>),
    File(sst::SstScan<'a>),
}

impl Iterator for MergeSource<'_> {
    type Item = Result<(String, Option<Vec<u8>>)>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Mem(it) => it.next().map(Ok),
            Self::File(it) => it.next(),
        }
    }
}

/// Heap entry for [`pull_merge_next`], ordered by ascending key with source
/// rank breaking ties: smaller rank is newer, so the first pop of a key is
/// its newest entry and decides it. `BinaryHeap` is a max-heap, hence the
/// reversed comparison.
struct MergeHeapEntry {
    key: String,
    rank: usize,
    value: Option<Vec<u8>>,
    source: usize,
}

impl PartialEq for MergeHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.rank == other.rank
    }
}

impl Eq for MergeHeapEntry {}

impl PartialOrd for MergeHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MergeHeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .key
            .cmp(&self.key)
            .then_with(|| other.rank.cmp(&self.rank))
    }
}

/// K-way newest-first merge over sorted per-source iterators, ascending.
///
/// Sources must yield range-filtered `(key, raw)` pairs with `None` marking
/// tombstones, ordered newest-first (rank is the source position). Pulls the
/// globally smallest undecided key: its newest entry decides it, tombstones
/// suppress older duplicates without yielding, and iteration stops after
/// `limit` live keys (`None` drains). Exact: entries pull in global order,
/// so stopping early returns precisely what an uncapped merge-then-truncate
/// would, without decoding the unread tail.
fn pull_merge_next(
    sources: &mut [MergeSource<'_>],
    limit: Option<usize>,
) -> Result<Vec<(String, Vec<u8>)>> {
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let mut heap = std::collections::BinaryHeap::new();
    for (rank, source) in sources.iter_mut().enumerate() {
        if let Some(head) = source.next() {
            let (key, value) = head?;
            heap.push(MergeHeapEntry {
                key,
                rank,
                value,
                source: rank,
            });
        }
    }
    let mut out = Vec::new();
    while let Some(entry) = heap.pop() {
        while let Some(top) = heap.peek() {
            if top.key != entry.key {
                break;
            }
            if let Some(dup) = heap.pop()
                && let Some(next) = sources[dup.source].next()
            {
                let (key, value) = next?;
                heap.push(MergeHeapEntry {
                    key,
                    rank: dup.rank,
                    value,
                    source: dup.source,
                });
            }
        }
        if let Some(next) = sources[entry.source].next() {
            let (key, value) = next?;
            heap.push(MergeHeapEntry {
                key,
                rank: entry.rank,
                value,
                source: entry.source,
            });
        }
        if let Some(value) = entry.value {
            out.push((entry.key, value));
            if limit.is_some_and(|lim| out.len() >= lim) {
                break;
            }
        }
    }
    Ok(out)
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

    struct FlakySst {
        inner: MemStorage,
        fail_once: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl Storage for FlakySst {
        async fn get(&self, path: &ObjectPath) -> Result<GetOutput> {
            let is_sst = std::path::Path::new(path.as_str())
                .extension()
                .is_some_and(|ext| ext == "sst");
            if is_sst
                && self
                    .fail_once
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StoreError::Storage("not found: injected".to_string()));
            }
            self.inner.get(path).await
        }

        async fn get_opts(&self, path: &ObjectPath, options: GetOptions) -> Result<GetOutput> {
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

    async fn flaky_writer() -> OxKvStore {
        let backend: Arc<dyn Storage> = Arc::new(FlakySst {
            inner: MemStorage::new(),
            fail_once: std::sync::atomic::AtomicBool::new(true),
        });
        let store = OxKvStore::builder()
            .with_store(backend)
            .skip_probe(true)
            .build()
            .await
            .expect("build");
        store.put_bytes("k", b"v").await.expect("put");
        store
            .flush_mem_to_sst_force()
            .await
            .expect("flush")
            .expect("sst");
        store
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn point_get_retries_sst_not_found() {
        let store = flaky_writer().await;
        assert_eq!(
            store.get_bytes("k").await.expect("get"),
            Some(b"v".to_vec())
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn scan_prev_retries_sst_not_found() {
        let store = flaky_writer().await;
        let rows = store
            .gets_bytes(Some(10), Direction::Prev, (Some("z".to_string()), None))
            .await
            .expect("scan");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "k");
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn tx_point_get_retries_sst_not_found() {
        let store = flaky_writer().await;
        let tx = store.begin_tx().expect("tx");
        assert_eq!(tx.get_bytes("k").await.expect("get"), Some(b"v".to_vec()));
        tx.rollback().await.expect("rollback");
    }

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
            matches!(err, StoreError::CasConflict(_)),
            "unexpected error: {err:?}"
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

    /// Concurrent tasks share one store on a multi-threaded runtime: blind
    /// writes, point reads, and per-task transaction commits interleave
    /// across threads. Distinct values per key catch cross-talk; the final
    /// sweep asserts every write landed exactly once.
    // Threaded stress: wasm32-unknown-unknown is single-threaded and the
    // multi-thread scheduler feature does not compile there (see Cargo.toml).
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers_and_readers_share_store() {
        let kv = Arc::new(
            OxKvStore::builder()
                .with_store(new_in_memory())
                .with_prefix(ObjectPath::from("oxkv-concurrent"))
                .build()
                .await
                .expect("build"),
        );
        let mut handles = Vec::new();
        for t in 0..8u32 {
            let kv = Arc::clone(&kv);
            handles.push(tokio::spawn(async move {
                for i in 0..25u32 {
                    let k = format!("t{t}-k{i}");
                    kv.put_bytes(&k, k.as_bytes()).await.expect("shared write");
                    let got = kv
                        .get_bytes(&k)
                        .await
                        .expect("shared read")
                        .expect("present");
                    assert_eq!(got, k.as_bytes());
                }
                let tx = kv.begin_tx().expect("begin_tx");
                let k = format!("t{t}-tx");
                tx.put_bytes(&k, k.as_bytes()).await.expect("stage");
                tx.commit().await.expect("commit");
            }));
        }
        for handle in handles {
            handle.await.expect("task");
        }
        for t in 0..8u32 {
            for i in 0..25u32 {
                let k = format!("t{t}-k{i}");
                let got = kv
                    .get_bytes(&k)
                    .await
                    .expect("sweep read")
                    .expect("present");
                assert_eq!(got, k.as_bytes());
            }
            let k = format!("t{t}-tx");
            let got = kv
                .get_bytes(&k)
                .await
                .expect("sweep tx read")
                .expect("present");
            assert_eq!(got, k.as_bytes());
        }
    }

    /// Concurrent writes to one key never lose updates: every task reports
    /// success and the final value is one of the written ones.
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn group_commit_same_key_hammer() {
        let kv = Arc::new(
            OxKvStore::builder()
                .with_store(new_in_memory())
                .with_prefix(ObjectPath::from("oxkv-group-same-key"))
                .build()
                .await
                .expect("build"),
        );
        let mut handles = Vec::new();
        for t in 0..8u32 {
            let kv = Arc::clone(&kv);
            handles.push(tokio::spawn(async move {
                for i in 0..20u32 {
                    let v = format!("t{t}-v{i}");
                    kv.put_bytes("hot-key", v.as_bytes()).await.expect("write");
                }
            }));
        }
        for handle in handles {
            handle.await.expect("task");
        }
        let final_value = kv
            .get_bytes("hot-key")
            .await
            .expect("read")
            .expect("present");
        let final_str = String::from_utf8(final_value).expect("utf8");
        let (t, i) = final_str
            .strip_prefix('t')
            .and_then(|rest| rest.split_once("-v"))
            .map(|(t, i)| (t.parse::<u32>(), i.parse::<u32>()))
            .expect("shape");
        assert!(t.is_ok() && t.unwrap() < 8 && i.is_ok() && i.unwrap() < 20);
    }

    /// Barrier-released writers fuse into fewer WAL files than operations:
    /// at least one batch must carry two or more records.
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn group_commit_batches_concurrent_writes() {
        use std::sync::Barrier;
        const TASKS: u32 = 8;
        const PER_TASK: u32 = 25;
        let inner = new_in_memory();
        let kv = Arc::new(
            OxKvStore::builder()
                .with_store(Arc::clone(&inner))
                .with_prefix(ObjectPath::from("oxkv-group-batch"))
                .build()
                .await
                .expect("build"),
        );
        let barrier = Arc::new(Barrier::new(TASKS as usize));
        let mut handles = Vec::new();
        for t in 0..TASKS {
            let kv = Arc::clone(&kv);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait();
                for i in 0..PER_TASK {
                    let k = format!("t{t}-k{i}");
                    kv.put_bytes(&k, k.as_bytes()).await.expect("write");
                }
            }));
        }
        for handle in handles {
            handle.await.expect("task");
        }
        let (manifest, _) = load_manifest(
            Arc::clone(&inner),
            &ObjectPath::from("oxkv-group-batch"),
            kv.epoch(),
            &kv.manifest_cache,
            std::time::Duration::from_secs(0),
        )
        .await
        .expect("manifest");
        let wal_files = manifest.wal.len();
        assert!(
            wal_files < (TASKS * PER_TASK) as usize,
            "expected grouping, got one WAL file per op: {wal_files}"
        );
        let got = kv.get_bytes("t3-k7").await.expect("read").expect("present");
        assert_eq!(got, b"t3-k7");
    }

    /// Dropped stores rebuild from multi-record WAL files: concurrent writes
    /// replay exactly, proving batched WAL needs no format changes.
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn group_commit_wal_replay_after_rebuild() {
        let inner = new_in_memory();
        let prefix = ObjectPath::from("oxkv-group-rebuild");
        let kv = Arc::new(
            OxKvStore::builder()
                .with_store(Arc::clone(&inner))
                .with_prefix(prefix.clone())
                .with_session("sess-rebuild-1")
                .build()
                .await
                .expect("build"),
        );
        let mut handles = Vec::new();
        for t in 0..4u32 {
            let kv = Arc::clone(&kv);
            handles.push(tokio::spawn(async move {
                for i in 0..10u32 {
                    let k = format!("t{t}-k{i}");
                    kv.put_bytes(&k, k.as_bytes()).await.expect("write");
                }
            }));
        }
        for handle in handles {
            handle.await.expect("task");
        }
        drop(kv);
        let rebuilt = OxKvStore::builder()
            .with_store(inner)
            .with_prefix(prefix)
            .with_session("sess-rebuild-2")
            .build()
            .await
            .expect("rebuild");
        for t in 0..4u32 {
            for i in 0..10u32 {
                let k = format!("t{t}-k{i}");
                let got = rebuilt.get_bytes(&k).await.expect("read").expect("present");
                assert_eq!(got, k.as_bytes());
            }
        }
    }

    /// `pull_merge_next` agrees with the materialized merge on overlapping
    /// sources with tombstones, at every limit including the truncation edge
    /// and full drain. Guards the heap ordering, newest-wins dedup, and
    /// early-stop exactness of lazy page scans.
    #[test]
    fn pull_merge_matches_materialized() {
        use std::collections::BTreeMap;
        let opt = |v: &str| Some(v.as_bytes().to_vec());
        let old: BTreeMap<String, Option<Vec<u8>>> = [
            ("a", opt("old-a")),
            ("b", opt("old-b")),
            ("c", opt("old-c")),
            ("d", opt("old-d")),
            ("e", opt("old-e")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let new: BTreeMap<String, Option<Vec<u8>>> =
            [("c", opt("new-c")), ("d", None), ("f", opt("new-f"))]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
        let mem: Vec<(String, Option<Vec<u8>>)> =
            vec![("a".to_string(), None), ("b".to_string(), opt("mem-b"))];
        let old_file =
            SstFile::parse(sst::build_sst(&old, 64).expect("old sst")).expect("parse old");
        let new_file =
            SstFile::parse(sst::build_sst(&new, 64).expect("new sst")).expect("parse new");
        let materialized = |limit: Option<usize>| {
            let merged = merge_sources(vec![
                mem.clone(),
                new_file
                    .scan_with_tombstones(None, None, None)
                    .expect("new scan"),
                old_file
                    .scan_with_tombstones(None, None, None)
                    .expect("old scan"),
            ]);
            let live: Vec<(String, Vec<u8>)> =
                merged.into_iter().map(|kv| (kv.key, kv.value)).collect();
            match limit {
                Some(lim) => live.into_iter().take(lim).collect(),
                None => live,
            }
        };
        for limit in [None, Some(0), Some(1), Some(2), Some(3), Some(10)] {
            let mut sources = vec![
                MergeSource::Mem(mem.clone().into_iter()),
                MergeSource::File(new_file.scan_iter(None, None)),
                MergeSource::File(old_file.scan_iter(None, None)),
            ];
            let pulled = pull_merge_next(&mut sources, limit).expect("pull");
            assert_eq!(pulled, materialized(limit), "limit {limit:?}");
        }
    }

    /// Newest-wins across flush/compact cycles: a newer L0 outranks older
    /// L1s, and the manifest re-sort inside `compact` preserves that because
    /// `compact` drains every L0 it merges while `flush` appends new L0s
    /// after the sorted L1s, keeping reverse manifest order newest-first.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn newest_wins_across_flush_compact() {
        let inner = new_in_memory();
        let s = OxKvStore::builder()
            .with_store(Arc::clone(&inner))
            .with_prefix(ObjectPath::from("oxkv-resort"))
            .with_session("sess-resort")
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        let read_manifest = || async {
            let path = ObjectPath::from("oxkv-resort").child("manifest.json");
            let out = inner.get(&path).await.expect("manifest readable");
            serde_json::from_slice::<Manifest>(&out.bytes).expect("manifest parses")
        };
        // Four single-key flushes, then compact merges the L0s into one L1.
        for (k, v) in [("k1", "a"), ("k2", "b"), ("k3", "c"), ("k4", "d")] {
            s.put_bytes(k, v.as_bytes()).await.unwrap();
            s.flush_mem_to_sst_force().await.expect("flush");
        }
        s.compact().await.unwrap();
        // Old mango version rides one L0; new version plus a smaller key
        // rides the next, so min_key order and recency order disagree.
        s.put_bytes("mango", b"v1").await.unwrap();
        s.put_bytes("zebra", b"v1").await.unwrap();
        s.flush_mem_to_sst_force().await.expect("flush");
        s.put_bytes("apple", b"x").await.unwrap();
        s.put_bytes("mango", b"v2").await.unwrap();
        s.flush_mem_to_sst_force().await.expect("flush");
        // Unmerged L0s lose nothing: the newest L0 outranks older files.
        assert_eq!(
            s.get_bytes("mango").await.unwrap().as_deref(),
            Some(b"v2".as_slice())
        );
        // Top up L0s so the final compact must merge (and re-sort); each
        // iteration nets one L0 because auto-compacts only fire at four.
        loop {
            let manifest = read_manifest().await;
            if manifest.sst.iter().filter(|m| m.level == 0).count() >= 4 {
                break;
            }
            let pad = manifest.sst.len();
            s.put_bytes(&format!("pad{pad}"), b"p").await.unwrap();
            s.flush_mem_to_sst_force().await.expect("flush");
        }
        s.compact().await.unwrap();
        assert_eq!(
            s.get_bytes("mango").await.unwrap().as_deref(),
            Some(b"v2".as_slice())
        );
        assert_eq!(
            s.get_bytes("apple").await.unwrap().as_deref(),
            Some(b"x".as_slice())
        );
        assert_eq!(
            s.get_bytes("zebra").await.unwrap().as_deref(),
            Some(b"v1".as_slice())
        );
        assert_eq!(
            s.get_bytes("k1").await.unwrap().as_deref(),
            Some(b"a".as_slice())
        );
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
        let s3 = OxKvStore::builder()
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
        let s3 = OxKvStore::builder()
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
            // Tolerate `None`: WAL maintenance may have flushed this round's
            // mem mid-round once the WAL list hits its threshold.
            let _ = s3.flush_mem_to_sst_force().await.expect("sst flush");
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
        let s3 = OxKvStore::builder()
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
            let tx = s3.begin_tx().unwrap();
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
        let s3 = OxKvStore::builder()
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
        let tx = s3.begin_tx().unwrap();
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
        let s3 = builder.build().await.unwrap();
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
        let (manifest_held, _) = load_manifest(
            Arc::clone(&store),
            &ObjectPath::from("gc-test"),
            s3.epoch(),
            &s3.manifest_cache,
            std::time::Duration::from_secs(0),
        )
        .await
        .unwrap();
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
        let (manifest_gc, _) = load_manifest(
            Arc::clone(&store),
            &ObjectPath::from("gc-test"),
            s3.epoch(),
            &s3.manifest_cache,
            std::time::Duration::from_secs(0),
        )
        .await
        .unwrap();
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
        let (m_before, _) = load_manifest(
            Arc::clone(&store),
            &ObjectPath::from("compact-test"),
            s3.epoch(),
            &s3.manifest_cache,
            std::time::Duration::from_secs(0),
        )
        .await
        .unwrap();
        let l0_before = m_before.sst.iter().filter(|m| m.level == 0).count();
        assert!(l0_before >= 4, "need >=4 L0 for trigger, got {l0_before}");
        // First compaction should produce L1.
        let new_l1 = s3.compact().await.unwrap().expect("should compact");
        assert_eq!(new_l1.level, 1);
        // Verify manifest now has no L0 (or fewer) and one L1.
        let (m_after, _) = load_manifest(
            Arc::clone(&store),
            &ObjectPath::from("compact-test"),
            s3.epoch(),
            &s3.manifest_cache,
            std::time::Duration::from_secs(0),
        )
        .await
        .unwrap();
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
        let s3 = OxKvStore::builder()
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
        let tx = s3.begin_tx().unwrap();
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
