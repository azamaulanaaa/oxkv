//! The LSM store is generic over the [`Storage`] trait.
//! The store runs on native targets and on `wasm32`.

use std::sync::Arc;

#[cfg(test)]
use crate::store::storage::{MemStorage, Storage};

mod blob;
#[cfg(feature = "btree")]
mod cached;
mod manifest;
mod merge;
mod ownership;
mod probe;
pub(crate) mod read;
mod reader;
mod sst;
mod store;
mod tx;
mod wal;

pub(crate) use blob::{get_blob, try_decode_blob_pointer};
#[cfg(feature = "btree")]
pub use cached::{CachedOxKvStore, CachedTx, WarmMode};
pub(crate) use manifest::{Manifest, ManifestCache, load_manifest};
pub(crate) use ownership::read_ownership;
pub(crate) use read::{
    ReadCtx, filter_rows, is_not_found, point_lookup, range_lookup, retry_once_not_found,
};
pub use reader::{OxKvReader, OxKvRoTx};
/// Parsed SST file. Name it to weigh a custom [`crate::store::Cache`].
/// See [`SstFile::size`].
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
/// The store uses a counter, not wall time. The `std::time` clocks panic on
/// `wasm32`. Fencing safety comes from the monotonic epoch, not from session
/// uniqueness.
static SESSION_CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Number of WAL entries that trigger force flush and GC maintenance.
///
/// Each write appends one WAL id to `manifest.json` with a CAS. Without a
/// bound, the manifest grows without limit on small-write workloads. On such a
/// workload the 32 MiB SST threshold never fires. Every write then pays an
/// O(list) scan and a serde cost. Crossing this count force flushes an SST.
/// The store then GCs the covered WALs. This keeps the per-write manifest cost
/// flat.
const WAL_MAINTENANCE_COUNT: usize = 200;

/// Number of L1 files that trigger a bounding compaction. The compaction
/// merges the smallest adjacent pair. This keeps the SST list short. This also
/// keeps the scan in every read short.
const L1_MERGE_COUNT: usize = 16;

/// Maximum number of single-key writes that one group-commit batch fuses.
///
/// This value bounds the WAL file that a leader assembles. It also bounds the
/// time that followers wait. Write overflow stays queued for the next gate
/// holder. Write overflow does not stall behind one giant batch.
const MAX_GROUP_WRITES: usize = 256;

/// In-memory store helper for tests.
#[cfg(test)]
pub(crate) fn new_in_memory() -> Arc<dyn Storage> {
    Arc::new(MemStorage::new())
}

#[cfg(test)]
mod tests {
    use super::blob::encode_blob_pointer;
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
                // The store ignores conditional modes.
                // It overwrites unconditionally because it has no fencing
                // support.
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
                // The store ignores conditional modes.
                // It overwrites unconditionally because it has no fencing
                // support.
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

    /// Concurrent tasks share one store on a multi-threaded runtime. Blind
    /// writes, point reads, and per-task transaction commits interleave across
    /// threads. Distinct values per key catch cross-talk. The final sweep
    /// asserts that every write landed exactly once.
    // Threaded stress. The `wasm32-unknown-unknown` target is single-threaded.
    // The multi-thread scheduler feature does not compile on that target.
    // See `Cargo.toml`.
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

    /// Concurrent writes to one key never lose updates. Every task reports
    /// success. The final value is one of the written values.
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

    /// Writers released by a barrier fuse into fewer WAL files than the
    /// operations. At least one batch must carry two or more records.
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

    /// Dropped stores rebuild from multi-record WAL files. Concurrent writes
    /// replay exactly. This proves that batched WAL needs no format changes.
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

    /// The newest value wins across flush and compact cycles. A newer L0
    /// outranks older L1s. The manifest re-sort inside `compact` preserves that
    /// order. `compact` drains every L0 that it merges. `flush` appends new L0s
    /// after the sorted L1s. These two rules keep the reverse manifest order
    /// newest-first.
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
        // An old version of `mango` rides in one L0. A new version and a
        // smaller key ride in the next L0. The `min_key` order and the recency
        // order disagree.
        s.put_bytes("mango", b"v1").await.unwrap();
        s.put_bytes("zebra", b"v1").await.unwrap();
        s.flush_mem_to_sst_force().await.expect("flush");
        s.put_bytes("apple", b"x").await.unwrap();
        s.put_bytes("mango", b"v2").await.unwrap();
        s.flush_mem_to_sst_force().await.expect("flush");
        // Unmerged L0s lose nothing. The newest L0 outranks older files.
        assert_eq!(
            s.get_bytes("mango").await.unwrap().as_deref(),
            Some(b"v2".as_slice())
        );
        // Top up the L0s so that the final compact must merge and re-sort. Each
        // iteration nets one L0, because auto compacts fire only at four.
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

