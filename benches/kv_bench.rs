#![allow(
    missing_docs,
    clippy::expect_used,
    clippy::unwrap_used,
    dead_code,
    unused_variables
)]

//! Benchmarks for storing and manipulating key-value data at 1,000-item,
//! 100,000-item, and 1,000,000-item scales across all shipped backends.
//!
//! Run every benchmark with `cargo bench`. Run a single group with
//! `cargo bench -- seq_insert`. Tune wall-clock time with the standard flags of
//! `criterion`, for example `cargo bench -- --sample-count 10 --measurement-time 5`.
//!
//! Always run through `cargo bench` because that command passes `--bench`, which
//! selects measurement mode. Invoking the compiled binary directly runs each routine
//! exactly once as a smoke test. That run is `criterion` test mode. It records
//! nothing.
//!
//! Workloads:
//! - `seq_insert`: sequential insertion of every key. This is the dominant write path.
//! - `random_get`: point reads in a prime-stride permutation order.
//! - `page_fetch_100`: one paginated range fetch of 100 entries.
//! - `tx_commit_batch_1000`: commit of a pre-staged 1,000-write transaction. Staging
//!   happens in untimed setup. The number therefore measures durability cost.
//! - `seq_delete`: deletion of every key from a freshly populated store.
//! - `point_update`: in-place updates of a few keys inside a large store.
//! - `write_stage` / `write_wal_put` / `write_flush_check`: write-path breakdown
//!   (oxkv only). These groups cover staged-only, raw storage, and flush-check costs.
//!   They isolate the stages of a durable write.
//! - `concurrent_write`: 8 threads x 128 blind writes against one shared store on a
//!   multi-thread runtime (oxkv only). This group exercises the write gate.
//! - `zipf_get`: end-to-end skewed reads over 50 forced SSTs with a 320 KiB cache
//!   (oxkv only). This group covers file-level caching with real SST parse costs and
//!   compaction interplay. The hit ratio prints to stderr. This group guards the
//!   `fetch_sst` wiring that the `cache_zipf` micro-benchmark cannot see. For an
//!   isolated policy A/B, see `cache_bench`.
//! - `concurrent_random_get`: shared-store point reads from 8 threads (oxkv only).
//!
//! A/B comparisons run against a base commit. The baselines live in the gitignored
//! `target/criterion` directory, so they never leave your machine. Run
//! `mise bench --bench kv_bench -- --save-baseline base` on the base commit.
//! Then run `mise bench --bench kv_bench -- --baseline base` on the contender.
//!
//! The scale strategy keeps the full suite in minutes, not hours:
//! - Full-scan writes (`seq_insert`, `seq_delete`) scale linearly. They therefore
//!   run at 1K and 100K only. A 1M store would cost 1M writes *per iteration* (times
//!   samples, times backends). Linear scaling supplies no signal beyond itself.
//! - Depth is still tested at 1M through `random_get`, `page_fetch_100`, and
//!   `point_update`. Their per-iteration work is bounded: a capped read sample, one
//!   page, or a few updates. They run against a 1M-key store that is built once and
//!   then reused. Tree depth, SST levels, and index size are the same as in a full
//!   1M scan. Only the repeated per-iteration cost is removed.
//! - Large-store groups use fewer samples with short warmup and measurement windows
//!   (see `configure`).
//!
//! The `OxKv` backend (feature `oxkv`) uses `MemStorage` with `skip_probe(true)`.
//! The numbers are therefore comparable to `btree_mem` and `oxkv_mem` without
//! network I/O. Each store instance uses a unique prefix to avoid ownership fencing
//! within the same `MemStorage` bucket.
//!
//! `cached_mem` is the same durable core wrapped by `CachedOxKvStore`
//! (`build_cached`). Every write mirrors into RAM. Therefore only the read shapes
//! (`random_get`, `page_fetch`) are measured. Writes share the durable path of
//! `oxkv_mem` plus one B-tree insert each. The other groups cover writes.

use std::hint::black_box;
#[cfg(feature = "oxkv")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
#[cfg(feature = "btree")]
use oxkv::BTreeStore;
#[cfg(feature = "oxkv")]
use oxkv::OxKvStore;
use oxkv::{Direction, GetSet, Store, Transaction};

const SMALL: usize = 1_000;
const MEDIUM: usize = 100_000;
const LARGE: usize = 1_000_000;
const DELETE_CAP: usize = 100_000;
const TX_BATCH: usize = 1_000;
/// Point reads measured per iteration against a LARGE store. The store still
/// holds 1M keys. Depth is therefore preserved. Only the repeated work is capped.
/// One iteration then costs 10K gets instead of 1M.
const READ_SAMPLES: usize = 10_000;
const PAGE: u32 = 100;
const PAYLOAD: [u8; 64] = [b'x'; 64];
/// Prime stride used for deterministic pseudo-random key selection.
const STRIDE: usize = 7919;

