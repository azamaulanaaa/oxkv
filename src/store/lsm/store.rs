//! This module provides the single-writer LSM store. It provides the
//! durable write paths over the shared engines.
//!
//! [`OxKvStore`] owns the writer state. It serializes concurrent writers
//! behind a fair gate. Reads delegate to the shared lookup engine. WAL work
//! delegates to the WAL module. Construction goes through
//! [`OxKvStoreBuilder`]. Transactions convert through
//! [`OxKvTx`](super::tx::OxKvTx).

use std::sync::Arc;

use async_trait::async_trait;

use super::blob::{encode_blob_pointer, is_overflow, put_blob};
#[cfg(feature = "btree")]
use super::cached::CachedOxKvStore;
use super::manifest::{ManifestCache, SstMeta, cas_manifest, load_manifest};
use super::merge::merge_sources;
use super::ownership::{
    acquire_ownership, cas_backoff, format_epoch, read_ownership, sst_path, wal_path,
};
use super::probe::probe_store;
use super::read::{
    ReadCtx, fetch_sst, filter_rows, point_lookup, range_lookup, resolve_value,
    retry_once_not_found,
};
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

/// One staged blind write. It waits for a group commit or rides one.
///
/// The store encodes the payload once at staging. Therefore a failure at that
/// point fails only its own write. The leader concatenates `encoded`
/// buffers verbatim into a single WAL file. That file replays with no
/// format changes.
pub(crate) struct PendingWrite {
    id: u64,
    key: String,
    value: Vec<u8>,
    encoded: Vec<u8>,
    responder: futures::channel::oneshot::Sender<Result<()>>,
}

/// Process-wide id for [`PendingWrite`]. It lets a group-commit leader
/// tell whether the batch it drained contained its own write.
static PENDING_CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Storage-backed LSM store. It has a probe. It has fencing. It has a WAL
/// gate. It has SST handling.
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
    /// Fair gate serializing every durable write path (`flush`,
    /// `flush_mem_to_sst*`, `gc_wal`, `compact`, `put_bytes`, `delete` and
    /// tx `commit`). Concurrent writers queue here instead of storming the
    /// manifest with CAS retries. Callers always acquire this gate
    /// outermost. Callers never acquire it while they hold
    /// `manifest_cache`. This order keeps the lock ordering acyclic. This
    /// gate makes the WAL sequence allocation order equal the program
    /// order.
    pub(crate) write_gate: Arc<async_lock::Mutex<()>>,
    /// Staged blind writes awaiting group commit. Whoever acquires
    /// `write_gate` drains the queue into one WAL file and one manifest
    /// CAS. The queue holds at most [`MAX_GROUP_WRITES`] entries. The
    /// `delete` method and tx commits take the gate without staging. They
    /// then use the gate on their own.
    pub(crate) pending: Arc<std::sync::Mutex<Vec<PendingWrite>>>,
    /// SST file cache. It uses a scan-resistant `S3-FIFO` (~256 MB with 32KB
    /// blocks).
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

/// Clones share the writer state. The `MemTable`, the sequencers, the
/// manifest cache, the write gate, and the SST cache are reference-counted.
/// Therefore clones coordinate as one owner. Decorator composition
/// (`HookStore`, `OtelStore`) requires this sharing.
impl<C> Clone for OxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    fn clone(&self) -> Self {
        Self::clone_shared(self)
    }
}

/// Cache-independent entry points (`builder`, `probe`) live on the default
/// `LruCache` instantiation. Therefore `OxKvStore::builder()` needs no
/// turbofish.
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
    /// The probe validates `If-None-Match` and `If-Match` conditional writes.
    /// The method returns `Ok(())` only on
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

    /// Returns SST-cache hit/miss statistics. It returns `None` for cache
    /// backends that do not track them (e.g. `moka`).
    #[must_use]
    pub fn sst_cache_stats(&self) -> Option<CacheStats> {
        self.sst_cache.stats()
    }

    /// Stages `set` into `MemTable` and the WAL buffer (commit = mem).
    ///
    /// This method does not access storage. Use [`Self::flush`] for RPO=0.
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

    /// Stages `delete` into `MemTable` and the WAL buffer.
    pub async fn stage_delete(&self, key: &str) {
        self.mem.write().await.insert(key.to_string(), None);
        self.wal_buffer.lock().await.push((key.to_string(), None));
    }
}

