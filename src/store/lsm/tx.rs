//! Transactional overlay over an [`OxKvStore`](super::store::OxKvStore).
//!
//! [`OxKvTx`] stages writes invisibly until `commit` and shares the lookup
//! engine, the WAL module, and the owner state with its parent store.

use std::sync::Arc;

use async_trait::async_trait;

use super::manifest::{ManifestCache, cas_manifest, load_manifest};
use super::ownership::{cas_backoff, read_ownership, wal_path};
use super::read::{ReadCtx, filter_rows, is_not_found, point_lookup, range_lookup, resolve_value};
use super::sst::TOMBSTONE_VLEN;
use super::store::OxKvStore;
use super::store::PendingWrite;
use super::{MemTable, WAL_MAINTENANCE_COUNT};
use crate::store::cache::{Cache, LruCache};
use crate::store::storage::{ObjectPath, PutMode, Storage};
use crate::store::{
    Direction, GetSet, KeyValue, Result, SstFile, StoreError, Transaction, lock_ignore_poison,
    sleep,
};

/// Transaction for `OxKvStore` — staged overlay, durable only on `commit`.
///
/// `stage_set`/`stage_delete` are buffered in `overlay` and invisible to
/// the parent `OxKvStore` until `commit` applies them to the shared
/// `MemTable`/`WalBuffer` and `flush`es the WAL to S3 (RPO=0).
/// `get`/`has`/`gets` see `overlay` first (read-your-writes) then the
/// parent's `MemTable` + `SST`s via the same heap-merge.
pub struct OxKvTx<C = LruCache<String, Arc<SstFile>>> {
    pub(crate) inner: Arc<dyn Storage>,
    pub(crate) prefix: ObjectPath,
    pub(crate) epoch: u64,
    pub(crate) session: String,
    pub(crate) mem: MemTable,
    pub(crate) wal_seq: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) sst_seq: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) manifest_cache: Arc<async_lock::Mutex<ManifestCache>>,
    pub(crate) readers: Arc<async_lock::Mutex<std::collections::BTreeMap<u64, usize>>>,
    /// Fair gate serializing the durable write paths (`put_bytes` and tx
    /// `commit`): concurrent writers queue here instead of CAS-retry-storming
    /// the manifest. Always acquired outermost, never while holding
    /// `manifest_cache`, so lock ordering stays acyclic.
    pub(crate) write_gate: Arc<async_lock::Mutex<()>>,
    /// Group-commit queue shared with the parent store: blind writes staged
    /// anywhere drain through whoever holds `write_gate`.
    pub(crate) pending: Arc<std::sync::Mutex<Vec<PendingWrite>>>,
    pub(crate) sst_cache: C,
    pub(crate) overlay: std::sync::Mutex<std::collections::BTreeMap<String, Option<Vec<u8>>>>,
}

#[async_trait]
#[allow(clippy::too_many_lines)]
impl<C> GetSet for OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let staged = lock_ignore_poison(&self.overlay).get(key).cloned();
        if let Some(hit) = staged {
            return match hit {
                Some(raw) => Ok(Some(resolve_value(&self.read_ctx(), raw).await?)),
                None => Ok(None),
            };
        }
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
        let overlay_rows = {
            let overlay = lock_ignore_poison(&self.overlay);
            filter_rows(&overlay, direction, &cursor)
        };
        let mem_rows = {
            let mem = self.mem.read().await;
            filter_rows(&mem, direction, &cursor)
        };
        match range_lookup(
            &self.read_ctx(),
            vec![overlay_rows, mem_rows],
            limit,
            direction,
            (cursor.0.clone(), cursor.1.clone()),
        )
        .await
        {
            Err(e) if is_not_found(&e) => {
                self.manifest_cache.lock().await.clear();
                let overlay_rows = {
                    let overlay = lock_ignore_poison(&self.overlay);
                    filter_rows(&overlay, direction, &cursor)
                };
                let mem_rows = {
                    let mem = self.mem.read().await;
                    filter_rows(&mem, direction, &cursor)
                };
                range_lookup(
                    &self.read_ctx(),
                    vec![overlay_rows, mem_rows],
                    limit,
                    direction,
                    cursor,
                )
                .await
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

impl<C> OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
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