fn key(i: usize) -> String {
    format!("key:{i:07}")
}

/// Deterministic permutation of `0..n` via a prime stride. The stride is coprime
/// with both benchmark sizes. Random access is therefore reproducible without a
/// `rand` dependency.
fn shuffled(n: usize) -> Vec<usize> {
    (0..n).map(|i| (i * STRIDE) % n).collect()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio current-thread runtime")
}

async fn populate<S: GetSet>(store: &mut S, keys: &[String]) {
    for k in keys {
        store.set_bytes(k, &PAYLOAD).await.expect("populate set");
    }
}

/// Tune sampling by *store size* while reporting throughput by *ops per
/// iteration*. The two measures differ for sampled reads at LARGE scale.
fn configure(group: &mut criterion::BenchmarkGroup<'_, WallTime>, store: usize, ops: usize) {
    group.throughput(Throughput::Elements(
        ops.try_into().expect("element count fits u64"),
    ));
    if store >= LARGE {
        // The one-time 1M populate is kept. Repeated per-iteration work is cheap
        // (sampled reads, one page, or a few updates), so a short window suffices.
        group.sample_size(10);
        group.warm_up_time(Duration::from_secs(2));
        group.measurement_time(Duration::from_secs(10));
    } else if store >= MEDIUM || ops >= MEDIUM {
        // 100K full-scan writes and reads: each iteration takes seconds. The sample
        // count and the window therefore stay small. Effect sizes here dwarf
        // sampling noise.
        group.sample_size(10);
        group.warm_up_time(Duration::from_secs(2));
        group.measurement_time(Duration::from_secs(5));
    }
}

fn seq_insert<S>(rt: &tokio::runtime::Runtime, c: &mut Criterion, backend: &str, n: usize)
where
    S: GetSet + Store + Default,
{
    let mut group = c.benchmark_group(format!("seq_insert/{backend}/{n}"));
    configure(&mut group, n, n);
    let keys: Vec<String> = (0..n).map(key).collect();

    group.bench_function("store", |b| {
        b.iter(|| {
            rt.block_on(async {
                let store = S::default();
                for k in &keys {
                    black_box(store.set_bytes(k, &PAYLOAD).await.expect("set"));
                }
            });
        });
    });

    group.finish();
}

fn random_get<S>(rt: &tokio::runtime::Runtime, c: &mut Criterion, backend: &str, n: usize)
where
    S: GetSet + Default,
{
    let mut group = c.benchmark_group(format!("random_get/{backend}/{n}"));
    // At LARGE scale the store holds 1M keys. Each iteration samples
    // `READ_SAMPLES` gets, which keeps depth and bounds repeated work.
    let take = if n >= LARGE { READ_SAMPLES } else { n };
    configure(&mut group, n, take);
    let keys: Vec<String> = (0..n).map(key).collect();
    let order: Vec<usize> = shuffled(n);

    // The store populates lazily on the first (untimed warmup) iteration. A
    // benchmark that the filter removes then pays no setup cost.
    let mut store: Option<S> = None;

    group.bench_function("get", |b| {
        b.iter(|| {
            let s = store.get_or_insert_with(|| {
                let mut s = S::default();
                rt.block_on(populate(&mut s, &keys));
                s
            });
            rt.block_on(async {
                for i in order.iter().take(take) {
                    black_box(s.get_bytes(&keys[*i]).await.expect("get"));
                }
            });
        });
    });

    group.finish();
}

fn page_fetch<S>(rt: &tokio::runtime::Runtime, c: &mut Criterion, backend: &str, n: usize)
where
    S: GetSet + Default,
{
    let mut group = c.benchmark_group(format!("page_fetch_{PAGE}/{backend}/{n}"));
    configure(
        &mut group,
        n,
        usize::try_from(PAGE).expect("page size fits"),
    );
    let keys: Vec<String> = (0..n).map(key).collect();

    // The store populates lazily on the first (untimed warmup) iteration. A
    // benchmark that the filter removes then pays no setup cost.
    let mut store: Option<S> = None;

    // Rotate through distinct start cursors. The benchmark then does not always
    // read the same page. Cached tree paths do not make the numbers look better
    // than they are.
    let starts: Vec<String> = (0..128usize).map(|j| key((j * 7919 + n / 2) % n)).collect();

    group.bench_function("fetch", |b| {
        let mut j = 0usize;
        b.iter(|| {
            let start = starts[j % starts.len()].clone();
            j += 1;
            let s = store.get_or_insert_with(|| {
                let mut s = S::default();
                rt.block_on(populate(&mut s, &keys));
                s
            });
            rt.block_on(async {
                black_box(
                    s.gets_bytes(Some(PAGE), Direction::Next, (Some(start), None))
                        .await
                        .expect("gets_bytes"),
                );
            });
        });
    });

    group.finish();
}

