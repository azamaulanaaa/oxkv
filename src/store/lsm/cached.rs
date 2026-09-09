//! Write-through RAM mirror over [`OxKvStore`].
//!
//! [`CachedOxKvStore`] pairs a durable LSM core with a [`BTreeStore`] holding
//! every resolved key, so point and range reads serve from memory while writes
//! keep WAL durability. [`BTreeStore`] is imported for the mirror type only;
//! the durable behavior stays LSM.
//!
//! The mirror is authoritative under a single writer: standalone writes apply
//! to storage first and to memory second, and [`CachedTx`] applies its staged
//! overlay to memory only after a successful commit. Blind `put` paths
//! stay blind end to end — the previous value for `set` comes from the mirror
//! instead of an LSM read.
//!
//! A mirror can fall behind when another owner takes the epoch or writes
//! through a different handle. Freshness is derived from `manifest.json`
//! (`version` + `epoch` + SST set) via conditional polling; `ownership.json`
//! is read only when the manifest suggests a takeover, never per read. Plain
//! [`GetSet`] reads are always zero-I/O; the `*_checked` variants revalidate
//! against a time-to-live first. A write rejected with [`StoreError::Fenced`]
//! poisons the mirror: reads fail with the same fencing error until
//! [`refresh`](CachedOxKvStore::refresh) rebuilds from the new owner.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::read_ownership;
use super::{Manifest, ManifestCache, OxKvStore, OxKvTx, decode_wal_records, load_manifest};
use crate::store::cache::{Cache, LruCache};
use crate::store::storage::ObjectPath;
use crate::store::{
    BTreeStore, Direction, GetSet, KeyValue, Result, SstFile, Store, StoreError, Transaction,
    lock_ignore_poison, now_millis,
};

/// Batch size for the paged scans behind [`CachedOxKvStore::rebuild`].
const SCAN_BATCH: u32 = 256;

/// Staleness bound for [`CachedOxKvStore`] when none is configured.
///
/// Matches the manifest poll TTL so a default mirror converges on the same
/// envelope as an [`OxKvReader`](super::OxKvReader).
const DEFAULT_STALE_TTL: Duration = Duration::from_secs(1);

/// Long cache peek for post-write syncs: the entry was just published by our
/// own write, so any bound comfortably above zero hits without I/O.
const CACHE_PEEK_TTL: Duration = Duration::from_secs(3600);

/// Durability-first view of one mirror generation.
#[derive(Debug)]
struct CachedState {
    /// Manifest epoch the mirror was built from.
    epoch: u64,
    /// Manifest version the mirror reflects.
    version: u64,
    /// SST ids visible when the mirror was synced.
    sst_ids: BTreeSet<String>,
    /// WAL ids already applied to the mirror.
    replayed: BTreeSet<String>,
    /// Last freshness poll in millis.
    last_check_ms: u64,
    /// Fencing message while the owning writer is superseded.
    poisoned: Option<String>,
}

/// Records one manifest generation into `state`.
///
/// The ownership epoch is tracked separately: `manifest.epoch` records the
/// creation lineage and never moves on takeover, so it cannot detect one.
fn observe_manifest(state: &mut CachedState, manifest: &Manifest) {
    state.version = manifest.version;
    state.sst_ids = manifest.sst.iter().map(|meta| meta.id.clone()).collect();
    state.replayed.extend(manifest.wal.iter().cloned());
    state.last_check_ms = now_millis();
    state.poisoned = None;
}

/// Returns the SST id set of one manifest generation.
fn sst_ids(manifest: &Manifest) -> BTreeSet<String> {
    manifest.sst.iter().map(|meta| meta.id.clone()).collect()
}

/// Returns `true` when `ttl_ms` millis elapsed since `last_check_ms`.
fn ttl_expired(last_check_ms: u64, ttl_ms: u64) -> bool {
    now_millis().wrapping_sub(last_check_ms) >= ttl_ms
}

/// Returns `true` when `key` falls inside the `direction`/`cursor` window.
fn in_window(key: &str, direction: Direction, cursor: &(Option<String>, Option<String>)) -> bool {
    if direction == Direction::Prev && cursor.0.is_none() {
        return false;
    }
    let (lower, upper) = match direction {
        Direction::Next => (cursor.0.as_deref(), cursor.1.as_deref()),
        Direction::Prev => (cursor.1.as_deref(), cursor.0.as_deref()),
    };
    if let Some(lo) = lower
        && key < lo
    {
        return false;
    }
    if let Some(hi) = upper
        && key > hi
    {
        return false;
    }
    true
}