/// Read access to the writer state shared by [`OxKvStore`] and [`OxKvTx`].
///
/// Implemented for both types. Therefore [`OxKvStore::clone_shared`] has a
/// single field list. A hand-maintained copy per concrete type is not
/// needed.
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
    /// This is the one place that holds the *field assignments*. `Clone` and
    /// the transaction's maintenance view both route through it.
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
    /// This method never fails the write it follows. These tasks are
    /// threshold-driven. They must not turn an acknowledged write into an
    /// error.
    async fn maintain_after_write(&self, wal_len: usize) {
        let _ = self.flush_mem_to_sst_inner(false).await;
        let _ = self.compact_inner().await;
        self.maintain_wal(wal_len).await;
    }

    /// Appends one WAL record and lists it in the manifest.
    ///
    /// This is the single durable-append path. Store flush, group commit,
    /// delete, and tx commit all route through here. Therefore the fencing
    /// and the manifest CAS discipline cannot drift apart between
    /// hand-copied versions.
    ///
    /// The caller must hold [`Self::write_gate`]. That rule makes the
    /// sequence allocated here reflect the program order.
    ///
    /// The store applies `mem_updates` to the `MemTable` once the record is
    /// durable. Pass an empty slice when the records are already in the
    /// `MemTable` (staged writes).
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` on `PUT`/CAS failure. It also returns
    /// `StoreError::Fenced` if ownership moved. It also returns an error if
    /// this sequence already holds different bytes.
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
            // `Create` lost. The cause is either an idempotent retry of our own
            // record or a burned sequence reused by a restarted writer. Only
            // the first cause is safe. Verify the bytes rather than
            // assuming them.
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
            // Owned copy for mutation. Readers share the cached `Arc`.
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
    /// Every durable write path runs this method *before* it allocates its
    /// own WAL sequence. Therefore a `stage_set` that happened-before a
    /// `put_bytes` gets the lower sequence and wins on replay. Allocating
    /// the sequence at flush time instead let an acknowledged write be
    /// reverted by a restart. It also let a staged write resurrect a
    /// deleted key.
    ///
    /// The caller must hold [`Self::write_gate`].
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
        // Staged ops are already in the `MemTable`. The call needs no mem updates.
        self.append_wal_locked(Self::encode_ops(&ops)?, &[]).await
    }

    /// Flushes buffered WAL ops to `e{epoch}/wal/{seq:08}.log` through
    /// `PutMode::Create` (`If-None-Match:"*"`). Then the method checks
    /// ownership.
    ///
    /// Implements `commit_durable` RPO=0. The method takes the writer gate.
    /// Therefore these records land in the program order relative to every
    /// other durable write.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` on `PUT` failure. It also returns
    /// `StoreError::Fenced` if `ownership.json` no longer names this
    /// epoch/session after the `PUT`.
    pub async fn flush(&self) -> Result<()> {
        let _gate = self.write_gate.lock().await;
        self.drain_staged_locked().await
    }

    /// Best-effort WAL maintenance after a manifest CAS that carries `wal_len`
    /// entries. The method force-flushes an SST. The method also GCs covered
    /// WALs once the list reaches `WAL_MAINTENANCE_COUNT`. This keeps the
    /// per-write manifest cost flat. The method swallows failures (fencing,
    /// conflicts, reader pins).
    ///
    /// The caller must hold [`Self::write_gate`].
    async fn maintain_wal(&self, wal_len: usize) {
        if wal_len < WAL_MAINTENANCE_COUNT {
            return;
        }
        // GC is only safe after the flush it depends on has succeeded. The
        // records it would delete live in that SST and nowhere else. Swallowing
        // the flush error and deleting anyway lost acknowledged writes.
        match self.flush_mem_to_sst_inner(true).await {
            Ok(_) => {}
            Err(_) => return,
        }
        let _ = self.gc_wal_inner().await;
    }

    /// Flushes `MemTable` to `L0` SST if above `32 MiB` or `force`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` on `PUT`/`CAS` failure. It also returns
    /// `StoreError::Fenced` if `ownership` no longer matches.
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
    /// Compare-and-remove, not unconditional. A key rewritten between the
    /// snapshot and this point holds a *newer* value that is not in the SST.
    /// Removing that key would silently drop an acknowledged write. An entry
    /// whose value still matches the snapshot is safe to drop. It is also
    /// safe to drop when a newer write happened to store the same bytes.
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
        // Fully prefixed id (same layout as `wal_path`). `fetch_sst` can
        // resolve it with `ObjectPath::from(id)`. Manifests stay
        // prefix-safe.
        let sst_id = sst_path(&self.prefix, self.epoch, 0, seq).to_string();
        let sst_path = ObjectPath::from(sst_id.as_str());
        let put_res = self
            .inner
            .put_opts(&sst_path, sst_bytes.clone(), PutMode::Create)
            .await;
        match put_res {
            Ok(_) => {}
            // `Create` lost. The cause is either an idempotent retry of our own SST
            // or a burned sequence whose object already holds different
            // bytes. Verify rather than assume, exactly as
            // `append_wal_locked` does for the WAL. Accepting blindly would
            // publish an `SstMeta` describing bytes that were never written.
            // The store would then drop the live `MemTable` entries on the
            // strength of them.
            Err(StoreError::CasConflict(_)) => {
                let existing = self
                    .inner
                    .get(&sst_path)
                    .await
                    .map_err(|e| StoreError::Storage(format!("read back sst failed: {e}")))?;
                if existing.bytes != sst_bytes {
                    return Err(StoreError::Storage(format!(
                        "sst sequence {seq} already holds different bytes; refusing to overwrite"
                    )));
                }
            }
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
        // Owned copy for mutation. Readers share the cached `Arc`.
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

    /// Reads `key` via `MemTable` → SSTs (newest first) → blob deref.
    ///
    /// Restarts once against a fresh manifest when an SST or blob read hits
    /// `not found`. The snapshot may predate a compaction that deleted the
    /// file.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O or CRC failure.
    pub async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        retry_once_not_found(&self.manifest_cache, || {
            let staged = async { self.mem.read().await.get(key).cloned() };
            async move { point_lookup(&self.read_ctx(), staged.await, key).await }
        })
        .await
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

    /// Range scan that merges `MemTable` and SSTs with tombstone suppression
    /// and blob deref.
    ///
    /// Restarts once against a fresh manifest when an SST or blob read hits
    /// `not found`.
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
        retry_once_not_found(&self.manifest_cache, || {
            let layers = async {
                let mem = self.mem.read().await;
                vec![filter_rows(&mem, direction, &cursor)]
            };
            let cursor2 = cursor.clone();
            async move {
                range_lookup(
                    &self.read_ctx(),
                    layers.await,
                    limit,
                    direction,
                    cursor2,
                )
                .await
            }
        })
        .await
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

    /// GCs `WAL` entries once a covering `SST` is manifest-visible.
    ///
    /// Every record in a listed `WAL` is also in the `MemTable` until a flush
    /// publishes it into an `SST`. `maintain_wal` only calls this after a
    /// *successful* force-flush. Therefore a non-empty `manifest.sst` means
    /// the listed `WAL` is covered. A concurrent
    /// [`OxKvReader`](super::reader::OxKvReader) that still has a collected
    /// `WAL` id finds it absent. The reader treats its records as
    /// `SST`-covered, which is what they are.
    ///
    /// Returns the number of files deleted.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on `GET`/`CAS`/`DELETE` failure.
    pub async fn gc_wal(&self) -> Result<usize> {
        let _gate = self.write_gate.lock().await;
        self.gc_wal_inner().await
    }

    pub(crate) async fn gc_wal_inner(&self) -> Result<usize> {
        for _ in 0..4 {
            let (manifest, etag) = load_manifest(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                &self.manifest_cache,
                std::time::Duration::from_secs(1),
            )
            .await?;
            // Owned copy for mutation. Readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
            if manifest.wal.is_empty() || manifest.sst.is_empty() {
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
    /// Optimistic. Multiple compactors may race. The loser reuses the
    /// `If-None-Match` SST through idempotency. The loser also retries `CAS`.
    /// `DELETE` deletes the old objects only after the replacement is
    /// manifest-visible. The method returns the new `L1` meta if it
    /// compacted.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O or `Fenced`.
    #[allow(clippy::too_many_lines)]
    pub async fn compact(&self) -> Result<Option<SstMeta>> {
        let _gate = self.write_gate.lock().await;
        self.compact_inner().await
    }

    /// The caller must hold [`Self::write_gate`].
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn compact_inner(&self) -> Result<Option<SstMeta>> {
        // Load the manifest and check the trigger.
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
        // Collect the L0 range.
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
        // Collect the overlapping L1 files.
        let mut l1_overlapping: Vec<SstMeta> = manifest_snapshot
            .sst
            .iter()
            .filter(|m| m.level == 1)
            .filter(|m| !(m.max_key.as_str() < l0_min || m.min_key.as_str() > l0_max))
            .cloned()
            .collect();
        // Bound the L1 file count. Fold the smallest adjacent pair, and
        // anything overlapping its range, into this compaction. Using
        // adjacent pairs only keeps the L1 sorted runs non-overlapping.
        // Therefore the L1 tombstone drop stays sound.
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
            // The fold appends by `min_key` adjacency, not by sequence. This
            // leaves the vector out of recency order. The merge below
            // consumes it with `.rev()`. `merge_sources` is newest-wins via
            // `or_insert`. Therefore an out-of-order entry would let an
            // older file's value win and then be deleted.
            l1_overlapping.sort_by_key(|m| m.seq);
        }
        if l0_metas.is_empty() && l1_overlapping.is_empty() {
            return Ok(None);
        }
        // Read all overlapping SSTs through a heap merge. The merge is
        // newest-wins. It suppresses tombstones in the final L1 except
        // where they are needed.
        let mut sources: Vec<Vec<(String, Option<Vec<u8>>)>> = Vec::new();
        for meta in l0_metas.iter().rev().chain(l1_overlapping.iter().rev()) {
            let sst = fetch_sst(&self.read_ctx(), &meta.id).await?;
            let scan = sst.scan_with_tombstones(None, None, None)?;
            let mut resolved: Vec<(String, Option<Vec<u8>>)> = Vec::with_capacity(scan.len());
            for (k, v) in scan {
                match v {
                    Some(raw) => {
                        // Same verification as the read path (`resolve_value`). Dereferencing
                        // here without it would inline a corrupt blob into L1.
                        // It would also make the corruption permanent.
                        let val = resolve_value(&self.read_ctx(), raw).await?;
                        resolved.push((k, Some(val)));
                    }
                    None => resolved.push((k, None)),
                }
            }
            sources.push(resolved);
        }
        // Merge with newest-wins. The merge keeps the tombstones for now.
        // It drops them if the L1 range is non-overlapping and a shadow
        // exists.
        let merged = merge_sources(sources);
        // Build the L1 entries. Drop the tombstones where no older shadow
        // exists. The L1 range is non-overlapping, so drop all tombstones.
        let mut l1_entries: std::collections::BTreeMap<String, Option<Vec<u8>>> =
            std::collections::BTreeMap::new();
        for kv in merged {
            // `merge_sources` already suppressed the tombstones for the L1
            // compaction. Therefore only live keys remain. A tombstone
            // (`None`) cannot remain in `merged`, so the code inserts only
            // live keys.
            l1_entries.insert(kv.key, Some(kv.value));
        }
        // Handle the overflow of large values for L1 too.
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
            // No live keys. CAS removes the old files only.
            for _ in 0..4 {
                let (manifest, etag) = load_manifest(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    &self.manifest_cache,
                    std::time::Duration::from_secs(1),
                )
                .await?;
                // Owned copy for mutation. Readers share the cached `Arc`.
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
            Ok(_) => {}
            // Same verification as the WAL append and the L0 flush. A `Create`
            // conflict means the object exists. It does not mean that the
            // object is ours.
            Err(StoreError::CasConflict(_)) => {
                let existing =
                    self.inner.get(&l1_path).await.map_err(|e| {
                        StoreError::Storage(format!("read back L1 sst failed: {e}"))
                    })?;
                if existing.bytes != sst_bytes {
                    return Err(StoreError::Storage(format!(
                        "L1 sst sequence {seq} already holds different bytes; \
                         refusing to overwrite"
                    )));
                }
            }
            Err(e) => return Err(StoreError::Storage(format!("put L1 sst failed: {e}"))),
        }
        // Verify that the store still owns the prefix.
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
        // CAS the manifest. Remove the old overlapping L0 and L1 files. Add the
        // new L1 file.
        for _ in 0..4 {
            let (manifest, etag) = load_manifest(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                &self.manifest_cache,
                std::time::Duration::from_secs(1),
            )
            .await?;
            // Owned copy for mutation. Readers share the cached `Arc`.
            let mut manifest = (*manifest).clone();
            // Idempotency. If the new L1 is already present, reuse it.
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
            // Oldest-first by write sequence, never by key range. The read
            // paths walk this list in reverse and stop at the first hit.
            // Therefore the list position is the recency. Sorting by
            // `min_key` let a fresh L0 file land ahead of older L1 data and
            // serve stale values.
            new_sst_list.sort_by_key(|m| m.seq);
            manifest.sst = new_sst_list;
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    self.manifest_cache.lock().await.update(manifest, new_etag);
                    // Invalidate the deleted files in `sst_cache`. Keep the new file.
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

    /// Persists one group-commit batch. The batch contains the concatenated
    /// records in a single WAL file. The
    /// store performs one ownership check. The store performs one manifest
    /// CAS append. The store applies the `MemTable` once. Then the store
    /// performs the usual maintenance. Shared fate. Every entry succeeds or
    /// fails together. This is exactly the outcome of sequential
    /// `put_bytes` calls if the process crashed between them.
    ///
    /// The caller must hold [`Self::write_gate`].
    async fn persist_batch(&self, batch: &[PendingWrite]) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        // Anything staged earlier belongs ahead of this batch in the WAL.
        self.drain_staged_locked().await?;
        // Atomic. Write the whole batch before mutating `MemTable`.
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
        // The existence read and the tombstone must be atomic with respect to
        // the gate. The drain must also happen either way. Reading before
        // the gate meant that a `put_bytes` still queued in `pending` was
        // invisible to it. Therefore `delete` could answer `false` for a
        // key that a moment later existed.
        let _gate = self.write_gate.lock().await;
        // Anything staged earlier belongs ahead of this tombstone in the WAL.
        self.drain_staged_locked().await?;
        // Read under the gate, after the drain, so it also sees staged ops.
        let existed = OxKvStore::get_bytes(self, key).await?.is_some();
        if !existed {
            return Ok(false);
        }
        // Atomic. Encode the tombstone and flush the WAL before mutating
        // `MemTable`.
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
        // Stage into the group-commit queue with encode-once semantics. A
        // failure here fails only this write. It happens before any shared
        // state moves.
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
            // Served while queued behind the gate. Return the recorded outcome.
            // `Closed` means the serving leader vanished mid-batch. The fate
            // of the write is unknowable. Report it instead of silently
            // dropping it.
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
            // Leader. Drain the queued writes (at least our own entry) into one
            // WAL file and one manifest CAS. The overflow rides the next
            // holder.
            let batch: Vec<PendingWrite> = {
                let mut pending = lock_ignore_poison(&self.pending);
                let take = pending.len().min(MAX_GROUP_WRITES);
                pending.drain(..take).collect()
            };
            if batch.is_empty() {
                // A leader that has not answered yet took our entry.
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
            // report that batch's success as *its* outcome. Its record is
            // still queued, so acknowledging now would claim durability it
            // lacks. Defence in depth, and currently unreachable.
            // `pending` is drained from the front. `write_gate` is fair
            // (FIFO). Therefore a leader is always at the head of the queue
            // it drains. Therefore its own entry is always inside the
            // batch. This check matters only if the push moved after the
            // lock acquisition or the gate stopped being FIFO. Either
            // change would let this task acknowledge a batch that excluded
            // its own write. That would claim durability the task does not
            // have. The check is cheap to keep, so keep it.
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
    /// Sets the backing [`Storage`]. Use `MemStorage` in tests and wasm.
    /// On native, an `object_store` backend also works via
    /// `with_object_store`.
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

    /// Sets the owner session id (unique per builder). If the caller sets no
    /// value, the builder generates a deterministic fallback.
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

    /// Asserts that no other writer touches the prefix. `TTL`-fresh manifests
    /// return from the cache without a revalidation poll. This saves one
    /// roundtrip per operation.
    ///
    /// Only enable this method when a single writer owns the prefix.
    /// Ownership checks and manifest CAS conflicts still detect a takeover.
    #[must_use]
    pub fn assume_single_writer(mut self, assume: bool) -> Self {
        self.assume_single_writer = assume;
        self
    }

    /// Builds a read-only view over the prefix without acquiring ownership.
    ///
    /// Skips the storage probe and the `ownership.json` epoch CAS. Therefore
    /// opening a reader never fences the current writer. The method ignores
    /// `with_session` and `assume_single_writer`. The manifest is always
    /// revalidated by TTL.
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
    /// Acquires ownership like [`Self::build`]. Then the method warms every
    /// key into memory via [`CachedOxKvStore::open`]. Reads serve from the
    /// mirror. Writes keep WAL durability. See the type docs for the
    /// single-writer contract and the staleness knobs.
    ///
    /// Requires the `btree` feature. The mirror is a `BTreeStore`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails. It also returns
    /// `StoreError::Fenced` if `ownership.json` CAS loses the race. It also
    /// returns `StoreError` when the warming scan fails.
    #[cfg(feature = "btree")]
    pub async fn build_cached(self) -> Result<CachedOxKvStore> {
        let inner = self.build().await?;
        CachedOxKvStore::open(inner).await
    }

    /// Builds a write-through RAM mirror with a caller-supplied SST cache.
    ///
    /// See [`Self::build_cached`] for the ownership and warming semantics.
    /// See [`Self::build_with_cache`] for the cache choices. With a full
    /// mirror the SST cache only serves maintenance reads, so it can stay
    /// small.
    ///
    /// Requires the `btree` feature.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails. It also returns
    /// `StoreError::Fenced` if `ownership.json` CAS loses the race. It also
    /// returns `StoreError` when the warming scan fails.
    #[cfg(feature = "btree")]
    pub async fn build_cached_with_cache<C>(self, cache: C) -> Result<CachedOxKvStore<C>>
    where
        C: Cache<String, Arc<SstFile>>,
    {
        let inner = self.build_with_cache(cache).await?;
        CachedOxKvStore::open(inner).await
    }

    /// Builds the store with the default SST cache (256 MB scan-resistant
    /// `S3-FIFO`, see [`LruCache`]). It runs the probe unless skipped.
    /// Then it acquires the `ownership.json` epoch with `CAS`. The method
    /// fences the returned store to that epoch.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails. It also returns
    /// `StoreError::Fenced` if `ownership.json` CAS loses the race.
    pub async fn build(self) -> Result<OxKvStore> {
        self.build_with_cache(LruCache::new(
            256 * 1024 * 1024,
            |_: &String, v: &Arc<SstFile>| u32::try_from(v.size()).unwrap_or(u32::MAX),
        ))
        .await
    }

    /// Builds the store with a caller-supplied SST cache. It runs the probe
    /// unless skipped. Then it acquires the `ownership.json` epoch with
    /// `CAS`.
    ///
    /// Pass the default [`LruCache`] (`S3-FIFO`, see [`Self::build`]). You
    /// can also pass a `moka::future::Cache` (native-only, `moka` feature).
    /// You can also pass any custom [`Cache`] implementation.
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
    /// Returns `StoreError::Storage` if the probe fails. It also returns
    /// `StoreError::Fenced` if `ownership.json` CAS loses the race.
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
            write_gate: Arc::new(async_lock::Mutex::new(())),
            pending: Arc::new(std::sync::Mutex::new(Vec::new())),
            sst_cache: cache,
        };
        // WAL replay for restart and fencing. It makes the not-yet-SSTed WAL
        // visible.
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
            let wal_prefix = format!("{}/wal/", format_epoch(cur_epoch));
            // High-water mark, not a tally. A sequence burned by a failed append
            // leaves an unlisted object. Counting the listed WALs can
            // therefore hand a live sequence to the next write.
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
            // High-water mark across the WHOLE prefix, not just this epoch.
            // The manifest is prefix-scoped. It carries SSTs forward across
            // an epoch takeover. But `seq` is a globally monotonic ordering
            // key. `compact_inner` sorts the list by it. Every read walks
            // that list in reverse as if position == recency. Restarting
            // the counter per epoch would make a new epoch's seq collide
            // with an inherited old-epoch seq. That would invert recency
            // on the next compaction.
            let mut max_sst: u64 = 0;
            for meta in &manifest.sst {
                if meta.id.contains("/sst/")
                    && let Some(fname) = meta.id.rsplit('/').next()
                    && let Some(s) = fname
                        .strip_suffix(".sst")
                        .and_then(|s| s.parse::<u64>().ok())
                {
                    max_sst = max_sst.max(s + 1);
                }
            }
            s3store
                .sst_seq
                .store(max_sst, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(s3store)
    }
}
