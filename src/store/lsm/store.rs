//! Single-writer LSM store: durable write paths over the shared engines.
//!
//! [`OxKvStore`] owns the writer state and serializes concurrent writers
//! behind a fair gate. Reads delegate to the shared lookup engine, WAL work
//! to the WAL module, and construction goes through [`OxKvStoreBuilder`].
//! Transactions convert through [`OxKvTx`](super::tx::OxKvTx).

use std::sync::Arc;

use async_trait::async_trait;

use super::blob::{encode_blob_pointer, get_blob, is_overflow, put_blob, try_decode_blob_pointer};
#[cfg(feature = "btree")]
use super::cached::CachedOxKvStore;
use super::manifest::{ManifestCache, SstMeta, cas_manifest, load_manifest};
use super::merge::merge_sources;
use super::ownership::{acquire_ownership, cas_backoff, read_ownership, sst_path, wal_path};
use super::probe::probe_store;
use super::read::{ReadCtx, filter_rows, is_not_found, point_lookup, range_lookup};
use super::reader::OxKvReader;
use super::sst::{DEFAULT_BLOCK_SIZE, SstFile, TOMBSTONE_VLEN, build_sst};
use super::tx::OxKvTx;
use super::wal::replay_listed_wals;
use super::{
    L1_MERGE_COUNT, MAX_GROUP_WRITES, MemTable, SESSION_CTR, WAL_MAINTENANCE_COUNT, WalBuffer,
};
use crate::store::cache::{Cache, CacheStats, LruCache};
use crate::store::storage::{ObjectPath, PutMode, Storage};
use crate::store::{
    Direction, GetSet, KeyValue, Result, Store, StoreError, lock_ignore_poison, sleep,
};

/// One staged blind write waiting for (or riding) a group commit.
///
/// Payload is encoded once at staging, so a failure there fails only its
/// own write; the leader concatenates `encoded` buffers verbatim into a
/// single WAL file that replays with no format changes.
pub(crate) struct PendingWrite {
    id: u64,
    key: String,
    value: Vec<u8>,
    encoded: Vec<u8>,
    responder: futures::channel::oneshot::Sender<Result<()>>,
}

/// Process-wide id for [`PendingWrite`], so a group-commit leader can tell
/// whether the batch it drained actually contained its own write.
static PENDING_CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Storage-backed LSM store (probe + fencing + WAL gate + SST).
pub struct OxKvStore<C = LruCache<String, Arc<SstFile>>> {
    pub(crate) inner: Arc<dyn Storage>,
    pub(crate) prefix: ObjectPath,
    pub(crate) epoch: u64,
    pub(crate) session: String,
    pub(crate) mem: MemTable,
    pub(crate) wal_seq: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) wal_buffer: WalBuffer,
    pub(crate) sst_seq: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) manifest_cache: Arc<async_lock::Mutex<ManifestCache>>,
    /// Pinned reader versions for WAL GC watermark.
    /// `BTreeMap<version, count>` — `min_key` is the watermark.
    pub(crate) readers: Arc<async_lock::Mutex<std::collections::BTreeMap<u64, usize>>>,
    /// Fair gate serializing every durable write path (`flush`,
    /// `flush_mem_to_sst*`, `gc_wal`, `compact`, `put_bytes`, `delete` and
    /// tx `commit`): concurrent writers queue here instead of CAS-retry-storming
    /// the manifest. Always acquired outermost, never while holding
    /// `manifest_cache`, so lock ordering stays acyclic. It is what makes WAL
    /// sequence allocation order equal program order.
    pub(crate) write_gate: Arc<async_lock::Mutex<()>>,
    /// Staged blind writes awaiting group commit. Whoever acquires
    /// `write_gate` drains the queue (capped by [`MAX_GROUP_WRITES`]) into
    /// one WAL file plus one manifest CAS; `delete` and tx commits take the
    /// gate without staging and ride it exclusively.
    pub(crate) pending: Arc<std::sync::Mutex<Vec<PendingWrite>>>,
    /// SST file cache - scan-resistant `S3-FIFO` (~256 MB with 32KB blocks).
    pub(crate) sst_cache: C,
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