/// Write-through RAM mirror over an [`OxKvStore`].
///
/// Share with `Arc`: the mirror and the sync state are reference-counted
/// through clones of the inner handles, so one `Arc<CachedOxKvStore>` serves
/// every thread.
pub struct CachedOxKvStore<C = LruCache<String, Arc<SstFile>>> {
    inner: OxKvStore<C>,
    mirror: BTreeStore,
    state: Arc<async_lock::Mutex<CachedState>>,
    /// Staleness bound for the `*_checked` reads, in millis.
    ttl_ms: Arc<std::sync::atomic::AtomicU64>,
}

impl<C> std::fmt::Debug for CachedOxKvStore<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedOxKvStore").finish_non_exhaustive()
    }
}

/// Clones share the mirror and the sync state: the `BTreeStore` handle and
/// the generation tracker are reference-counted, so clones observe the same
/// keys. Required for decorator composition (`HookStore`, `OtelStore`).
impl<C> Clone for CachedOxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            mirror: self.mirror.clone(),
            state: Arc::clone(&self.state),
            ttl_ms: Arc::clone(&self.ttl_ms),
        }
    }
}

impl<C> CachedOxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    /// Opens a mirror over `inner`, warming every key into memory.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the bulk scan or the manifest load fails.
    pub async fn open(inner: OxKvStore<C>) -> Result<Self> {
        let mirror = BTreeStore::default();
        let store = Self {
            inner,
            mirror,
            state: Arc::new(async_lock::Mutex::new(CachedState {
                epoch: 0,
                version: 0,
                sst_ids: BTreeSet::new(),
                replayed: BTreeSet::new(),
                last_check_ms: 0,
                poisoned: None,
            })),
            ttl_ms: Arc::new(std::sync::atomic::AtomicU64::new(
                u64::try_from(DEFAULT_STALE_TTL.as_millis()).unwrap_or(u64::MAX),
            )),
        };
        store.rebuild().await?;
        store.state.lock().await.epoch = store.inner.epoch;
        Ok(store)
    }

    /// Sets the staleness bound for the `*_checked` reads.
    ///
    /// `Duration::ZERO` revalidates on every checked read; large bounds serve
    /// from memory until [`refresh`](Self::refresh) or [`check_stale`](Self::check_stale)
    /// is called explicitly.
    #[must_use]
    pub fn with_stale_ttl(self, ttl: Duration) -> Self {
        self.ttl_ms.store(
            u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
        self
    }

    /// Returns the durable core for `flush`, `compact`, and `gc_wal`.
    ///
    /// Mirror reads never touch these paths; maintenance issued here does not
    /// invalidate the mirror because compactions and collections preserve the
    /// logical key set.
    #[must_use]
    pub fn inner(&self) -> &OxKvStore<C> {
        &self.inner
    }

    /// Returns the ownership epoch the mirror was built from.
    pub async fn cached_epoch(&self) -> u64 {
        self.state.lock().await.epoch
    }

    /// Returns the manifest version the mirror reflects.
    pub async fn cached_version(&self) -> u64 {
        self.state.lock().await.version
    }

    /// Reports whether the mirror is poisoned by a fencing rejection.
    ///
    /// A poisoned mirror fails reads with [`StoreError::Fenced`] until
    /// [`refresh`](Self::refresh) adopts the new owner.
    pub async fn is_poisoned(&self) -> bool {
        self.state.lock().await.poisoned.is_some()
    }

    /// Fails with the fencing message while the mirror is poisoned.
    async fn reject_if_poisoned(&self) -> Result<()> {
        if let Some(reason) = self.state.lock().await.poisoned.clone() {
            return Err(StoreError::Fenced(reason));
        }
        Ok(())
    }

    /// Records a fencing rejection for subsequent reads.
    async fn poison(&self, reason: String) {
        self.state.lock().await.poisoned = Some(reason);
    }

    /// Poisons the mirror when `err` is a fencing rejection.
    async fn note_fenced(&self, err: &StoreError) {
        if let StoreError::Fenced(reason) = err {
            self.poison(reason.clone()).await;
        }
    }

    /// Pulls the just-published manifest generation into the sync state.
    ///
    /// Our own writes publish before returning, so a cache peek observes them
    /// without I/O and later refreshes skip re-applying our own WALs.
    async fn sync_after_write(&self) {
        let snapshot = {
            let guard = self.inner.manifest_cache.lock().await;
            guard
                .get_cached(CACHE_PEEK_TTL)
                .map(|(manifest, _)| manifest)
        };
        if let Some(manifest) = snapshot {
            let mut state = self.state.lock().await;
            observe_manifest(&mut state, &manifest);
        }
    }

    /// Revalidates when the staleness bound elapsed.
    async fn maybe_refresh(&self) -> Result<()> {
        let ttl_ms = self.ttl_ms.load(std::sync::atomic::Ordering::Relaxed);
        let expired = {
            let state = self.state.lock().await;
            state.poisoned.is_some() || ttl_expired(state.last_check_ms, ttl_ms)
        };
        if expired {
            self.refresh().await?;
        }
        Ok(())
    }

    /// Returns `true` when the mirror lags the durable core.
    ///
    /// Polls `manifest.json` conditionally and compares `version` and the SST
    /// set; `ownership.json` is not read here. A poisoned mirror always
    /// reports stale.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the manifest poll fails.
    pub async fn check_stale(&self) -> Result<bool> {
        if self.is_poisoned().await {
            return Ok(true);
        }
        let (manifest, _) = load_manifest(
            Arc::clone(&self.inner.inner),
            &self.inner.prefix,
            self.inner.epoch,
            &self.inner.manifest_cache,
            Duration::from_secs(0),
        )
        .await?;
        let stale = {
            let state = self.state.lock().await;
            manifest.version != state.version || sst_ids(&manifest) != state.sst_ids
        };
        if !stale {
            self.state.lock().await.last_check_ms = now_millis();
        }
        Ok(stale)
    }

    /// Applies every missing generation to the mirror.
    ///
    /// Same-owner WAL appends replay incrementally; a new ownership epoch or
    /// an SST set change (a flush or collection window this mirror missed)
    /// rebuilds from a full scan instead. Returns the number of key records
    /// applied.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the manifest, a WAL file, the ownership
    /// record, or the rebuild scan fails.
    pub async fn refresh(&self) -> Result<usize> {
        let known = {
            let state = self.state.lock().await;
            (
                state.epoch,
                state.version,
                state.sst_ids.clone(),
                state.replayed.clone(),
            )
        };
        let (manifest, _) = load_manifest(
            Arc::clone(&self.inner.inner),
            &self.inner.prefix,
            self.inner.epoch,
            &self.inner.manifest_cache,
            Duration::from_secs(0),
        )
        .await?;
        if manifest.version == known.1 && sst_ids(&manifest) == known.2 {
            self.state.lock().await.last_check_ms = now_millis();
            return Ok(0);
        }
        let owner = read_ownership(Arc::clone(&self.inner.inner), &self.inner.prefix).await?;
        let owner_epoch = owner.map_or(known.0, |record| record.epoch);
        if owner_epoch != known.0 || sst_ids(&manifest) != known.2 {
            let applied = self.rebuild().await?;
            let mut state = self.state.lock().await;
            state.epoch = owner_epoch;
            state.version = manifest.version;
            state.sst_ids = sst_ids(&manifest);
            state.replayed.extend(manifest.wal.iter().cloned());
            state.last_check_ms = now_millis();
            state.poisoned = None;
            return Ok(applied);
        }
        let missing: Vec<String> = manifest
            .wal
            .iter()
            .filter(|id| !known.3.contains(*id))
            .cloned()
            .collect();
        let applied = self.apply_wal_ids(&missing).await?;
        let mut state = self.state.lock().await;
        state.version = manifest.version;
        state.replayed.extend(manifest.wal.iter().cloned());
        state.last_check_ms = now_millis();
        state.poisoned = None;
        Ok(applied)
    }

    /// Applies listed WAL files to the mirror, skipping collected ones.
    async fn apply_wal_ids(&self, wal_ids: &[String]) -> Result<usize> {
        let mut applied = 0usize;
        for id in wal_ids {
            let path = ObjectPath::from(id.as_str());
            let bytes = match self.inner.inner.get(&path).await {
                Ok(out) => out.bytes,
                Err(e) if e.to_string().contains("not found") => continue,
                Err(e) => return Err(e),
            };
            for (key, value) in decode_wal_records(&bytes) {
                match value {
                    Some(raw) => self.mirror.put_bytes(&key, &raw).await?,
                    None => {
                        self.mirror.delete(&key).await?;
                    }
                }
                applied += 1;
            }
        }
        Ok(applied)
    }

    /// Rebuilds the mirror from a full scan of the durable core.
    ///
    /// Listed WAL files overlay the scan newest-wins: the scan only sees
    /// SSTs plus the local `MemTable`, so unflushed generations from another
    /// owner would otherwise be missed.
    async fn rebuild(&self) -> Result<usize> {
        let (manifest, _) = load_manifest(
            Arc::clone(&self.inner.inner),
            &self.inner.prefix,
            self.inner.epoch,
            &self.inner.manifest_cache,
            Duration::from_secs(0),
        )
        .await?;
        let mut scanned: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut cursor: Option<String> = None;
        loop {
            let batch = self
                .inner
                .gets_bytes(Some(SCAN_BATCH), Direction::Next, (cursor.clone(), None))
                .await?;
            if batch.is_empty() {
                break;
            }
            let last = batch.last().map(|kv| kv.key.clone());
            let short = batch.len() < SCAN_BATCH as usize;
            for kv in batch {
                if cursor.as_deref() == Some(kv.key.as_str()) {
                    continue;
                }
                scanned.insert(kv.key, kv.value);
            }
            cursor = last;
            if short {
                break;
            }
        }
        for id in &manifest.wal {
            let path = ObjectPath::from(id.as_str());
            let bytes = match self.inner.inner.get(&path).await {
                Ok(out) => out.bytes,
                Err(e) if e.to_string().contains("not found") => continue,
                Err(e) => return Err(e),
            };
            for (key, value) in decode_wal_records(&bytes) {
                match value {
                    Some(raw) => {
                        scanned.insert(key, raw);
                    }
                    None => {
                        scanned.remove(&key);
                    }
                }
            }
        }
        let current = self
            .mirror
            .gets_bytes(None, Direction::Next, (None, None))
            .await?;
        for kv in &current {
            if !scanned.contains_key(&kv.key) {
                self.mirror.delete(&kv.key).await?;
            }
        }
        for (key, value) in &scanned {
            self.mirror.put_bytes(key, value).await?;
        }
        let mut state = self.state.lock().await;
        observe_manifest(&mut state, &manifest);
        Ok(scanned.len())
    }

    /// Reads `key` from memory, revalidating first when the bound elapsed.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when revalidation fails or the mirror is poisoned.
    pub async fn get_bytes_checked(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.maybe_refresh().await?;
        self.get_bytes(key).await
    }

    /// Checks `key` in memory, revalidating first when the bound elapsed.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when revalidation fails or the mirror is poisoned.
    pub async fn has_checked(&self, key: &str) -> Result<bool> {
        self.maybe_refresh().await?;
        self.has(key).await
    }

    /// Scans memory in `direction`, revalidating first when the bound elapsed.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when revalidation fails or the mirror is poisoned.
    pub async fn gets_bytes_checked(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.maybe_refresh().await?;
        self.gets_bytes(limit, direction, cursor).await
    }
}

