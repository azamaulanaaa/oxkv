//! LSM store generic over the [`Storage`] trait — native and `wasm32`.

use std::sync::Arc;

#[cfg(test)]
use crate::store::storage::{MemStorage, Storage};

mod blob;
mod cached;
mod manifest;
mod merge;
mod ownership;
mod probe;
mod read;
mod reader;
mod sst;
mod store;
mod tx;
mod wal;

pub(crate) use blob::{get_blob, try_decode_blob_pointer};
pub use cached::{CachedOxKvStore, CachedTx, WarmMode};
pub(crate) use manifest::{Manifest, ManifestCache, load_manifest};
pub(crate) use ownership::read_ownership;
pub(crate) use read::{ReadCtx, filter_rows, is_not_found, point_lookup, range_lookup};
pub use reader::{OxKvReader, OxKvRoTx};
/// Parsed SST file; name it to weigh a custom [`Cache`] (see [`SstFile::size`]).
pub use sst::SstFile;
pub(crate) use sst::TOMBSTONE_VLEN;
pub use store::{OxKvStore, OxKvStoreBuilder};
pub use tx::OxKvTx;
pub(crate) use wal::{decode_wal_records, replay_listed_wals};

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

/// In-memory store helper for tests.
#[cfg(test)]
pub(crate) fn new_in_memory() -> Arc<dyn Storage> {
    Arc::new(MemStorage::new())
}

#[cfg(test)]
mod tests {
    use super::ownership::{acquire_ownership, sst_path, wal_path};
    use super::probe::probe_store;
    use super::sst::DEFAULT_BLOCK_SIZE;
    use super::*;
    use crate::store::storage::{
        GetOptions, GetOutput, MemStorage, ObjectPath, ObjectVersion, PutMode, PutOutcome, Storage,
    };
    use crate::store::{Direction, GetSet, Result, Store, StoreError, Transaction};

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
