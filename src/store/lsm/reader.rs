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
    Manifest, ManifestCache, MemTable, ReadCtx, SstFile, decode_wal_records, filter_rows,
    is_not_found, load_manifest, point_lookup, range_lookup, read_ownership, replay_listed_wals,
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
    replayed: Arc<async_lock::Mutex<std::collections::BTreeSet<String>>>,
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
            replayed: Arc::new(async_lock::Mutex::new(std::collections::BTreeSet::new())),
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
        replay_listed_wals(&self.inner, &manifest.wal, &self.overlay).await;
        self.replayed
            .lock()
            .await
            .extend(manifest.wal.iter().cloned());
    }

    /// Replays WAL files listed since the last call into the overlay.
    ///
    /// Called at the top of every read, so post-open writer batches become
    /// visible without reopening. A fully collected WAL list means every
    /// record is SST-covered, so the overlay and the id set are dropped to
    /// keep a long-lived reader bounded to one WAL window.
    async fn ensure_replayed(&self) -> Result<()> {
        let (manifest, _etag) = load_manifest(
            Arc::clone(&self.inner),
            &self.prefix,
            self.epoch,
            &self.manifest_cache,
            std::time::Duration::from_secs(1),
        )
        .await?;
        if manifest.wal.is_empty() {
            self.replayed.lock().await.clear();
            self.overlay.write().await.clear();
            return Ok(());
        }
        let missing: Vec<String> = {
            let replayed = self.replayed.lock().await;
            manifest
                .wal
                .iter()
                .filter(|id| !replayed.contains(*id))
                .cloned()
                .collect()
        };
        for wal_id in missing {
            let path = ObjectPath::from(wal_id.as_str());
            let Ok(out) = self.inner.get(&path).await else {
                continue;
            };
            let mut replayed = self.replayed.lock().await;
            if replayed.contains(&wal_id) {
                continue;
            }
            let mut overlay = self.overlay.write().await;
            for (key, value) in decode_wal_records(&out.bytes) {
                overlay.insert(key, value);
            }
            replayed.insert(wal_id);
        }
        Ok(())
    }

    /// Reads `key` via the replay overlay → SSTs (newest first) → blob deref.
    ///
    /// Restarts once against a fresh manifest when an SST read hits `not found`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` on I/O or CRC failure.
    pub async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.ensure_replayed().await?;
        let staged = self.overlay.read().await.get(key).cloned();
        match point_lookup(&self.read_ctx(), staged, key).await {
            Err(e) if is_not_found(&e) => {
                self.manifest_cache.lock().await.clear();
                self.ensure_replayed().await?;
                let staged = self.overlay.read().await.get(key).cloned();
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
        self.ensure_replayed().await?;
        let layers = vec![{
            let overlay = self.overlay.read().await;
            filter_rows(&overlay, direction, &cursor)
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
                self.ensure_replayed().await?;
                let layers = vec![{
                    let overlay = self.overlay.read().await;
                    filter_rows(&overlay, direction, &cursor)
                }];
                range_lookup(&self.read_ctx(), layers, limit, direction, cursor).await
            }
            other => other,
        }
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
    use crate::store::storage::{GetOptions, GetOutput, PutMode, PutOutcome};

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

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn follows_post_open_writes() {
        let (store, backend, prefix) = writer().await;
        store.put_bytes("a", b"1").await.expect("put");
        let reader = OxKvReader::open(Arc::clone(&backend), prefix)
            .await
            .expect("open");
        store.put_bytes("b", b"2").await.expect("put");
        store.delete("a").await.expect("delete");
        assert_eq!(
            reader.get_bytes("b").await.expect("get"),
            Some(b"2".to_vec())
        );
        assert_eq!(reader.get_bytes("a").await.expect("get"), None);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn overlay_cleared_after_wal_gc() {
        let (store, backend, prefix) = writer().await;
        store.put_bytes("k", b"v").await.expect("put");
        let reader = OxKvReader::open(Arc::clone(&backend), prefix)
            .await
            .expect("open");
        assert!(!reader.overlay.read().await.is_empty());
        store.flush_mem_to_sst_force().await.expect("flush");
        store.gc_wal().await.expect("gc");
        assert_eq!(
            reader.get_bytes("k").await.expect("get"),
            Some(b"v".to_vec())
        );
        assert!(reader.overlay.read().await.is_empty());
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn reader_follows_epoch_failover() {
        let (writer1, backend, prefix) = writer().await;
        writer1.put_bytes("a", b"1").await.expect("put");
        let reader = OxKvReader::open(Arc::clone(&backend), prefix.clone())
            .await
            .expect("open");
        let writer2 = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .with_prefix(prefix)
            .skip_probe(true)
            .build()
            .await
            .expect("takeover");
        writer2.put_bytes("b", b"2").await.expect("put");
        assert_eq!(
            reader.get_bytes("b").await.expect("get"),
            Some(b"2".to_vec())
        );
        assert_eq!(
            reader.get_bytes("a").await.expect("get"),
            Some(b"1".to_vec())
        );
        assert!(matches!(
            writer1.put_bytes("x", b"y").await,
            Err(StoreError::Fenced(_))
        ));
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn concurrent_reader_opens() {
        let (store, backend, prefix) = writer().await;
        store.put_bytes("k", b"v").await.expect("put");
        let (r1, r2, r3) = tokio::join!(
            OxKvReader::open(Arc::clone(&backend), prefix.clone()),
            OxKvReader::open(Arc::clone(&backend), prefix.clone()),
            OxKvReader::open(Arc::clone(&backend), prefix.clone()),
        );
        for reader in [r1.expect("open"), r2.expect("open"), r3.expect("open")] {
            assert_eq!(
                reader.get_bytes("k").await.expect("get"),
                Some(b"v".to_vec())
            );
        }
        let epoch = read_ownership(Arc::clone(&backend), &prefix)
            .await
            .expect("ownership")
            .expect("present")
            .epoch;
        assert_eq!(epoch, 1);
    }

    struct FlakyOnce {
        inner: MemStorage,
        fail: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl Storage for FlakyOnce {
        async fn get(&self, path: &ObjectPath) -> Result<GetOutput> {
            let is_sst = std::path::Path::new(path.as_str())
                .extension()
                .is_some_and(|ext| ext == "sst");
            if is_sst && self.fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
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

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn reader_retries_stale_sst() {
        let flaky = Arc::new(FlakyOnce {
            inner: MemStorage::new(),
            fail: std::sync::atomic::AtomicBool::new(false),
        });
        let backend: Arc<dyn Storage> = flaky.clone();
        let store = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .with_prefix(ObjectPath::from("flaky"))
            .skip_probe(true)
            .build()
            .await
            .expect("build");
        store.put_bytes("k", b"v").await.expect("put");
        store.flush_mem_to_sst_force().await.expect("flush");
        store.gc_wal().await.expect("gc");
        let reader = OxKvReader::open(Arc::clone(&backend), ObjectPath::from("flaky"))
            .await
            .expect("open");
        flaky.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            reader.get_bytes("k").await.expect("get"),
            Some(b"v".to_vec())
        );
    }
}