#[async_trait]
impl<C> GetSet for CachedOxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.reject_if_poisoned().await?;
        self.mirror.get_bytes(key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        self.reject_if_poisoned().await?;
        self.mirror.has(key).await
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        self.reject_if_poisoned().await?;
        match self.inner.delete(key).await {
            Err(e) => {
                self.note_fenced(&e).await;
                Err(e)
            }
            Ok(existed) => {
                if existed {
                    self.mirror.delete(key).await?;
                }
                self.sync_after_write().await;
                Ok(existed)
            }
        }
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        self.reject_if_poisoned().await?;
        let prev = self.mirror.get_bytes(key).await?;
        if let Err(e) = self.inner.put_bytes(key, value).await {
            self.note_fenced(&e).await;
            return Err(e);
        }
        self.mirror.put_bytes(key, value).await?;
        self.sync_after_write().await;
        Ok(prev)
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        self.reject_if_poisoned().await?;
        if let Err(e) = self.inner.put_bytes(key, value).await {
            self.note_fenced(&e).await;
            return Err(e);
        }
        self.mirror.put_bytes(key, value).await?;
        self.sync_after_write().await;
        Ok(())
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.reject_if_poisoned().await?;
        self.mirror.gets_bytes(limit, direction, cursor).await
    }
}

