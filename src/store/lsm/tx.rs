//! Transactional overlay over an [`OxKvStore`](super::store::OxKvStore).
//!
//! [`OxKvTx`] stages writes invisibly until `commit`. It shares the lookup
//! engine with its parent store. It also shares the WAL module. It also
//! shares the owner state.

use std::sync::Arc;

use async_trait::async_trait;

use super::manifest::ManifestCache;
use super::read::{
    ReadCtx, filter_rows, point_lookup, range_lookup, resolve_value, retry_once_not_found,
};
use super::store::OxKvStore;
use super::store::PendingWrite;
use super::{MemTable, WalBuffer};
use crate::store::cache::{Cache, LruCache};
use crate::store::storage::{ObjectPath, Storage};
use crate::store::{Direction, GetSet, KeyValue, Result, SstFile, Transaction, lock_ignore_poison};

/// Transaction for `OxKvStore`. It stages an overlay. It is durable only
/// on `commit`.
///
/// `stage_set` and `stage_delete` buffer their writes in `overlay`. The
/// parent `OxKvStore` does not see these writes. The `commit` method
/// applies them to the shared `MemTable` and `WalBuffer`. The method then
/// flushes the WAL to S3 (RPO=0).
///
/// `get`, `has`, and `gets` read `overlay` first. This order gives
/// read-your-writes behavior. Then these methods read the parent
/// `MemTable` and the `SST`s through the same heap merge.
pub struct OxKvTx<C = LruCache<String, Arc<SstFile>>> {
    pub(crate) inner: Arc<dyn Storage>,
    pub(crate) prefix: ObjectPath,
    pub(crate) epoch: u64,
    pub(crate) session: String,
    pub(crate) mem: MemTable,
    pub(crate) wal_seq: Arc<std::sync::atomic::AtomicU64>,
    /// Shared with the parent store. A tx commit uses this field to order
    /// itself after anything already staged there.
    pub(crate) wal_buffer: WalBuffer,
    pub(crate) sst_seq: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) manifest_cache: Arc<async_lock::Mutex<ManifestCache>>,
    /// Fair gate serializing the durable write paths (`put_bytes` and tx
    /// `commit`). Concurrent writers queue here instead of storming the
    /// manifest with CAS retries. Callers always acquire this gate
    /// outermost. Callers never acquire it while they hold
    /// `manifest_cache`. This order keeps the lock ordering acyclic.
    pub(crate) write_gate: Arc<async_lock::Mutex<()>>,
    /// Group-commit queue shared with the parent store. Blind writes
    /// staged anywhere drain through whoever holds `write_gate`.
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
        retry_once_not_found(&self.manifest_cache, || {
            let staged = async { self.mem.read().await.get(key).cloned() };
            async move { point_lookup(&self.read_ctx(), staged.await, key).await }
        })
        .await
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
        retry_once_not_found(&self.manifest_cache, || {
            let layers = self.snapshot_layers(direction, &cursor);
            let cursor2 = cursor.clone();
            async move {
                range_lookup(&self.read_ctx(), layers.await, limit, direction, cursor2).await
            }
        })
        .await
    }
}

#[async_trait]
impl<C> Transaction for OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    #[allow(clippy::too_many_lines)]
    async fn commit(self) -> Result<()> {
        // Serialize concurrent writers. See `write_gate`.
        let _gate = self.write_gate.lock().await;
        // Drain under the lock. The `commit` method consumes the
        // transaction, so taking ownership up front is equivalent. It also
        // keeps no guard across awaits.
        let overlay = std::mem::take(&mut *lock_ignore_poison(&self.overlay));
        if overlay.is_empty() {
            return Ok(());
        }
        // Encode directly from overlay. Do not mutate shared mem until WAL
        // is durable (atomic).
        let updates: Vec<(String, Option<Vec<u8>>)> = overlay.into_iter().collect();
        let payload_buf = OxKvStore::<C>::encode_ops(&updates)?;
        // Anything already staged on the parent store happened-before this
        // commit. It must land ahead of this commit in the WAL.
        self.maintenance_view().drain_staged_locked().await?;
        self.maintenance_view()
            .append_wal_locked(payload_buf, &updates)
            .await?;
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
    /// Newest-layer rows for a scan. The layers are the transaction overlay
    /// over `MemTable`.
    async fn snapshot_layers(
        &self,
        direction: Direction,
        cursor: &(Option<String>, Option<String>),
    ) -> Vec<Vec<(String, Option<Vec<u8>>)>> {
        let overlay_rows = {
            let overlay = lock_ignore_poison(&self.overlay);
            filter_rows(&overlay, direction, cursor)
        };
        let mem_rows = {
            let mem = self.mem.read().await;
            filter_rows(&mem, direction, cursor)
        };
        vec![overlay_rows, mem_rows]
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

    /// Store view sharing all mutable state. It runs the durable write path
    /// and the maintenance tasks (SST flush, GC, compaction) from tx-only
    /// workloads.
    fn maintenance_view(&self) -> OxKvStore<C> {
        OxKvStore::clone_shared(self)
    }
}