fn tx_commit_batch<S>(rt: &tokio::runtime::Runtime, c: &mut Criterion, backend: &str)
where
    S: Store + Default,
    S::Transaction: Transaction + GetSet,
{
    const NAME: &str = "tx_commit_batch_1000";
    let mut group = c.benchmark_group(format!("{NAME}/{backend}"));
    configure(&mut group, TX_BATCH, TX_BATCH);
    let keys: Vec<String> = (0..TX_BATCH).map(key).collect();

    // Staging runs in untimed setup. The measured section covers only the commit,
    // which is the cost of making a batch durable.
    group.bench_function("commit", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let store = S::default();
                    let tx = store.begin_tx().expect("begin_tx");
                    for k in &keys {
                        tx.set_bytes(k, &PAYLOAD).await.expect("stage set");
                    }
                    tx
                })
            },
            |tx| {
                rt.block_on(async {
                    tx.commit().await.expect("commit");
                });
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

fn seq_delete<S>(rt: &tokio::runtime::Runtime, c: &mut Criterion, backend: &str, requested: usize)
where
    S: GetSet + Default,
{
    let n = requested.min(DELETE_CAP);
    let mut group = c.benchmark_group(format!("seq_delete/{backend}/{n}"));
    configure(&mut group, n, n);
    let keys: Vec<String> = (0..n).map(key).collect();

    group.bench_function("delete", |b| {
        b.iter_batched(
            || {
                let mut store = S::default();
                rt.block_on(populate(&mut store, &keys));
                store
            },
            |store| {
                rt.block_on(async {
                    for k in &keys {
                        black_box(store.delete(k).await.expect("delete"));
                    }
                });
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

/// Manipulation workload: apply `m` in-place updates (`set_bytes` over
/// existing keys) against a store that holds `n` items.
///
/// The store builds once and is then reused. Updates never change the key set.
/// Iterations therefore stay comparable without a rebuild. Each iteration rotates
/// to a different subset of keys. The benchmark therefore never measures the same
/// tree region every time.
/// Matrix: 1,000-item stores take 1 change and 10 changes. 1,000,000-item stores
/// take 1, 100, and 1,000 changes.
fn point_update<S>(
    runtime: &tokio::runtime::Runtime,
    crit: &mut Criterion,
    backend: &str,
    items: usize,
    changes: usize,
) where
    S: GetSet + Default,
{
    let mut group = crit.benchmark_group(format!(
        "point_update/{backend}/{items}items_{changes}changes"
    ));
    configure(&mut group, items, changes);
    let keys: Vec<String> = (0..items).map(key).collect();

    // The store populates lazily on the first (untimed warmup) iteration. A
    // benchmark that the filter removes then pays no setup cost.
    let mut store: Option<S> = None;

    group.bench_function("update", |b| {
        let mut j = 0usize;
        b.iter(|| {
            let base = (j * changes) % items;
            j += 1;
            let s = store.get_or_insert_with(|| {
                let mut s = S::default();
                runtime.block_on(populate(&mut s, &keys));
                s
            });
            runtime.block_on(async {
                for i in 0..changes {
                    let k = &keys[(base + i * STRIDE) % items];
                    black_box(s.set_bytes(k, &PAYLOAD).await.expect("update"));
                }
            });
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// OxKv backend (`MemStorage`, `skip_probe`). The workload shapes match the
// groups above. The helpers differ, because `OxKvStore` does not implement
// `Default` and requires an async builder.
// ---------------------------------------------------------------------------
#[cfg(feature = "oxkv")]
#[allow(clippy::wildcard_imports)]
mod oxkv_bench {
    use std::sync::Arc;

    use oxkv::{LruCache, MemStorage, ObjectPath, PutMode, SstFile, Storage};

    use super::*;

    static OXKV_CTR: AtomicUsize = AtomicUsize::new(0);
    /// `zipf_get` counters are cumulative across `criterion` cycles. The stats
    /// line therefore reports once per process, not once per cycle.
    static ZIPF_REPORTED: std::sync::Once = std::sync::Once::new();

    pub(crate) async fn new_oxkv_store() -> OxKvStore {
        let id = OXKV_CTR.fetch_add(1, Ordering::Relaxed);
        OxKvStore::builder()
            .with_store(Arc::new(MemStorage::new()))
            .with_prefix(ObjectPath::from(format!("bench-{id}")))
            .with_session(format!("bench-sess-{id}"))
            .skip_probe(true)
            .build()
            .await
            .expect("OxKvStore::builder with MemStorage")
    }

    pub(crate) fn seq_insert(rt: &tokio::runtime::Runtime, c: &mut Criterion, n: usize) {
        let mut group = c.benchmark_group(format!("seq_insert/oxkv_mem/{n}"));
        configure(&mut group, n, n);
        let keys: Vec<String> = (0..n).map(key).collect();
        // Blind writes: `seq_insert` measures durable-ingest throughput.
        // `set_bytes` would add a read of the previous value per key, and
        // `random_get` covers that cost.
        group.bench_function("store", |b| {
            b.iter(|| {
                rt.block_on(async {
                    let store = new_oxkv_store().await;
                    for k in &keys {
                        store.put_bytes(k, &PAYLOAD).await.expect("put");
                        black_box(());
                    }
                });
            });
        });
        group.finish();
    }

    pub(crate) fn random_get(rt: &tokio::runtime::Runtime, c: &mut Criterion, n: usize) {
        let mut group = c.benchmark_group(format!("random_get/oxkv_mem/{n}"));
        let take = if n >= LARGE { READ_SAMPLES } else { n };
        configure(&mut group, n, take);
        let keys: Vec<String> = (0..n).map(key).collect();
        let order = shuffled(n);
        let mut store: Option<OxKvStore> = None;
        group.bench_function("get", |b| {
            b.iter(|| {
                let s = store.get_or_insert_with(|| {
                    rt.block_on(async {
                        let mut s = new_oxkv_store().await;
                        populate(&mut s, &keys).await;
                        s
                    })
                });
                rt.block_on(async {
                    for i in order.iter().take(take) {
                        black_box(s.get_bytes(&keys[*i]).await.expect("get"));
                    }
                });
            });
        });
        group.finish();
    }

    pub(crate) fn page_fetch(rt: &tokio::runtime::Runtime, c: &mut Criterion, n: usize) {
        let mut group = c.benchmark_group(format!("page_fetch_{PAGE}/oxkv_mem/{n}"));
        configure(
            &mut group,
            n,
            usize::try_from(PAGE).expect("page size fits"),
        );
        let keys: Vec<String> = (0..n).map(key).collect();
        let mut store: Option<OxKvStore> = None;
        let starts: Vec<String> = (0..128usize).map(|j| key((j * 7919 + n / 2) % n)).collect();
        group.bench_function("fetch", |b| {
            let mut j = 0usize;
            b.iter(|| {
                let start = starts[j % starts.len()].clone();
                j += 1;
                let s = store.get_or_insert_with(|| {
                    rt.block_on(async {
                        let mut s = new_oxkv_store().await;
                        populate(&mut s, &keys).await;
                        s
                    })
                });
                rt.block_on(async {
                    black_box(
                        s.gets_bytes(Some(PAGE), Direction::Next, (Some(start), None))
                            .await
                            .expect("gets_bytes"),
                    );
                });
            });
        });
        group.finish();
    }

    pub(crate) fn tx_commit_batch(rt: &tokio::runtime::Runtime, c: &mut Criterion) {
        let mut group = c.benchmark_group("tx_commit_batch_1000/oxkv_mem");
        configure(&mut group, TX_BATCH, TX_BATCH);
        let keys: Vec<String> = (0..TX_BATCH).map(key).collect();
        group.bench_function("commit", |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let store = new_oxkv_store().await;
                        let tx = store.begin_tx().expect("begin_tx");
                        for k in &keys {
                            tx.set_bytes(k, &PAYLOAD).await.expect("stage");
                        }
                        tx
                    })
                },
                |tx| {
                    rt.block_on(async {
                        tx.commit().await.expect("commit");
                    });
                },
                BatchSize::PerIteration,
            );
        });
        group.finish();
    }

    pub(crate) fn seq_delete(rt: &tokio::runtime::Runtime, c: &mut Criterion, requested: usize) {
        let n = requested.min(DELETE_CAP);
        let mut group = c.benchmark_group(format!("seq_delete/oxkv_mem/{n}"));
        configure(&mut group, n, n);
        let keys: Vec<String> = (0..n).map(key).collect();
        group.bench_function("delete", |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let mut s = new_oxkv_store().await;
                        populate(&mut s, &keys).await;
                        s
                    })
                },
                |store| {
                    rt.block_on(async {
                        for k in &keys {
                            black_box(store.delete(k).await.expect("delete"));
                        }
                    });
                },
                BatchSize::PerIteration,
            );
        });
        group.finish();
    }

    /// In-place updates of a few keys inside a large store.
    ///
    /// Setup drains maintenance first: force flush, WAL GC, and compaction
    /// drain. Timed iterations then measure update cost, not the populate
    /// backlog. After a large durable populate the store otherwise holds pending
    /// L0s and foldable L1s. A single timed op can then start a multi-megabyte
    /// merge. Timed updates still generate steady-state maintenance. The benchmark
    /// measures that maintenance. Only the setup backlog is drained.
    pub(crate) fn point_update(
        rt: &tokio::runtime::Runtime,
        c: &mut Criterion,
        items: usize,
        changes: usize,
    ) {
        let mut group = c.benchmark_group(format!(
            "point_update/oxkv_mem/{items}items_{changes}changes"
        ));
        configure(&mut group, items, changes);
        let keys: Vec<String> = (0..items).map(key).collect();
        let mut store: Option<OxKvStore> = None;
        group.bench_function("update", |b| {
            let mut j = 0usize;
            b.iter(|| {
                let base = (j * changes) % items;
                j += 1;
                let s = store.get_or_insert_with(|| {
                    rt.block_on(async {
                        let mut s = new_oxkv_store().await;
                        populate(&mut s, &keys).await;
                        s.flush_mem_to_sst_force().await.expect("quiesce flush");
                        s.gc_wal().await.expect("quiesce gc");
                        for _ in 0..64 {
                            if s.compact().await.expect("quiesce compact").is_none() {
                                break;
                            }
                        }
                        s.flush_mem_to_sst_force().await.expect("quiesce flush");
                        s.gc_wal().await.expect("quiesce gc");
                        s
                    })
                });
                rt.block_on(async {
                    for i in 0..changes {
                        let k = &keys[(base + i * STRIDE) % items];
                        black_box(s.set_bytes(k, &PAYLOAD).await.expect("update"));
                    }
                });
            });
        });
        group.finish();
    }

    /// Staged-only writes. `stage_set` touches `MemTable` and the WAL buffer. It
    /// performs no I/O. This group isolates the in-memory floor of the write path.
    pub(crate) fn write_stage(rt: &tokio::runtime::Runtime, c: &mut Criterion) {
        const N: usize = 10_000;
        let mut group = c.benchmark_group("write_stage/oxkv_mem/10000");
        group.throughput(Throughput::Elements(N as u64));
        group.bench_function("stage", |b| {
            b.iter_batched(
                || rt.block_on(new_oxkv_store()),
                |s| {
                    rt.block_on(async {
                        for i in 0..N {
                            s.stage_set(&key(i), &PAYLOAD).await;
                        }
                        black_box(());
                    });
                },
                BatchSize::PerIteration,
            );
        });
        group.finish();
    }

    /// Raw storage floor. `MemStorage` conditional PUTs of WAL-sized payloads under
    /// fresh keys. No LSM work runs above these PUTs. This group isolates the
    /// per-write storage cost from ownership, manifest, and maintenance.
    pub(crate) fn write_wal_put(rt: &tokio::runtime::Runtime, c: &mut Criterion) {
        const N: usize = 10_000;
        let mut group = c.benchmark_group("write_wal_put/oxkv_mem/10000");
        group.throughput(Throughput::Elements(N as u64));
        group.bench_function("put", |b| {
            b.iter_batched(
                MemStorage::new,
                |store| {
                    rt.block_on(async {
                        for i in 0..N {
                            let path = ObjectPath::from(format!("bench-wal/{i:08}.log"));
                            store
                                .put_opts(&path, PAYLOAD.to_vec(), PutMode::Create)
                                .await
                                .expect("wal put");
                        }
                        black_box(());
                    });
                },
                BatchSize::PerIteration,
            );
        });
        group.finish();
    }

    /// Non-force flush-check cost over staged `MemTable` sizes. The benchmark stages
    /// `n` entries without durability. It then times a single `flush_mem_to_sst`,
    /// which must stay `None` because the sizes sit far below the 32 MiB threshold.
    /// This isolates the per-write size-estimate scan from all other work.
    pub(crate) fn write_flush_check(rt: &tokio::runtime::Runtime, c: &mut Criterion) {
        for &n in &[1024, 4096, 16384] {
            let mut group = c.benchmark_group(format!("write_flush_check/oxkv_mem/{n}staged"));
            group.throughput(Throughput::Elements(1));
            group.bench_function("check", |b| {
                b.iter_batched(
                    || {
                        rt.block_on(async {
                            let s = new_oxkv_store().await;
                            for i in 0..n {
                                s.stage_set(&key(i), &PAYLOAD).await;
                            }
                            s
                        })
                    },
                    |s| {
                        rt.block_on(async {
                            black_box(s.flush_mem_to_sst().await.expect("flush check"));
                        });
                    },
                    BatchSize::PerIteration,
                );
            });
            group.finish();
        }
    }

    /// Concurrent blind writes from spawned tasks that share one store.
    ///
    /// The benchmark runs on a multi-threaded runtime, which the caller passes
    /// in. It uses 8 tasks x 128 keys. Each key has exactly one owner task.
    /// Therefore the count measures write-gate throughput, not contention on a
    /// single key.
    pub(crate) fn concurrent_write(rt: &tokio::runtime::Runtime, c: &mut Criterion) {
        const TASKS: usize = 8;
        const PER_TASK: usize = 128;
        let mut group =
            c.benchmark_group(format!("concurrent_write/oxkv_mem/{}", TASKS * PER_TASK));
        configure(&mut group, TASKS * PER_TASK, TASKS * PER_TASK);
        group.bench_function("store", |b| {
            b.iter_batched(
                || rt.block_on(new_oxkv_store()),
                |store| {
                    let store = Arc::new(store);
                    rt.block_on(async {
                        let mut handles = Vec::with_capacity(TASKS);
                        for t in 0..TASKS {
                            let store = Arc::clone(&store);
                            handles.push(tokio::spawn(async move {
                                for i in 0..PER_TASK {
                                    let k = format!("t{t:02}:{i:04}");
                                    store.put_bytes(&k, &PAYLOAD).await.expect("put");
                                }
                            }));
                        }
                        for handle in handles {
                            handle.await.expect("task");
                        }
                        black_box(());
                    });
                },
                BatchSize::PerIteration,
            );
        });
        group.finish();
    }

    /// Deterministic Zipf-distributed read order. The order is reproducible
    /// without a `rand` dependency: fixed-seed xorshift, a precomputed CDF, and
    /// binary search. The same binary always produces the same order.
    /// Cross-platform float rounding may shift the exact ranks. That shift only
    /// adds noise, and never signal.
    // Ratios and unit-interval math: precision past 2^53 is irrelevant.
    #[allow(clippy::cast_precision_loss)]
    fn zipf_order(keys: usize, samples: usize, skew: f64) -> Vec<usize> {
        let mut cdf = Vec::with_capacity(keys);
        let mut acc = 0.0f64;
        for rank in 1..=keys {
            acc += 1.0 / (rank as f64).powf(skew);
            cdf.push(acc);
        }
        for p in &mut cdf {
            *p /= acc;
        }
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut order = Vec::with_capacity(samples);
        for _ in 0..samples {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let u = (rng >> 11) as f64 / (1u64 << 53) as f64;
            order.push(cdf.partition_point(|&p| p < u).min(keys - 1));
        }
        order
    }

    /// Skewed reads over many small SSTs, with a cache that fits only a few of
    /// them. Hot SSTs must survive the one-hit tail. Admission policy decides
    /// exactly that. The hit ratio prints to stderr for information.
    /// `criterion` still measures wall time.
    pub(crate) fn zipf_get(rt: &tokio::runtime::Runtime, c: &mut Criterion) {
        const SST_KEYS: usize = 200;
        const SSTS: usize = 50;
        // Sized for the post-compaction layout, because background merges fold the
        // 50 L0s into larger files. This size holds a mid-range fraction. The Zipf
        // head therefore stays resident while the tail churns.
        const CACHE_BYTES: usize = 320 * 1024;
        const READS: usize = 2_000;
        const ZIPF_SKEW: f64 = 1.07;
        let total = SST_KEYS * SSTS;
        let mut group = c.benchmark_group(format!("zipf_get/oxkv_mem/{total}"));
        configure(&mut group, total, READS);
        let keys: Vec<String> = (0..total).map(key).collect();
        let mut store: Option<OxKvStore> = None;
        let mut order: Vec<usize> = Vec::new();
        group.bench_function("get", |b| {
            let s = store.get_or_insert_with(|| {
                rt.block_on(async {
                    let id = OXKV_CTR.fetch_add(1, Ordering::Relaxed);
                    let s = OxKvStore::builder()
                        .with_store(Arc::new(MemStorage::new()))
                        .with_prefix(ObjectPath::from(format!("bench-zipf-{id}")))
                        .with_session(format!("bench-zipf-sess-{id}"))
                        .skip_probe(true)
                        .build_with_cache(LruCache::new(
                            CACHE_BYTES,
                            |_: &String, v: &Arc<SstFile>| {
                                u32::try_from(v.size()).unwrap_or(u32::MAX)
                            },
                        ))
                        .await
                        .expect("zipf store");
                    for f in 0..SSTS {
                        for i in 0..SST_KEYS {
                            let k = key(f * SST_KEYS + i);
                            s.put_bytes(&k, &PAYLOAD).await.expect("put");
                        }
                        // The call may yield no SST when WAL maintenance already
                        // force-flushed this batch. Either way, one more SST is on
                        // disk. Cache pressure needs only dozens of SSTs.
                        let _ = s.flush_mem_to_sst_force().await.expect("flush");
                    }
                    s
                })
            });
            if order.is_empty() {
                order = zipf_order(total, READS * 8, ZIPF_SKEW);
            }
            let mut j = 0usize;
            b.iter(|| {
                rt.block_on(async {
                    for _ in 0..READS {
                        let idx = order[j % order.len()];
                        j += 1;
                        black_box(s.get_bytes(&keys[idx]).await.expect("get"));
                    }
                });
            });
            // `criterion` invokes this closure once per warmup or sample cycle.
            // The counters are cumulative, so the benchmark reports them once.
            // See `ZIPF_REPORTED`.
            ZIPF_REPORTED.call_once(|| {
                if let Some(stats) = s.sst_cache_stats() {
                    eprintln!(
                        "[zipf_get] hit_ratio={:.3} hits={} misses={} evictions={}",
                        stats.hit_ratio(),
                        stats.hits,
                        stats.misses,
                        stats.evictions
                    );
                }
            });
        });
        group.finish();
    }

    /// Shared-store point reads from spawned tasks. This group proves that the
    /// read path scales while writes serialize behind the gate.
    pub(crate) fn concurrent_random_get(rt: &tokio::runtime::Runtime, c: &mut Criterion) {
        const TASKS: usize = 8;
        const N: usize = MEDIUM;
        let take = READ_SAMPLES;
        let mut group = c.benchmark_group(format!("concurrent_random_get/oxkv_mem/{N}"));
        configure(&mut group, N, take);
        let keys: Arc<Vec<String>> = Arc::new((0..N).map(key).collect());
        let order = Arc::new(shuffled(N));
        let mut store: Option<Arc<OxKvStore>> = None;
        group.bench_function("get", |b| {
            let s = store.get_or_insert_with(|| {
                Arc::new(rt.block_on(async {
                    let mut s = new_oxkv_store().await;
                    populate(&mut s, &keys).await;
                    s
                }))
            });
            b.iter(|| {
                rt.block_on(async {
                    let mut handles = Vec::with_capacity(TASKS);
                    for t in 0..TASKS {
                        let s = Arc::clone(s);
                        let keys = Arc::clone(&keys);
                        let order = Arc::clone(&order);
                        handles.push(tokio::spawn(async move {
                            for i in order.iter().skip(t * take / TASKS).take(take / TASKS) {
                                black_box(s.get_bytes(&keys[*i]).await.expect("get"));
                            }
                        }));
                    }
                    for handle in handles {
                        handle.await.expect("task");
                    }
                });
            });
        });
        group.finish();
    }
}

// Cached backend (`MemStorage`, `skip_probe`, `build_cached`). Read shapes
// only. The mirror warms synchronously on every write. No extra warm step is
// therefore needed. Writes share the durable path of `oxkv_mem` by construction.
// ---------------------------------------------------------------------------
#[cfg(feature = "oxkv")]
#[allow(clippy::wildcard_imports)]
mod cached_bench {
    use std::sync::Arc;

    use oxkv::{CachedOxKvStore, MemStorage, ObjectPath};

    use super::*;

    static CACHED_CTR: AtomicUsize = AtomicUsize::new(0);

    pub(crate) async fn new_cached_store() -> CachedOxKvStore {
        let id = CACHED_CTR.fetch_add(1, Ordering::Relaxed);
        OxKvStore::builder()
            .with_store(Arc::new(MemStorage::new()))
            .with_prefix(ObjectPath::from(format!("bench-cached-{id}")))
            .with_session(format!("bench-cached-sess-{id}"))
            .skip_probe(true)
            .build_cached()
            .await
            .expect("OxKvStore::builder build_cached with MemStorage")
    }

    pub(crate) fn random_get(rt: &tokio::runtime::Runtime, c: &mut Criterion, n: usize) {
        let mut group = c.benchmark_group(format!("random_get/cached_mem/{n}"));
        let take = if n >= LARGE { READ_SAMPLES } else { n };
        configure(&mut group, n, take);
        let keys: Vec<String> = (0..n).map(key).collect();
        let order = shuffled(n);
        let mut store: Option<CachedOxKvStore> = None;
        group.bench_function("get", |b| {
            b.iter(|| {
                let s = store.get_or_insert_with(|| {
                    rt.block_on(async {
                        let mut s = new_cached_store().await;
                        populate(&mut s, &keys).await;
                        s
                    })
                });
                rt.block_on(async {
                    for i in order.iter().take(take) {
                        black_box(s.get_bytes(&keys[*i]).await.expect("get"));
                    }
                });
            });
        });
        group.finish();
    }

    pub(crate) fn page_fetch(rt: &tokio::runtime::Runtime, c: &mut Criterion, n: usize) {
        let mut group = c.benchmark_group(format!("page_fetch_{PAGE}/cached_mem/{n}"));
        configure(
            &mut group,
            n,
            usize::try_from(PAGE).expect("page size fits"),
        );
        let keys: Vec<String> = (0..n).map(key).collect();
        let mut store: Option<CachedOxKvStore> = None;
        let starts: Vec<String> = (0..128usize).map(|j| key((j * 7919 + n / 2) % n)).collect();
        group.bench_function("fetch", |b| {
            let mut j = 0usize;
            b.iter(|| {
                let start = starts[j % starts.len()].clone();
                j += 1;
                let s = store.get_or_insert_with(|| {
                    rt.block_on(async {
                        let mut s = new_cached_store().await;
                        populate(&mut s, &keys).await;
                        s
                    })
                });
                rt.block_on(async {
                    black_box(
                        s.gets_bytes(Some(PAGE), Direction::Next, (Some(start), None))
                            .await
                            .expect("gets_bytes"),
                    );
                });
            });
        });
        group.finish();
    }
}

fn benchmark(c: &mut Criterion) {
    let rt = runtime();

    // Full-scan writes: 1K and 100K only, because scaling is linear. The read,
    // update, and page benches cover 1M depth without 1M writes per iteration.
    for &n in &[SMALL, MEDIUM] {
        #[cfg(feature = "btree")]
        {
            seq_insert::<BTreeStore>(&rt, c, "btree_mem", n);
            seq_delete::<BTreeStore>(&rt, c, "btree_mem", n);
        }

        #[cfg(feature = "oxkv")]
        {
            oxkv_bench::seq_insert(&rt, c, n);
            oxkv_bench::seq_delete(&rt, c, n);
        }
    }

    // Reads at all three scales. LARGE samples `READ_SAMPLES` gets out of a
    // 1M-key store. See `random_get`.
    for &n in &[SMALL, MEDIUM, LARGE] {
        #[cfg(feature = "btree")]
        random_get::<BTreeStore>(&rt, c, "btree_mem", n);

        #[cfg(feature = "oxkv")]
        oxkv_bench::random_get(&rt, c, n);

        #[cfg(feature = "oxkv")]
        cached_bench::random_get(&rt, c, n);
    }

    // Range fetch: the per-iteration work is one page. Store depth varies.
    for &n in &[SMALL, LARGE] {
        #[cfg(feature = "btree")]
        page_fetch::<BTreeStore>(&rt, c, "btree_mem", n);

        #[cfg(feature = "oxkv")]
        oxkv_bench::page_fetch(&rt, c, n);

        #[cfg(feature = "oxkv")]
        cached_bench::page_fetch(&rt, c, n);
    }

    #[cfg(feature = "btree")]
    tx_commit_batch::<BTreeStore>(&rt, c, "btree_mem");

    #[cfg(feature = "oxkv")]
    oxkv_bench::tx_commit_batch(&rt, c);

    #[cfg(feature = "oxkv")]
    oxkv_bench::zipf_get(&rt, c);

    // Write-path breakdown: these groups isolate the memory, storage, and
    // flush-check costs. They attribute the full `put_bytes` latency instead of
    // leaving it to guesswork.
    #[cfg(feature = "oxkv")]
    {
        oxkv_bench::write_stage(&rt, c);
        oxkv_bench::write_wal_put(&rt, c);
        oxkv_bench::write_flush_check(&rt, c);
    }

    // Threaded writes need a multi-threaded runtime. Every other benchmark stays
    // on the single-threaded runtime. The numbers therefore remain comparable.
    #[cfg(feature = "oxkv")]
    {
        let mt_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(8)
            .enable_all()
            .build()
            .expect("multi-thread runtime");
        oxkv_bench::concurrent_write(&mt_rt, c);
        oxkv_bench::concurrent_random_get(&mt_rt, c);
    }

    // Changes matrix: (store size, change counts)
    for &(n, counts) in &[(SMALL, &[1usize, 10][..]), (LARGE, &[1, 100, 1_000][..])] {
        for &m in counts {
            #[cfg(feature = "btree")]
            point_update::<BTreeStore>(&rt, c, "btree_mem", n, m);

            #[cfg(feature = "oxkv")]
            oxkv_bench::point_update(&rt, c, n, m);
        }
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = benchmark
}
criterion_main!(benches);
