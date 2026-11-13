//! Transactional overlay over an [`OxKvStore`](super::store::OxKvStore).
//!
//! [`OxKvTx`] stages writes invisibly until `commit`. It shares the lookup
//! engine with its parent store. It also shares the WAL module. It also
//! shares the owner state.

use std::sync::Arc;

use async_trait::async_trait;

use super::manifest::ManifestCache;
use super::read::{
    ReadCtx, filter_rows, point_lookup, range_keys, range_lookup, resolve_value,
    retry_once_not_found,
};
use super::store::OxKvStore;
use super::store::PendingWrite;
use super::{MemTable, WalBuffer};
use crate::store::cache::{Cache, LruCache};
use crate::store::storage::{ObjectPath, Storage};
use crate::store::{
    ChangeEvent, ChangeKind, Direction, GetSet, HookState, KeyValue, Result, SstFile, StoreError,
    Transaction, TxCas, Validator, lock_ignore_poison,
};

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
    pub(crate) hooks: HookState,
    pub(crate) validators: Vec<Arc<dyn Validator>>,
    pub(crate) staged: std::sync::Mutex<Vec<ChangeEvent>>,
    /// Committed values that the transaction requires at commit time. The
    /// condition is `None` for an absent key.
    pub(crate) preconditions: std::sync::Mutex<std::collections::BTreeMap<String, Option<Vec<u8>>>>,
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
        let old_value = self.get_bytes(key).await?;
        let existed = old_value.is_some();
        if existed {
            lock_ignore_poison(&self.overlay).insert(key.to_string(), None);
            self.stage(ChangeEvent {
                key: key.to_string(),
                kind: ChangeKind::Delete,
                old_value,
                new_value: None,
                seq: 0,
            });
        }
        Ok(existed)
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        crate::store::hooks::validate_key(&self.validators, self, key, value).await?;
        let prev = self.get_bytes(key).await?;
        lock_ignore_poison(&self.overlay).insert(key.to_string(), Some(value.to_vec()));
        self.stage(ChangeEvent {
            key: key.to_string(),
            kind: ChangeKind::Set,
            old_value: prev.clone(),
            new_value: Some(value.to_vec()),
            seq: 0,
        });
        Ok(prev)
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        crate::store::hooks::validate_key(&self.validators, self, key, value).await?;
        let prev = self.get_bytes(key).await?;
        lock_ignore_poison(&self.overlay).insert(key.to_string(), Some(value.to_vec()));
        self.stage(ChangeEvent {
            key: key.to_string(),
            kind: ChangeKind::Set,
            old_value: prev,
            new_value: Some(value.to_vec()),
            seq: 0,
        });
        Ok(())
    }

    async fn gets_bytes(
        &self,
        limit: Option<usize>,
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

    async fn gets_keys(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<String>> {
        retry_once_not_found(&self.manifest_cache, || {
            let layers = self.snapshot_layers(direction, &cursor);
            let cursor2 = cursor.clone();
            async move {
                range_keys(&self.read_ctx(), layers.await, limit, direction, cursor2).await
            }
        })
        .await
    }
}

impl<C> OxKvTx<C> {
    fn stage(&self, mut event: ChangeEvent) {
        let mut staged = lock_ignore_poison(&self.staged);
        match staged.iter_mut().find(|e| e.key == event.key) {
            Some(existing) => {
                event.old_value = existing.old_value.take();
                *existing = event;
            }
            None => staged.push(event),
        }
    }

    /// Records the committed value that `key` must have at commit time.
    fn require_committed(&self, key: &str, expected: Option<Vec<u8>>) {
        lock_ignore_poison(&self.preconditions).insert(key.to_string(), expected);
    }

    /// Stages a delete without a read of the current overlay state.
    fn stage_delete(&self, key: &str, old_value: Option<Vec<u8>>) {
        lock_ignore_poison(&self.overlay).insert(key.to_string(), None);
        self.stage(ChangeEvent {
            key: key.to_string(),
            kind: ChangeKind::Delete,
            old_value,
            new_value: None,
            seq: 0,
        });
    }
}

impl<C> OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    /// Reads `key` from committed state. The transaction overlay is ignored.
    async fn committed_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        retry_once_not_found(&self.manifest_cache, || {
            let staged = async { self.mem.read().await.get(key).cloned() };
            async move { point_lookup(&self.read_ctx(), staged.await, key).await }
        })
        .await
    }
}

#[async_trait]
impl<C> TxCas for OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn set_if_absent(&self, key: &str, value: &[u8]) -> Result<()> {
        self.put_bytes(key, value).await?;
        self.require_committed(key, None);
        Ok(())
    }

    async fn compare_and_set(&self, key: &str, expected: Option<&[u8]>, new: &[u8]) -> Result<()> {
        self.put_bytes(key, new).await?;
        self.require_committed(key, expected.map(<[u8]>::to_vec));
        Ok(())
    }

    async fn delete_if(&self, key: &str, expected: &[u8]) -> Result<()> {
        let old_value = self.get_bytes(key).await?;
        self.stage_delete(key, old_value);
        self.require_committed(key, Some(expected.to_vec()));
        Ok(())
    }
}

#[async_trait]
impl<C> Transaction for OxKvTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    #[allow(clippy::too_many_lines)]
    async fn commit(self) -> Result<()> {
        let staged_events = lock_ignore_poison(&self.staged).clone();
        for event in &staged_events {
            if let (ChangeKind::Set, Some(value)) = (event.kind, event.new_value.as_deref()) {
                let view = crate::store::hooks::RevalidateView {
                    inner: &self,
                    key: &event.key,
                    old_value: event.old_value.as_deref(),
                };
                crate::store::hooks::validate_key(&self.validators, &view, &event.key, value)
                    .await?;
            }
        }

        // Serialize concurrent writers. See `write_gate`.
        let preconditions = lock_ignore_poison(&self.preconditions).clone();
        let _gate = self.write_gate.lock().await;
        // Drain under the lock. The `commit` method consumes the
        // transaction, so taking ownership up front is equivalent. It also
        // keeps no guard across awaits.
        let overlay = std::mem::take(&mut *lock_ignore_poison(&self.overlay));
        if overlay.is_empty() && preconditions.is_empty() {
            return Ok(());
        }
        // Encode directly from overlay. Do not mutate shared mem until WAL
        // is durable (atomic).
        let updates: Vec<(String, Option<Vec<u8>>)> = overlay.into_iter().collect();
        // Anything already staged on the parent store happened-before this
        // commit. It must land ahead of this commit in the WAL.
        self.maintenance_view().drain_staged_locked().await?;
        // The precondition check and the overlay apply share the write gate.
        // The check reads committed state, not the transaction overlay.
        for (key, expected) in &preconditions {
            let current = self.committed_get(key).await?;
            if current.as_deref() != expected.as_deref() {
                return Err(StoreError::TxConflict {
                    key: key.clone(),
                    current,
                });
            }
        }
        if updates.is_empty() {
            return Ok(());
        }
        let payload_buf = OxKvStore::<C>::encode_ops(&updates)?;
        self.maintenance_view()
            .append_wal_locked(payload_buf, &updates)
            .await?;
        drop(_gate);
        let subscribers = self.hooks.subscribers();
        let view = self.maintenance_view();
        for event in staged_events {
            crate::store::hooks::notify(&subscribers, &view, event).await;
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