        // The point read path is cache-through. A miss inserts an entry. A hit
        // serves the entry.
        let got = s3.get_bytes("k1").await.unwrap().expect("k1");
        assert_eq!(got, b"v1");
        cache.run_pending_tasks().await;
        assert_eq!(cache.entry_count(), 1);
        let got = s3.get_bytes("k1").await.unwrap().expect("k1 again");
        assert_eq!(got, b"v1");

        // The scan path bypasses the cache. The scan path stays correct.
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

        // This is 2.5x the maintenance threshold. Without the force flush and
        // the GC drain, the list would hold every WAL id. The manifest cost
        // would be quadratic.
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

        // 100 rounds of disjoint ranges. Each L0-triggered compact adds one
        // L1. Without pair collapse, the count would reach 25. With pair
        // collapse, the count oscillates around the threshold once the
        // threshold is crossed (~64 rounds).
        for i in 0..100 {
            for j in 0..4 {
                s3.put_bytes(&format!("c{i:03}/k{j}"), b"v").await.unwrap();
            }
            // Tolerate `None`. WAL maintenance may have flushed this round's
            // memtable mid-round, after the WAL list reached its threshold.
            let _ = s3.flush_mem_to_sst_force().await.expect("sst flush");
            s3.compact().await.unwrap();
        }

        let path = ObjectPath::from("oxkv-l1-bound").child("manifest.json");
        let out = inner.get(&path).await.expect("manifest readable");
        let manifest: Manifest = serde_json::from_slice(&out.bytes).expect("manifest parses");
        let l1 = manifest.sst.iter().filter(|m| m.level == 1).count();
        // The lower bound proves that compactions ran. It does not report a
        // vacuous zero. The upper bound proves that pair collapse engaged.
        // Without pair collapse, the count would be 25.
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

        // This is 2.5x the maintenance threshold in single-write commits.
        // Without transaction-side maintenance, the WAL list would hold the id
        // of every commit. The memtable would never reach an SST.
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

        // For a fresh key, a blind write lands. An overwrite replaces the
        // value.
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
        // For parity, `set_bytes` on the same key reports the `put` value as
        // `prev`.
        let prev = s3.set_bytes("k1", b"v3").await.unwrap();
        assert_eq!(prev.as_deref(), Some(b"v2".as_slice()));