/// Clones share writer state: the `MemTable`, sequencers, manifest cache,
/// write gate, and SST cache are reference-counted, so clones coordinate
/// as one owner. Required for decorator composition (`HookStore`, `OtelStore`).
impl<C> Clone for OxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    fn clone(&self) -> Self {
        Self::clone_shared(self)
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
}

/// Read access to the writer state shared by [`OxKvStore`] and [`OxKvTx`].
///
/// Implemented for both so [`OxKvStore::clone_shared`] has a single field list
/// instead of a hand-maintained copy per concrete type.
pub(crate) trait WriterStateRef<C> {
    fn inner(&self) -> &Arc<dyn Storage>;
    fn prefix(&self) -> &ObjectPath;
    fn epoch(&self) -> u64;
    fn session(&self) -> &str;
    fn mem(&self) -> &MemTable;
    fn wal_seq(&self) -> &Arc<std::sync::atomic::AtomicU64>;
    fn wal_buffer(&self) -> &WalBuffer;
    fn sst_seq(&self) -> &Arc<std::sync::atomic::AtomicU64>;
    fn manifest_cache(&self) -> &Arc<async_lock::Mutex<ManifestCache>>;
    fn readers(&self) -> &Arc<async_lock::Mutex<std::collections::BTreeMap<u64, usize>>>;
    fn write_gate(&self) -> &Arc<async_lock::Mutex<()>>;
    fn pending(&self) -> &Arc<std::sync::Mutex<Vec<PendingWrite>>>;
    fn sst_cache(&self) -> &C;
}

macro_rules! impl_writer_state_ref {
    ($ty:ty, $c:ty) => {
        impl<C> WriterStateRef<$c> for $ty {
            fn inner(&self) -> &Arc<dyn Storage> {
                &self.inner
            }
            fn prefix(&self) -> &ObjectPath {
                &self.prefix
            }
            fn epoch(&self) -> u64 {
                self.epoch
            }
            fn session(&self) -> &str {
                &self.session
            }
            fn mem(&self) -> &MemTable {
                &self.mem
            }
            fn wal_seq(&self) -> &Arc<std::sync::atomic::AtomicU64> {
                &self.wal_seq
            }
            fn wal_buffer(&self) -> &WalBuffer {
                &self.wal_buffer
            }
            fn sst_seq(&self) -> &Arc<std::sync::atomic::AtomicU64> {
                &self.sst_seq
            }
            fn manifest_cache(&self) -> &Arc<async_lock::Mutex<ManifestCache>> {
                &self.manifest_cache
            }
            fn readers(&self) -> &Arc<async_lock::Mutex<std::collections::BTreeMap<u64, usize>>> {
                &self.readers
            }
            fn write_gate(&self) -> &Arc<async_lock::Mutex<()>> {
                &self.write_gate
            }
            fn pending(&self) -> &Arc<std::sync::Mutex<Vec<PendingWrite>>> {
                &self.pending
            }
            fn sst_cache(&self) -> &C {
                &self.sst_cache
            }
        }
    };
}

impl_writer_state_ref!(OxKvStore<C>, C);
impl_writer_state_ref!(OxKvTx<C>, C);