#[async_trait]
impl<C> Store for CachedOxKvStore<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    type Transaction = CachedTx<C>;

    fn begin_tx(&self) -> Result<Self::Transaction> {
        Ok(CachedTx {
            tx: self.inner.begin_tx()?,
            mirror: self.mirror.clone(),
            staged: std::sync::Mutex::new(BTreeMap::new()),
            state: Arc::clone(&self.state),
            manifest_cache: Arc::clone(&self.inner.manifest_cache),
        })
    }
}

/// Transaction over a [`CachedOxKvStore`].
///
/// Reads merge the staged overlay with the mirror, so no storage I/O happens
/// before commit; the overlay lands in memory only after the durable commit
/// succeeds.
pub struct CachedTx<C = LruCache<String, Arc<SstFile>>> {
    tx: OxKvTx<C>,
    mirror: BTreeStore,
    staged: std::sync::Mutex<BTreeMap<String, Option<Vec<u8>>>>,
    state: Arc<async_lock::Mutex<CachedState>>,
    manifest_cache: Arc<async_lock::Mutex<ManifestCache>>,
}

impl<C> std::fmt::Debug for CachedTx<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedTx").finish_non_exhaustive()
    }
}

#[async_trait]
impl<C> GetSet for CachedTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if let Some(staged) = lock_ignore_poison(&self.staged).get(key) {
            return Ok(staged.clone());
        }
        self.mirror.get_bytes(key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        if let Some(staged) = lock_ignore_poison(&self.staged).get(key) {
            return Ok(staged.is_some());
        }
        self.mirror.has(key).await
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        if let Some(None) = lock_ignore_poison(&self.staged).get(key) {
            return Ok(false);
        }
        let existed = self.tx.delete(key).await?;
        if existed {
            lock_ignore_poison(&self.staged).insert(key.to_string(), None);
        }
        Ok(existed)
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = self.get_bytes(key).await?;
        self.tx.put_bytes(key, value).await?;
        lock_ignore_poison(&self.staged).insert(key.to_string(), Some(value.to_vec()));
        Ok(prev)
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        self.tx.put_bytes(key, value).await?;
        lock_ignore_poison(&self.staged).insert(key.to_string(), Some(value.to_vec()));
        Ok(())
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        let mut rows = self
            .mirror
            .gets_bytes(None, direction, cursor.clone())
            .await?;
        let staged = lock_ignore_poison(&self.staged).clone();
        for (key, value) in &staged {
            if !in_window(key, direction, &cursor) {
                continue;
            }
            rows.retain(|kv| kv.key != *key);
            if let Some(raw) = value {
                rows.push(KeyValue {
                    key: key.clone(),
                    value: raw.clone(),
                });
            }
        }
        match direction {
            Direction::Next => rows.sort_by(|a, b| a.key.cmp(&b.key)),
            Direction::Prev => rows.sort_by(|a, b| b.key.cmp(&a.key)),
        }
        if let Some(bound) = limit {
            rows.truncate(bound as usize);
        }
        Ok(rows)
    }
}