        // Transaction staging stays invisible until the commit, as `set_bytes`
        // does.
        let tx = s3.begin_tx().unwrap();
        tx.put_bytes("tk", b"tv").await.unwrap();
        assert_eq!(s3.get_bytes("tk").await.unwrap(), None);
        tx.commit().await.unwrap();
        assert_eq!(
            s3.get_bytes("tk").await.unwrap().as_deref(),
            Some(b"tv".as_slice())
        );
    }

    /// Counts `get_opts` calls for manifest polls.
    /// The struct delegates every other call.
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
        // Miss the `MemTable` so that every read reaches the manifest load.
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
        // Miss the `MemTable` so that every read reaches the manifest load.
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

    /// registered, because it has no handle to the writer's registry. The
    /// watermark was dead code that advertised a guarantee which nothing
    /// enforced.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn wal_gc_reclaims_wal_once_an_sst_covers_it() {
        let store = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(ObjectPath::from("gc-test"))
            .with_session("gc-sess")
            .build()
            .await
            .unwrap();

        s3.stage_set("k1", b"v1").await;
        s3.flush().await.unwrap();
        s3.stage_set("k2", b"v2").await;
        s3.flush_mem_to_sst_force().await.unwrap().expect("sst");

        let (before, _) = load_manifest(
            Arc::clone(&store),
            &ObjectPath::from("gc-test"),
            s3.epoch(),
            &s3.manifest_cache,
            std::time::Duration::from_secs(0),
        )
        .await
        .unwrap();
        assert!(!before.wal.is_empty(), "WAL listed before GC");

        let deleted = s3.gc_wal().await.unwrap();
        assert!(deleted > 0, "covered WAL should be collected");

        let (after, _) = load_manifest(
            Arc::clone(&store),
            &ObjectPath::from("gc-test"),
            s3.epoch(),
            &s3.manifest_cache,
            std::time::Duration::from_secs(0),
        )
        .await
        .unwrap();
        assert!(
            after.wal.is_empty(),
            "manifest.wal must be cleared after GC"
        );
        for wal in &before.wal {
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
        // The data stays readable through the SST after the WAL GC.
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
        // Create 4 L0 SSTs to trigger the compaction (>=4).
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
        // Verify the manifest state. It has no L0, or fewer L0s. It has one L1.
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
        // The data stays readable after the compaction. The newest value wins.
        assert_eq!(
            s3.get_bytes("k00").await.unwrap().as_deref(),
            Some(b"v0".as_slice())
        );
        assert_eq!(
            s3.get_bytes("common").await.unwrap().as_deref(),
            Some(b"2".as_slice())
        );
        // The second compaction should be a no-op. It should stay idempotent. It
        // must not create an extra L0.
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
        // The calls `Store::set`, `get`, and `delete` run directly against
        // persistent storage.
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
        // The transaction stays staged until the commit.
        let tx = s3.begin_tx().unwrap();
        tx.set_bytes("tx-k", b"tx-v").await.unwrap();
        assert_eq!(
            tx.get_bytes("tx-k").await.unwrap().as_deref(),
            Some(b"tx-v".as_slice())
        );
        // The write is not visible outside the transaction before the commit.
        assert_eq!(s3.get_bytes("tx-k").await.unwrap(), None);
        tx.commit().await.unwrap();
        assert_eq!(
            s3.get_bytes("tx-k").await.unwrap().as_deref(),
            Some(b"tx-v".as_slice())
        );
        // The call `gets` with limits and directions still works through the
        // heap merge.
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

    /// A user value shaped exactly like a blob pointer is user data, not a
    /// pointer.
    ///
    /// Regression detail: `try_decode_blob_pointer` used to accept any matching
    /// JSON object. Storing this value made the key permanently unreadable.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn blob_pointer_shaped_user_value_round_trips() {
        let s3 = OxKvStore::builder()
            .with_store(new_in_memory())
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        for payload in [
            &br#"{"blob":"nope","len":1,"crc":7}"#[..],
            &br#"{"crc":7,"len":1,"blob":"nope"}"#[..],
            &br#"{"blob":"x","len":1,"crc":2,"data":"user"}"#[..],
        ] {
            s3.set_bytes("k", payload).await.unwrap();
            assert_eq!(
                s3.get_bytes("k").await.unwrap().as_deref(),
                Some(payload),
                "user value must not be mistaken for a blob pointer"
            );
            // The value stays correct after it has passed through an SST.
            s3.flush_mem_to_sst_force().await.unwrap().unwrap();
            assert_eq!(
                s3.get_bytes("k").await.unwrap().as_deref(),
                Some(payload),
                "pointer shape must stay inert across the SST"
            );
        }
    }

    /// Real overflow still spills and dereferences through the tag.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn blob_pointer_tag_still_spills_and_dereferences() {
        let s3 = OxKvStore::builder()
            .with_store(new_in_memory())
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        let large = vec![b'x'; DEFAULT_BLOCK_SIZE];
        s3.stage_set("large", &large).await;
        s3.flush().await.unwrap();
        s3.flush_mem_to_sst_inner(true).await.unwrap().unwrap();
        assert_eq!(s3.get_bytes("large").await.unwrap(), Some(large));
        let encoded = encode_blob_pointer(&ObjectPath::from("e000001/blob/deadbeef"), 5, 9);
        assert!(try_decode_blob_pointer(&encoded).is_some());
        assert!(try_decode_blob_pointer(br#"{"blob":"x","len":1,"crc":2}"#).is_none());
    }

    /// A staged write that happens-before a durable write must win after a
    /// restart.
    ///
    /// Regression detail: the WAL sequence was allocated at flush time. A later
    /// `put_bytes` could take the *lower* sequence and win the replay.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn staged_write_before_durable_write_survives_restart() {
        let backend = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        s3.stage_set("k", b"v1").await;
        s3.put_bytes("k", b"v2").await.unwrap();
        s3.flush().await.unwrap();
        assert_eq!(
            s3.get_bytes("k").await.unwrap().as_deref(),
            Some(&b"v2"[..])
        );
        drop(s3);

        let reopened = OxKvStore::builder()
            .with_store(backend)
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        // `put_bytes` happened last, so `v2` is the durable truth.
        assert_eq!(
            reopened.get_bytes("k").await.unwrap().as_deref(),
            Some(&b"v2"[..]),
            "acknowledged write must not be reverted by replay"
        );
    }

    /// The same ordering rule applies across a delete.
    ///
    /// A staged write before the delete must not resurrect the key.
    /// A staged delete before a later put must not erase it.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn staged_ops_order_against_deletes_across_restart() {
        let backend = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        s3.put_bytes("gone", b"1").await.unwrap();
        // The write is staged first and the delete is second. The delete wins.
        s3.stage_set("gone", b"2").await;
        assert!(s3.delete("gone").await.unwrap());
        // The delete is first and the put is second. The put wins.
        s3.put_bytes("back", b"1").await.unwrap();
        assert!(s3.delete("back").await.unwrap());
        s3.stage_set("back", b"2").await;
        s3.flush().await.unwrap();
        drop(s3);

        let reopened = OxKvStore::builder()
            .with_store(backend)
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        assert_eq!(reopened.get_bytes("gone").await.unwrap(), None);
        assert_eq!(
            reopened.get_bytes("back").await.unwrap().as_deref(),
            Some(&b"2"[..])
        );
    }

    /// A transaction commit happens-after anything staged on the store.
    ///
    /// The staged record must take the lower WAL sequence.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn staged_write_before_tx_commit_survives_restart() {
        let backend = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        s3.stage_set("k", b"staged").await;
        let tx = s3.begin_tx().unwrap();
        tx.set_bytes("k", b"tx").await.unwrap();
        tx.commit().await.unwrap();
        drop(s3);

        let reopened = OxKvStore::builder()
            .with_store(backend)
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        assert_eq!(
            reopened.get_bytes("k").await.unwrap().as_deref(),
            Some(&b"tx"[..]),
            "tx commit happened last and must win the replay"
        );
    }

    /// A key rewritten while a flush is in flight keeps its newer value.
    ///
    /// The wrapper fires one `set_bytes` in the middle of the next SST upload.
    /// The flush snapshot and the write therefore overlap.
    struct Interleave {
        inner: MemStorage,
        store: std::sync::OnceLock<std::sync::Arc<OxKvStore>>,
        fired: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl Storage for Interleave {
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
            mode: PutMode,
        ) -> Result<PutOutcome> {
            if std::path::Path::new(path.as_str())
                .extension()
                .is_some_and(|ext| ext == "sst")
                && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst)
                && let Some(s3) = self.store.get()
            {
                // Overwrite `k` while the flush is between its snapshot and
                // its post-CAS memtable discard.
                s3.set_bytes("k", b"newer")
                    .await
                    .expect("interleaved write");
            }
            self.inner.put_opts(path, payload, mode).await
        }
        async fn delete(&self, path: &ObjectPath) -> Result<()> {
            self.inner.delete(path).await
        }
    }

    /// A key rewritten *while a flush is in flight* keeps its newer value.
    ///
    /// Regression detail: the flush discarded every key in its snapshot
    /// unconditionally. A write that landed between the snapshot and the
    /// post-CAS discard was dropped from the `MemTable`. The SST that the write
    /// targeted does not contain the write. The discard is a compare-and-remove.
    ///
    /// The test injects the interleaving at the SST upload. A sequential
    /// set/flush/set/flush sequence cannot reach it.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn flush_keeps_newer_write_that_overlapped_the_snapshot() {
        let backend = std::sync::Arc::new(Interleave {
            inner: MemStorage::new(),
            store: std::sync::OnceLock::new(),
            fired: std::sync::atomic::AtomicBool::new(false),
        });
        let s3 = std::sync::Arc::new(
            OxKvStore::builder()
                .with_store(std::sync::Arc::clone(&backend) as Arc<dyn Storage>)
                .skip_probe(true)
                .build()
                .await
                .unwrap(),
        );
        let _ = backend.store.set(std::sync::Arc::clone(&s3));
        s3.set_bytes("k", b"old").await.unwrap();
        // The call uses the ungated inner path. The public wrapper holds
        // `write_gate` across the flush. The injected write would deadlock on
        // that gate. The test therefore also confirms that the gate is held.
        s3.flush_mem_to_sst_inner(true).await.unwrap().unwrap();
        assert!(
            backend.fired.load(std::sync::atomic::Ordering::SeqCst),
            "the interleaved write never fired — this test proves nothing"
        );
        assert_eq!(
            s3.get_bytes("k").await.unwrap().as_deref(),
            Some(&b"newer"[..]),
            "flush discarded a value it never made durable"
        );
    }
    /// Every queued write is durable once `put_bytes` returns Ok, including
    /// the ones past `MAX_GROUP_WRITES` that no single batch could hold.
    ///
    /// Note on coverage: the test asserts the observable contract. It does not
    /// assert the `mine` check inside `put_bytes`. That branch is currently
    /// unreachable. `pending` is drained from the front under a fair FIFO gate.
    /// A leader is therefore always at the head of the queue it drains. Its own
    /// entry is therefore always inside the batch. An instrumented run (this
    /// many writers on 8 workers, released from a barrier) recorded zero
    /// re-queues. The test keeps the branch as defence in depth, not as a
    /// covered path.
    ///
    /// The test runs on native targets only, as its sibling test does. It spawns
    /// one task per writer. It needs a multi-threaded Tokio runtime. Neither
    /// exists on `wasm32`. Running the test under `wasm_bindgen_test` panicked
    /// with "there is no reactor running" and turned `wasm-pack test --node`
    /// red.
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn group_commit_never_acknowledges_another_batches_write() {
        use tokio::sync::Barrier;
        let backend = new_in_memory();
        let s3 = std::sync::Arc::new(
            OxKvStore::builder()
                .with_store(Arc::clone(&backend))
                .skip_probe(true)
                .build()
                .await
                .unwrap(),
        );
        // The test uses more writers than one capped batch can hold. It
        // releases all writers together. They pile up behind the gate before the
        // first leader drains.
        let total = MAX_GROUP_WRITES + 40;
        let barrier = Arc::new(Barrier::new(total));
        let mut handles = Vec::new();
        for i in 0..total {
            let s3 = std::sync::Arc::clone(&s3);
            let s3 = std::sync::Arc::clone(&s3);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                s3.put_bytes(&format!("k{i:04}"), format!("v{i}").as_bytes())
                    .await
            }));
        }
        for (i, h) in handles.into_iter().enumerate() {
            h.await
                .unwrap()
                .unwrap_or_else(|e| panic!("write {i} failed: {e}"));
        }
        // Every acknowledged write must be readable. The test checks it in
        // memory and after a restart.
        for i in 0..total {
            let key = format!("k{i:04}");
            assert_eq!(
                s3.get_bytes(&key).await.unwrap().as_deref(),
                Some(format!("v{i}").as_bytes()),
                "{key} acknowledged but not visible"
            );
        }
        drop(s3);
        let reopened = OxKvStore::builder()
            .with_store(backend)
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        for i in 0..total {
            let key = format!("k{i:04}");
            assert_eq!(
                reopened.get_bytes(&key).await.unwrap().as_deref(),
                Some(format!("v{i}").as_bytes()),
                "{key} acknowledged but lost on restart"
            );
        }
    }

    /// The manifest SST list is ordered oldest-first by write sequence. Every
    /// read walks the list in reverse. List position therefore *is* recency.
    ///
    /// Regression: compaction re-sorted the list by `min_key`. That sort can
    /// place a newer file ahead of older data and serve stale values.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn manifest_sst_list_stays_ordered_by_write_sequence() {
        let inner = new_in_memory();
        let s = OxKvStore::builder()
            .with_store(Arc::clone(&inner))
            .with_prefix(ObjectPath::from("oxkv-seq-order"))
            .with_session("sess-seq-order")
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        let read_manifest = || async {
            let out = inner
                .get(&ObjectPath::from("oxkv-seq-order").child("manifest.json"))
                .await
                .expect("manifest readable");
            serde_json::from_slice::<Manifest>(&out.bytes).expect("manifest parses")
        };

        // The test uses disjoint key ranges. Each compaction produces a
        // non-overlapping L1 that the next compaction *retains*. This is the
        // case where the list order carries information. After `L1_MERGE_COUNT`
        // files, compaction folds the smallest adjacent pair and rebuilds the
        // list.
        let mut saw_multi_file_list = false;
        for round in 0..(L1_MERGE_COUNT + 2) {
            for i in 0..4 {
                s.put_bytes(&format!("r{round:02}k{i}"), b"v1")
                    .await
                    .unwrap();
                s.flush_mem_to_sst_force().await.unwrap();
            }
            s.compact().await.unwrap();
            let m = read_manifest().await;
            if m.sst.len() > 1 {
                saw_multi_file_list = true;
            }
            assert!(
                m.sst.windows(2).all(|w| w[0].seq <= w[1].seq),
                "sst list not oldest-first by seq: {:?}",
                m.sst
                    .iter()
                    .map(|x| (x.id.as_str(), x.seq, x.level, x.min_key.as_str()))
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            saw_multi_file_list,
            "compaction never retained more than one file; test proves nothing"
        );

        // The newest value still wins, whatever order survived.
        for round in 0..(L1_MERGE_COUNT + 2) {
            s.put_bytes(&format!("r{round:02}k0"), b"v2").await.unwrap();
        }
        s.flush_mem_to_sst_force().await.unwrap();
        for round in 0..(L1_MERGE_COUNT + 2) {
            assert_eq!(
                s.get_bytes(&format!("r{round:02}k0"))
                    .await
                    .unwrap()
                    .as_deref(),
                Some(&b"v2"[..]),
                "r{round:02}k0"
            );
        }
    }

    /// Corrupting the blob object must surface as an error. The test does not
    /// accept a silent short or empty value.
    ///
    /// These tests are the only checks on spilled values. They had no coverage
    /// at all.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn blob_length_mismatch_surfaces_an_error() {
        let backend = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        let large = vec![b'x'; DEFAULT_BLOCK_SIZE];
        s3.stage_set("large", &large).await;
        s3.flush().await.unwrap();
        s3.flush_mem_to_sst_inner(true).await.unwrap().unwrap();
        assert_eq!(
            s3.get_bytes("large").await.unwrap().as_deref(),
            Some(&large[..])
        );

        // Truncate the spilled object in place.
        let hash = super::blob::blob_hash(&large);
        let path = super::blob::blob_path(s3.prefix(), s3.epoch(), &hash);
        let current = backend.get(&path).await.expect("blob object");
        backend.delete(&path).await.ok();
        backend
            .put_opts(
                &path,
                current.bytes[..current.bytes.len() - 1].to_vec(),
                PutMode::Create,
            )
            .await
            .expect("truncated blob must be writable");

        let err = s3
            .get_bytes("large")
            .await
            .expect_err("short blob must not read back");
        assert!(
            err.to_string().contains("blob len mismatch"),
            "unexpected error: {err}"
        );
    }

    /// Same, for a same-length corruption: only the CRC can catch it.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn blob_crc_mismatch_surfaces_an_error() {
        let backend = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        let large = vec![b'x'; DEFAULT_BLOCK_SIZE];
        s3.stage_set("large", &large).await;
        s3.flush().await.unwrap();
        s3.flush_mem_to_sst_inner(true).await.unwrap().unwrap();
        assert_eq!(
            s3.get_bytes("large").await.unwrap().as_deref(),
            Some(&large[..])
        );

        // The length is the same, but the bytes differ. The length check
        // passes. The CRC check must not pass.
        let hash = super::blob::blob_hash(&large);
        let path = super::blob::blob_path(s3.prefix(), s3.epoch(), &hash);
        let mut tampered = large.clone();
        tampered[0] = b'y';
        backend.delete(&path).await.ok();
        backend
            .put_opts(&path, tampered, PutMode::Create)
            .await
            .expect("tampered blob must be writable");

        let err = s3
            .get_bytes("large")
            .await
            .expect_err("corrupted blob must not read back");
        assert!(
            err.to_string().contains("blob crc mismatch"),
            "unexpected error: {err}"
        );
    }

    /// A spilled value must actually be spilled.
    ///
    /// A regression that inlined the value would pass every round-trip
    /// assertion.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn oversized_value_really_spills_to_a_blob_object() {
        let backend = new_in_memory();
        let s3 = OxKvStore::builder()
            .with_store(Arc::clone(&backend))
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        let large = vec![b'x'; DEFAULT_BLOCK_SIZE];
        s3.stage_set("large", &large).await;
        s3.flush().await.unwrap();
        s3.flush_mem_to_sst_inner(true).await.unwrap().unwrap();

        let hash = super::blob::blob_hash(&large);
        let path = super::blob::blob_path(s3.prefix(), s3.epoch(), &hash);
        let stored = backend
            .get(&path)
            .await
            .expect("value must have spilled to its own object");
        assert_eq!(stored.bytes, large);
    }

    /// `seq` orders the manifest SST list. The manifest is prefix-scoped. The
    /// manifest is inherited across an epoch takeover. Therefore `seq` must be
    /// a prefix-wide high-water mark, not a per-epoch mark.
    ///
    /// Regression: `sst_seq` was rebuilt from the SSTs of this epoch only. After
    /// a takeover, the SSTs of the new epoch got *lower* seqs than the
    /// inherited SSTs of the old epoch. Then `compact_inner`'s `sort_by_key(seq)`
    /// produced `[e1/seq4, e2/seq4, e1/seq9]`. That list placed the oldest data
    /// last. It inverted the "list position == recency" invariant that every
    /// read depends on.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn sst_seq_is_prefix_wide_across_an_epoch_takeover() {
        let backend = new_in_memory();
        let p = ObjectPath::from("seq-epoch");
        let mk = |backend: Arc<dyn Storage>, p: ObjectPath| async move {
            OxKvStore::builder()
                .with_store(backend)
                .with_prefix(p)
                .skip_probe(true)
                .build()
                .await
                .unwrap()
        };
        let epoch_of = |id: &str| -> u64 {
            id.split('/')
                .nth(1)
                .unwrap()
                .trim_start_matches('e')
                .parse()
                .unwrap()
        };

        // Epoch 1 burns sequences so that its retained L1s carry high seqs. The
        // `a*` keys never overlap the `x*` keys of epoch 2. Therefore these L1s
        // are retained.
        let a = mk(std::sync::Arc::clone(&backend), p.clone()).await;
        for i in 0..8 {
            a.set_bytes(&format!("a{i}"), b"old").await.unwrap();
            a.flush_mem_to_sst_force().await.unwrap().unwrap();
        }
        a.compact().await.unwrap();
        // The test drops the WALs of epoch 1 so that the open of epoch 2 does
        // not replay them into its memtable. Otherwise its first flush overlaps
        // the range of epoch 1. The L1s then merge legitimately. The ordering
        // question stays hidden.
        a.gc_wal().await.unwrap();
        drop(a);

        // Takeover: `sst_seq` restarts at 0 unless the store rebuilds it
        // prefix-wide.
        let b = mk(std::sync::Arc::clone(&backend), p.clone()).await;
        for i in 0..4 {
            b.set_bytes(&format!("x{i}"), b"new").await.unwrap();
            b.flush_mem_to_sst_force().await.unwrap().unwrap();
        }
        b.compact().await.unwrap();

        let out = backend
            .get(&p.child("manifest.json"))
            .await
            .expect("manifest");
        let m: Manifest = serde_json::from_slice(&out.bytes).unwrap();
        assert!(m.sst.len() > 1, "expected a mixed list, got {:?}", m.sst);
        let epochs: Vec<u64> = m.sst.iter().map(|s| epoch_of(&s.id)).collect();
        assert!(
            epochs.windows(2).all(|w| w[0] <= w[1]),
            "epoch order inverted in the sst list: {epochs:?} — reads walk this \
             list in reverse as newest-first, so the oldest file must come first"
        );
        // And the seqs themselves must be strictly ascending with the list.
        assert!(
            m.sst.windows(2).all(|w| w[0].seq < w[1].seq),
            "seq not monotonic across epochs: {:?}",
            m.sst
                .iter()
                .map(|s| (s.id.as_str(), s.seq))
                .collect::<Vec<_>>()
        );
        // Data written by the current epoch must still read back.
        assert_eq!(
            b.get_bytes("x0").await.unwrap().as_deref(),
            Some(&b"new"[..])
        );
        assert_eq!(
            b.get_bytes("a0").await.unwrap().as_deref(),
            Some(&b"old"[..])
        );
    }
}