impl<C> OxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    /// Copies every piece of writer state, whatever the source shape.
    ///
    /// The single place the writer-state field list is written down: `Clone`,
    /// `begin_tx` and the transaction's maintenance view all route through it,
    /// so adding a field cannot leave one of the three initializers stale.
    pub(crate) fn clone_shared<S: WriterStateRef<C>>(src: &S) -> Self {
        Self {
            inner: Arc::clone(src.inner()),
            prefix: src.prefix().clone(),
            epoch: src.epoch(),
            session: src.session().to_string(),
            mem: Arc::clone(src.mem()),
            wal_seq: Arc::clone(src.wal_seq()),
            wal_buffer: Arc::clone(src.wal_buffer()),
            sst_seq: Arc::clone(src.sst_seq()),
            manifest_cache: Arc::clone(src.manifest_cache()),
            readers: Arc::clone(src.readers()),
            write_gate: Arc::clone(src.write_gate()),
            pending: Arc::clone(src.pending()),
            sst_cache: src.sst_cache().clone(),
        }
    }

    /// Encodes ops into the framed WAL record format.
    pub(crate) fn encode_ops(ops: &[(String, Option<Vec<u8>>)]) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        for (key, value) in ops {
            if let Some(val) = value {
                crate::store::encode_record(&mut buf, key, val)
                    .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?;
            } else {
                let klen = u32::try_from(key.len())
                    .map_err(|e| StoreError::Storage(format!("key too long: {e}")))?;
                buf.extend_from_slice(&klen.to_le_bytes());
                buf.extend_from_slice(key.as_bytes());
                buf.extend_from_slice(&TOMBSTONE_VLEN.to_le_bytes());
            }
        }
        Ok(buf)
    }

    async fn apply_mem(&self, updates: &[(String, Option<Vec<u8>>)]) {
        if updates.is_empty() {
            return;
        }
        let mut mem = self.mem.write().await;
        for (key, value) in updates {
            mem.insert(key.clone(), value.clone());
        }
    }

    /// Best-effort maintenance after a durable write.
    ///
    /// Never fails the write it follows — these are threshold-driven and must
    /// not turn an acknowledged write into an error.
    async fn maintain_after_write(&self, wal_len: usize) {
        let _ = self.flush_mem_to_sst_inner(false).await;
        let _ = self.compact_inner().await;
        self.maintain_wal(wal_len).await;
    }

    /// Appends one WAL record and lists it in the manifest.
    ///
    /// The single durable-append path: store flush, group commit, delete and
    /// tx commit all route through here, so fencing and the manifest CAS
    /// discipline cannot drift apart between hand-copied versions.
    ///
    /// Caller must hold [`Self::write_gate`] — that is what makes the
    /// sequence allocated here reflect program order.
    ///
    /// `mem_updates` is applied to the `MemTable` once the record is durable;
    /// pass an empty slice when the records are already in the `MemTable`
    /// (staged writes).
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` on `PUT`/CAS failure, `StoreError::Fenced`
    /// if ownership moved, or an error if this sequence already holds
    /// different bytes.
    pub(crate) async fn append_wal_locked(
        &self,
        payload: Vec<u8>,
        mem_updates: &[(String, Option<Vec<u8>>)],
    ) -> Result<()> {
        let seq = self
            .wal_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = wal_path(&self.prefix, self.epoch, seq);

        match self
            .inner
            .put_opts(&path, payload.clone(), PutMode::Create)
            .await
        {
            Ok(_) => {}
            // `Create` lost: either an idempotent retry of our own record, or a
            // burned sequence reused by a restarted writer. Only the first is
            // safe, so verify the bytes rather than assuming them.
            Err(StoreError::CasConflict(_)) => {
                let existing = self
                    .inner
                    .get(&path)
                    .await
                    .map_err(|e| StoreError::Storage(format!("read back wal failed: {e}")))?;
                if existing.bytes != payload {
                    return Err(StoreError::Storage(format!(
                        "wal sequence {seq} already holds different bytes; refusing to overwrite"
                    )));
                }
            }
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
                self.apply_mem(mem_updates).await;
                self.maintain_after_write(manifest.wal.len()).await;
                return Ok(());
            }
            manifest.wal.push(wal_id.clone());
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    let wal_len = manifest.wal.len();
                    self.manifest_cache.lock().await.update(manifest, new_etag);
                    self.apply_mem(mem_updates).await;
                    self.maintain_after_write(wal_len).await;
                    return Ok(());
                }
                Err(StoreError::CasConflict(detail)) => {
                    self.manifest_cache.lock().await.clear();
                    if attempt == 3 {
                        return Err(StoreError::Storage(format!(
                            "wal manifest CAS conflict after retries: {detail}"
                        )));
                    }
                    sleep(cas_backoff(attempt)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Drains ops staged through `stage_set` into one durable WAL record.
    ///
    /// Every durable write path runs this *before* allocating its own WAL
    /// sequence, so a `stage_set` that happened-before a `put_bytes` gets the
    /// lower sequence and wins on replay. Allocating the sequence at flush time
    /// instead let an acknowledged write be reverted by a restart, and let a
    /// staged write resurrect a deleted key.
    ///
    /// Caller must hold [`Self::write_gate`].
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on `PUT`/ownership/manifest failure.
    pub(crate) async fn drain_staged_locked(&self) -> Result<()> {
        let ops: Vec<(String, Option<Vec<u8>>)> = {
            let mut buf = self.wal_buffer.lock().await;
            std::mem::take(&mut *buf)
        };
        if ops.is_empty() {
            return Ok(());
        }
        // Staged ops are already in the `MemTable`; no mem updates needed.
        self.append_wal_locked(Self::encode_ops(&ops)?, &[]).await
    }

    /// Flushes buffered WAL ops to `e{epoch}/wal/{seq:08}.log` via
    /// `PutMode::Create` (`If-None-Match:"*"`), then gates on ownership.
    ///
    /// Implements `commit_durable` RPO=0. Takes [`Self::write_gate`], so these
    /// records land in program order relative to every other durable write.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` on `PUT` failure and `StoreError::Fenced`
    /// if `ownership.json` no longer names this epoch/session after the `PUT`.
    pub async fn flush(&self) -> Result<()> {
        let _gate = self.write_gate.lock().await;
        self.drain_staged_locked().await
    }

    /// Best-effort WAL maintenance after a manifest CAS carrying `wal_len`
    /// entries: force-flush an SST and GC covered WALs once the list reaches
    /// `WAL_MAINTENANCE_COUNT`, keeping per-write manifest cost flat.
    /// Failures are swallowed (fencing/conflicts/reader pins).
    ///
    /// Caller must hold [`Self::write_gate`].
    async fn maintain_wal(&self, wal_len: usize) {
        if wal_len < WAL_MAINTENANCE_COUNT {
            return;
        }
        // GC is only safe once the flush it depends on has succeeded: the
        // records it would delete live in that SST and nowhere else. Swallowing
        // the flush error and deleting anyway lost acknowledged writes.
        match self.flush_mem_to_sst_inner(true).await {
            Ok(_) => {}
            Err(_) => return,
        }
        let _ = self.gc_wal_inner().await;
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
        let _gate = self.write_gate.lock().await;
        self.flush_mem_to_sst_inner(false).await
    }

    /// Forces `MemTable` flush to `L0` regardless of size.
    ///
    /// # Errors
    ///
    /// Same as [`Self::flush_mem_to_sst`].
    pub async fn flush_mem_to_sst_force(&self) -> Result<Option<SstMeta>> {
        let _gate = self.write_gate.lock().await;
        self.flush_mem_to_sst_inner(true).await
    }

    /// Drops the `MemTable` entries that were just made durable in an SST.
    ///
    /// Compare-and-remove, not unconditional: a key rewritten between the
    /// snapshot and this point holds a *newer* value that is not in the SST,
    /// and removing it would silently drop an acknowledged write. An entry
    /// whose value still matches the snapshot is safe to drop, including when
    /// a newer write happened to store the same bytes.
    async fn discard_flushed(
        &self,
        snapshot: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
    ) {
        let mut mem = self.mem.write().await;
        for (key, value) in snapshot {
            if mem.get(key) == Some(value) {
                mem.remove(key);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn flush_mem_to_sst_inner(&self, force: bool) -> Result<Option<SstMeta>> {
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
            self.discard_flushed(&snapshot).await;
            return Ok(existing);
        }
        let sst_meta = SstMeta {
            id: sst_id.clone(),
            seq,
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
                self.discard_flushed(&snapshot).await;
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
                    self.discard_flushed(&snapshot).await;
                    return Ok(reloaded.sst.iter().find(|m| m.id == sst_id).cloned());
                }
                Err(StoreError::Storage(format!(
                    "manifest CAS conflict after backoff retry: {detail}"
                )))
            }
            Err(e) => Err(e),
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
        let staged = self.mem.read().await.get(key).cloned();
        match point_lookup(&self.read_ctx(), staged, key).await {
            Err(e) if is_not_found(&e) => {
                self.manifest_cache.lock().await.clear();
                let staged = self.mem.read().await.get(key).cloned();
                point_lookup(&self.read_ctx(), staged, key).await
            }
            other => other,
        }
    }

    #[must_use]
    fn read_ctx(&self) -> ReadCtx<'_, C> {
        ReadCtx {
            inner: &self.inner,
            prefix: &self.prefix,
            epoch: self.epoch,
            manifest_cache: &self.manifest_cache,
            sst_cache: &self.sst_cache,
        }
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
        let layers = vec![{
            let mem = self.mem.read().await;
            filter_rows(&mem, direction, &cursor)
        }];
        match range_lookup(
            &self.read_ctx(),
            layers,
            limit,
            direction,
            (cursor.0.clone(), cursor.1.clone()),
        )
        .await
        {
            Err(e) if is_not_found(&e) => {
                self.manifest_cache.lock().await.clear();
                let layers = vec![{
                    let mem = self.mem.read().await;
                    filter_rows(&mem, direction, &cursor)
                }];
                range_lookup(&self.read_ctx(), layers, limit, direction, cursor).await
            }
            other => other,
        }
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
        let _gate = self.write_gate.lock().await;
        self.gc_wal_inner().await
    }

    pub(crate) async fn gc_wal_inner(&self) -> Result<usize> {
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
        let _gate = self.write_gate.lock().await;
        self.compact_inner().await
    }

    /// Caller must hold [`Self::write_gate`].
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn compact_inner(&self) -> Result<Option<SstMeta>> {
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
                seq,
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
            // Oldest-first by write sequence, never by key range: the read
            // paths walk this list in reverse and stop at the first hit, so
            // list position is recency. Sorting by `min_key` let a fresh L0
            // file land ahead of older L1 data and serve stale values.
            new_sst_list.sort_by_key(|m| m.seq);
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
    ///
    /// Caller must hold [`Self::write_gate`].
    async fn persist_batch(&self, batch: &[PendingWrite]) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        // Anything staged earlier belongs ahead of this batch in the WAL.
        self.drain_staged_locked().await?;
        // Atomic: write the whole batch before mutating MemTable
        let mut payload_buf = Vec::new();
        for entry in batch {
            payload_buf.extend_from_slice(&entry.encoded);
        }
        let updates: Vec<(String, Option<Vec<u8>>)> = batch
            .iter()
            .map(|e| (e.key.clone(), Some(e.value.clone())))
            .collect();
        self.append_wal_locked(payload_buf, &updates).await
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
        let _gate = self.write_gate.lock().await;
        // Anything staged earlier belongs ahead of this tombstone in the WAL.
        self.drain_staged_locked().await?;
        // Atomic: encode tombstone and flush WAL before mutating MemTable
        let payload_buf = OxKvStore::<C>::encode_ops(&[(key.to_string(), None)])?;
        self.append_wal_locked(payload_buf, &[(key.to_string(), None)])
            .await?;
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
        let id = PENDING_CTR.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        lock_ignore_poison(&self.pending).push(PendingWrite {
            id,
            key: key.to_string(),
            value: value.to_vec(),
            encoded,
            responder,
        });
        let _gate = self.write_gate.lock().await;
        loop {
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
            // Leader: drain queued writes (at least our own entry) into one WAL
            // file plus one manifest CAS; overflow rides the next holder.
            let batch: Vec<PendingWrite> = {
                let mut pending = lock_ignore_poison(&self.pending);
                let take = pending.len().min(MAX_GROUP_WRITES);
                pending.drain(..take).collect()
            };
            if batch.is_empty() {
                // Our entry was taken by a leader that has not answered yet.
                return waiter
                    .await
                    .map_err(|_| {
                        StoreError::Storage(
                            "group commit leader lost; write fate unknown — read back to confirm"
                                .to_string(),
                        )
                    })
                    .and_then(|r| r);
            }
            // A leader whose own entry fell outside the capped batch must not
            // report that batch's success as *its* outcome: its record is still
            // queued, so acknowledging now would claim durability it lacks.
            let mine = batch.iter().any(|e| e.id == id);
            let result = self.persist_batch(&batch).await;
            for entry in batch {
                let _ = entry.responder.send(result.clone());
            }
            if mine {
                return result;
            }
        }
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
impl<C> Store for OxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    type Transaction = OxKvTx<C>;

    fn begin_tx(&self) -> Result<Self::Transaction> {
        let shared = Self::clone_shared(self);
        Ok(OxKvTx {
            inner: shared.inner,
            prefix: shared.prefix,
            epoch: shared.epoch,
            session: shared.session,
            mem: shared.mem,
            wal_seq: shared.wal_seq,
            wal_buffer: shared.wal_buffer,
            sst_seq: shared.sst_seq,
            manifest_cache: shared.manifest_cache,
            readers: shared.readers,
            write_gate: shared.write_gate,
            pending: shared.pending,
            sst_cache: shared.sst_cache,
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

    /// Builds a write-through RAM mirror with the default SST cache.
    ///
    /// Acquires ownership like [`Self::build`], then warms every key into
    /// memory via [`CachedOxKvStore::open`]. Reads serve from the mirror
    /// while writes keep WAL durability; see the type docs for the
    /// single-writer contract and the staleness knobs.
    ///
    /// Requires the `btree` feature: the mirror is a `BTreeStore`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails, `StoreError::Fenced`
    /// if `ownership.json` CAS loses the race, or `StoreError` when the
    /// warming scan fails.
    #[cfg(feature = "btree")]
    pub async fn build_cached(self) -> Result<CachedOxKvStore> {
        let inner = self.build().await?;
        CachedOxKvStore::open(inner).await
    }

    /// Builds a write-through RAM mirror with a caller-supplied SST cache.
    ///
    /// See [`Self::build_cached`] for the ownership and warming semantics
    /// and [`Self::build_with_cache`] for the cache choices. With a full
    /// mirror the SST cache only serves maintenance reads, so it can stay
    /// small.
    ///
    /// Requires the `btree` feature.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails, `StoreError::Fenced`
    /// if `ownership.json` CAS loses the race, or `StoreError` when the
    /// warming scan fails.
    #[cfg(feature = "btree")]
    pub async fn build_cached_with_cache<C>(self, cache: C) -> Result<CachedOxKvStore<C>>
    where
        C: Cache<String, Arc<SstFile>>,
    {
        let inner = self.build_with_cache(cache).await?;
        CachedOxKvStore::open(inner).await
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
        // WAL replay for restart/fencing — make not-yet-SSTed WAL visible
        {
            let (manifest, _etag) = load_manifest(
                Arc::clone(&s3store.inner),
                &s3store.prefix,
                s3store.epoch,
                &s3store.manifest_cache,
                std::time::Duration::from_secs(1),
            )
            .await?;
            replay_listed_wals(
                &s3store.inner,
                &s3store.prefix,
                s3store.epoch,
                &s3store.manifest_cache,
                &manifest.wal,
                &s3store.mem,
            )
            .await?;
            let cur_epoch = s3store.epoch;
            let wal_prefix = format!("e{cur_epoch:06}/wal/");
            // High-water mark, not a tally: a sequence burned by a failed
            // append leaves an unlisted object, so counting the listed WALs
            // can hand a live sequence to the next write.
            let mut max_wal = 0u64;
            for id in &manifest.wal {
                if let Some(num) = id
                    .strip_prefix(&wal_prefix)
                    .and_then(|rest| rest.strip_suffix(".log"))
                    .and_then(|num| num.parse::<u64>().ok())
                {
                    max_wal = max_wal.max(num + 1);
                }
            }
            s3store
                .wal_seq
                .store(max_wal, std::sync::atomic::Ordering::SeqCst);
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