#[async_trait]
impl<C> Transaction for CachedTx<C>
where
    C: Cache<String, Arc<SstFile>>,
{
    async fn commit(self) -> Result<()> {
        let Self {
            tx,
            mirror,
            staged,
            state,
            manifest_cache,
        } = self;
        let staged = lock_ignore_poison(&staged).clone();
        if let Err(e) = tx.commit().await {
            if let StoreError::Fenced(reason) = &e {
                state.lock().await.poisoned = Some(reason.clone());
            }
            return Err(e);
        }
        for (key, value) in &staged {
            match value {
                Some(raw) => mirror.put_bytes(key, raw).await?,
                None => {
                    mirror.delete(key).await?;
                }
            }
        }
        let snapshot = {
            let guard = manifest_cache.lock().await;
            guard
                .get_cached(CACHE_PEEK_TTL)
                .map(|(manifest, _)| manifest)
        };
        if let Some(manifest) = snapshot {
            let mut guard = state.lock().await;
            observe_manifest(&mut guard, &manifest);
        }
        Ok(())
    }

    async fn rollback(self) -> Result<()> {
        self.tx.rollback().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStorage;
    use crate::store::storage::Storage;
    use crate::store::{HookStore, Validator};

    async fn writer(prefix: &str) -> (OxKvStore, Arc<dyn Storage>) {
        let backend: Arc<dyn Storage> = Arc::new(MemStorage::new());
        let store = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .with_prefix(ObjectPath::from(prefix))
            .skip_probe(true)
            .build()
            .await
            .expect("build");
        (store, backend)
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn open_warms_existing_keys() {
        let (writer, _) = writer("cached-warm").await;
        writer.put_bytes("a", b"1").await.expect("put");
        writer.put_bytes("b", b"2").await.expect("put");
        let cached = CachedOxKvStore::open(writer).await.expect("open");
        assert_eq!(
            cached.get_bytes("a").await.expect("get"),
            Some(b"1".to_vec())
        );
        assert!(cached.has("b").await.expect("has"));
        assert!(!cached.check_stale().await.expect("fresh"));
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn range_reads_serve_both_directions() {
        let (writer, _) = writer("cached-range").await;
        for key in ["a", "b", "c"] {
            writer.put_bytes(key, b"v").await.expect("put");
        }
        let cached = CachedOxKvStore::open(writer).await.expect("open");
        let next = cached
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .expect("scan");
        assert_eq!(next.len(), 3);
        let prev = cached
            .gets_bytes(None, Direction::Prev, (Some("c".to_string()), None))
            .await
            .expect("scan");
        let keys: Vec<&str> = prev.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, ["c", "b", "a"]);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn direct_writes_flow_through_to_storage() {
        let (writer, _) = writer("cached-write").await;
        let cached = CachedOxKvStore::open(writer).await.expect("open");
        assert_eq!(cached.set_bytes("k", b"v1").await.expect("set"), None);
        assert_eq!(
            cached.set_bytes("k", b"v2").await.expect("set"),
            Some(b"v1".to_vec())
        );
        cached.put_bytes("blind", b"b").await.expect("put");
        assert!(cached.delete("blind").await.expect("delete"));
        assert!(!cached.delete("blind").await.expect("delete"));
        let inner = cached.inner();
        assert_eq!(
            inner.get_bytes("k").await.expect("get"),
            Some(b"v2".to_vec())
        );
        assert_eq!(inner.get_bytes("blind").await.expect("get"), None);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn checked_reads_follow_takeover() {
        let (writer, backend) = writer("cached-takeover").await;
        writer.put_bytes("a", b"1").await.expect("put");
        let cached = CachedOxKvStore::open(writer).await.expect("open");
        let before = cached.cached_epoch().await;
        let rival = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .with_prefix(ObjectPath::from("cached-takeover"))
            .skip_probe(true)
            .build()
            .await
            .expect("takeover");
        rival.put_bytes("b", b"2").await.expect("put");
        assert!(cached.check_stale().await.expect("stale"));
        let applied = cached.refresh().await.expect("refresh");
        assert!(applied >= 1);
        assert!(cached.cached_epoch().await > before);
        assert_eq!(
            cached.get_bytes_checked("b").await.expect("get"),
            Some(b"2".to_vec())
        );
        assert_eq!(
            cached.get_bytes("a").await.expect("get"),
            Some(b"1".to_vec())
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn fenced_write_poisons_until_refresh() {
        let (writer, backend) = writer("cached-fence").await;
        writer.put_bytes("a", b"1").await.expect("put");
        let cached = CachedOxKvStore::open(writer).await.expect("open");
        let rival = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .with_prefix(ObjectPath::from("cached-fence"))
            .skip_probe(true)
            .build()
            .await
            .expect("takeover");
        rival.put_bytes("b", b"2").await.expect("put");
        assert!(matches!(
            cached.put_bytes("x", b"y").await,
            Err(StoreError::Fenced(_))
        ));
        assert!(cached.is_poisoned().await);
        assert!(matches!(
            cached.get_bytes("a").await,
            Err(StoreError::Fenced(_))
        ));
        cached.refresh().await.expect("refresh");
        assert!(!cached.is_poisoned().await);
        assert_eq!(
            cached.get_bytes("b").await.expect("get"),
            Some(b"2".to_vec())
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn tx_commit_applies_and_rollback_discards() {
        let (writer, _) = writer("cached-tx").await;
        writer.put_bytes("keep", b"0").await.expect("put");
        let cached = CachedOxKvStore::open(writer).await.expect("open");
        let tx = cached.begin_tx().expect("tx");
        tx.set_bytes(" staged", b"1").await.expect("stage");
        tx.put_bytes("keep", b"9").await.expect("stage");
        tx.delete("missing").await.expect("delete");
        assert_eq!(cached.get_bytes(" staged").await.expect("get"), None);
        tx.commit().await.expect("commit");
        assert_eq!(
            cached.get_bytes(" staged").await.expect("get"),
            Some(b"1".to_vec())
        );
        assert_eq!(
            cached.inner().get_bytes("keep").await.expect("get"),
            Some(b"9".to_vec())
        );
        let tx = cached.begin_tx().expect("tx");
        tx.set_bytes("ghost", b"g").await.expect("stage");
        tx.delete("keep").await.expect("stage");
        tx.rollback().await.expect("rollback");
        assert_eq!(cached.get_bytes("ghost").await.expect("get"), None);
        assert_eq!(
            cached.get_bytes("keep").await.expect("get"),
            Some(b"9".to_vec())
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn build_cached_warms_and_serves() {
        let backend: Arc<dyn Storage> = Arc::new(MemStorage::new());
        let seed = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .with_prefix(ObjectPath::from("cached-builder"))
            .skip_probe(true)
            .build()
            .await
            .expect("seed");
        seed.put_bytes("k", b"v").await.expect("put");
        drop(seed);
        let cached = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .with_prefix(ObjectPath::from("cached-builder"))
            .skip_probe(true)
            .build_cached()
            .await
            .expect("build_cached");
        assert_eq!(
            cached.get_bytes("k").await.expect("get"),
            Some(b"v".to_vec())
        );
        let small = LruCache::new(1024, |_: &String, v: &Arc<SstFile>| {
            u32::try_from(v.size()).unwrap_or(u32::MAX)
        });
        let custom = OxKvStore::builder()
            .with_store(backend)
            .with_prefix(ObjectPath::from("cached-builder-custom"))
            .skip_probe(true)
            .build_cached_with_cache(small)
            .await
            .expect("build_cached_with_cache");
        custom.put_bytes("j", b"w").await.expect("put");
        assert_eq!(
            custom.get_bytes("j").await.expect("get"),
            Some(b"w".to_vec())
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn composes_with_hook_decorator() {
        struct RequireJson;

        #[async_trait::async_trait]
        impl Validator for RequireJson {
            async fn validate(
                &self,
                _ctx: &dyn crate::store::StoreView,
                _key: &str,
                value: &[u8],
            ) -> Result<()> {
                serde_json::from_slice::<serde_json::Value>(value)
                    .map(|_| ())
                    .map_err(|e| StoreError::Other(e.to_string()))
            }
        }

        let (writer, _) = writer("cached-hooks").await;
        let cached = CachedOxKvStore::open(writer).await.expect("open");
        let hooked = HookStore::new(cached.clone()).with_validator(RequireJson);
        assert!(hooked.set_bytes("raw", b"nope").await.is_err());
        hooked
            .set_bytes("doc", br#"{"ok":true}"#)
            .await
            .expect("valid");
        assert_eq!(
            cached.get_bytes("doc").await.expect("get"),
            Some(br#"{"ok":true}"#.to_vec())
        );
        let scoped = HookStore::new(cached).with_validator(RequireJson);
        let _ = scoped.watch_all();
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn tx_reads_merge_staged_with_mirror() {
        let (writer, _) = writer("cached-tx-read").await;
        for key in ["a", "b", "c"] {
            writer.put_bytes(key, b"v").await.expect("put");
        }
        let cached = CachedOxKvStore::open(writer).await.expect("open");
        let tx = cached.begin_tx().expect("tx");
        tx.delete("b").await.expect("stage");
        tx.set_bytes("d", b"new").await.expect("stage");
        assert_eq!(tx.get_bytes("b").await.expect("get"), None);
        assert_eq!(tx.get_bytes("d").await.expect("get"), Some(b"new".to_vec()));
        let rows = tx
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .expect("scan");
        let keys: Vec<&str> = rows.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, ["a", "c", "d"]);
        tx.rollback().await.expect("rollback");
    }
}
