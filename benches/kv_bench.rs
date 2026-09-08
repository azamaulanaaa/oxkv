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
//! Run everything with `cargo bench`, a single group with
//! `cargo bench -- seq_insert`, or tune wall-clock with criterion's standard
//! flags, e.g. `cargo bench -- --sample-count 10 --measurement-time 5`.
//!
//! Workloads:
//! - `seq_insert` — sequential insertion of every key (the dominant write path)
//! - `random_get` — point reads in a prime-stride permutation order
//! - `page_fetch_100` — one paginated range fetch of 100 entries
//! - `tx_commit_batch_1000` — commit of a pre-staged 1,000-write transaction
//!   (staging happens in untimed setup, so the number measures durability cost)
//! - `seq_delete` — deletion of every key from a freshly populated store
//! - `point_update` — in-place updates of a few keys inside a large store
//! - `concurrent_write` — 8 threads x 128 blind writes against one shared
//!   store on a multi-thread runtime (oxkv only): exercises the write gate
//! - `zipf_get` — skewed reads over 50 forced SSTs with a 320 KiB cache
//!   (oxkv only): exercises admission policy; hit ratio prints to stderr
//! - `mt_random_get` — shared-store point reads from 8 threads (oxkv only)
//!
//! A/B comparisons against a base commit (baselines live in gitignored
//! `target/criterion`, so they never leave your machine):
//! `mise bench --bench kv_bench -- --save-baseline base` on base, then
//! `mise bench --bench kv_bench -- --baseline base` on the contender.
//!
//! Scale strategy (keeps the full suite in minutes, not hours):
//! - Full-scan writes (`seq_insert`, `seq_delete`) scale linearly, so they
//!   run at 1K and 100K only. A 1M store would cost 1M writes *per iteration*
//!   (times samples, times backends) with no extra signal beyond linearity.
//! - Depth is still tested at 1M via `random_get`, `page_fetch_100`, and
//!   `point_update`, whose per-iteration work is bounded (a capped read
//!   sample / one page / a few updates) against a 1M-key store built once
//!   and reused. Tree depth, SST levels, and index size are identical to a
//!   full 1M scan; only the repeated per-iteration cost is removed.
//! - Large-store groups use fewer samples with short warmup/measurement
//!   windows (see `configure`).
//!
//! `OxKv` backend (feature `oxkv`) uses `MemStorage` with
//! `skip_probe(true)` so the numbers are comparable to `btree_mem`/`oxkv_mem`
//! without network I/O. Prefix is unique per store instance to avoid
//! ownership fencing within the same `MemStorage` bucket.

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
/// holds 1M keys (depth preserved); only the repeated work is capped so a
/// single iteration costs 10K gets instead of 1M.
const READ_SAMPLES: usize = 10_000;
const PAGE: u32 = 100;
const PAYLOAD: [u8; 64] = [b'x'; 64];
/// Prime stride used for deterministic pseudo-random key selection.
const STRIDE: usize = 7919;

fn key(i: usize) -> String {
    format!("key:{i:07}")
}

/// Deterministic permutation of `0..n` via a prime stride (coprime with both
/// benchmark sizes), so "random" access is reproducible without a rand dep.
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
/// iteration* (they differ for sampled reads at LARGE scale).
fn configure(group: &mut criterion::BenchmarkGroup<'_, WallTime>, store: usize, ops: usize) {
    group.throughput(Throughput::Elements(
        ops.try_into().expect("element count fits u64"),
    ));
    if store >= LARGE {
        // One-time 1M populate is kept; repeated per-iteration work is cheap
        // (sampled reads / one page / few updates), so a short window suffices.
        group.sample_size(10);
        group.warm_up_time(Duration::from_secs(2));
        group.measurement_time(Duration::from_secs(10));
    } else if store >= MEDIUM || ops >= MEDIUM {
        // 100K full-scan writes/reads: each iteration is seconds, so keep
        // samples and window small — effect sizes here dwarf sampling noise.
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
    // At LARGE scale the store holds 1M keys but each iteration samples
    // READ_SAMPLES gets, keeping depth while bounding repeated work.
    let take = if n >= LARGE { READ_SAMPLES } else { n };
    configure(&mut group, n, take);
    let keys: Vec<String> = (0..n).map(key).collect();
    let order: Vec<usize> = shuffled(n);

    // Populated lazily on the first (untimed warmup) iteration so that
    // filtered-out benchmarks never pay the setup cost.
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

    // Populated lazily on the first (untimed warmup) iteration so that
    // filtered-out benchmarks never pay the setup cost.
    let mut store: Option<S> = None;

    // Rotate through distinct start cursors instead of always reading the
    // same page, so cached tree paths do not flatter the numbers.
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

    // Staging runs in untimed setup; the measured section is only the commit,
    // i.e. the cost of making a batch durable.
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
/// existing keys) against a store pre-populated with `n` items.
///
/// The store is built once and reused: updates never change the key set, so
/// iterations stay comparable without rebuilding. Each iteration rotates to a
/// different subset of keys, so no single tree region is measured every time.
/// Matrix: 1,000-item stores take 1 and 10 changes; 1,000,000-item stores
/// take 1, 100, and 1,000.
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

    // Populated lazily on the first (untimed warmup) iteration so that
    // filtered-out benchmarks never pay the setup cost.
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
// OxKv backend (MemStorage, skip_probe) — same workload shapes, distinct helpers
// because OxKvStore is not Default and requires async builder.
// ---------------------------------------------------------------------------
#[cfg(feature = "oxkv")]
#[allow(clippy::wildcard_imports)]
mod oxkv_bench {
    use std::sync::Arc;

    use oxkv::{LruCache, MemStorage, ObjectPath, SstFile};

    use super::*;

    static OXKV_CTR: AtomicUsize = AtomicUsize::new(0);

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
        // Blind writes: seq_insert measures durable-ingest throughput, and
        // set_bytes would add a prev-read per key (covered by random_get).
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

    /// Concurrent blind writes from spawned tasks sharing one store.
    ///
    /// Runs on a multi-threaded runtime (passed in): 8 tasks x 128 keys,
    /// each key owned by exactly one task, so the count measures write-gate
    /// throughput rather than contention on a single key.
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

    /// Deterministic Zipf-distributed read order: reproducible without a
    /// `rand` dependency (fixed-seed xorshift + precomputed CDF + binary
    /// search). Same binary, same order; cross-platform float rounding may
    /// shift exact ranks, which only adds noise, never signal.
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

    /// Skewed reads over many small SSTs with a cache that fits a few:
    /// hot SSTs must survive the one-hit tail, which is exactly what
    /// admission policy decides. Hit ratio prints to stderr (informational;
    /// criterion still measures wall time).
    pub(crate) fn zipf_get(rt: &tokio::runtime::Runtime, c: &mut Criterion) {
        const SST_KEYS: usize = 200;
        const SSTS: usize = 50;
        // Sized for the post-compaction layout (background merges fold the
        // 50 L0s into larger files): holds a mid-range fraction so the
        // Zipf head stays resident while the tail churns.
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
                        // May yield no SST when WAL maintenance already
                        // force-flushed this batch (still one more SST
                        // on disk either way); pressure only needs dozens.
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
        group.finish();
    }

    /// Shared-store point reads from spawned tasks: proves the read path
    /// scales while writes serialize behind the gate.
    pub(crate) fn mt_random_get(rt: &tokio::runtime::Runtime, c: &mut Criterion) {
        const TASKS: usize = 8;
        const N: usize = MEDIUM;
        let take = READ_SAMPLES;
        let mut group = c.benchmark_group(format!("mt_random_get/oxkv_mem/{N}"));
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

fn benchmark(c: &mut Criterion) {
    let rt = runtime();

    // Full-scan writes: 1K + 100K only (linear scaling; 1M depth is covered
    // by the read/update/page benches without 1M writes per iteration).
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

    // Reads at all three scales; LARGE samples READ_SAMPLES gets out of a
    // 1M-key store (see `random_get`).
    for &n in &[SMALL, MEDIUM, LARGE] {
        #[cfg(feature = "btree")]
        random_get::<BTreeStore>(&rt, c, "btree_mem", n);

        #[cfg(feature = "oxkv")]
        oxkv_bench::random_get(&rt, c, n);
    }

    // Range fetch: per-iteration work is one page; store depth varies.
    for &n in &[SMALL, LARGE] {
        #[cfg(feature = "btree")]
        page_fetch::<BTreeStore>(&rt, c, "btree_mem", n);

        #[cfg(feature = "oxkv")]
        oxkv_bench::page_fetch(&rt, c, n);
    }

    #[cfg(feature = "btree")]
    tx_commit_batch::<BTreeStore>(&rt, c, "btree_mem");

    #[cfg(feature = "oxkv")]
    oxkv_bench::tx_commit_batch(&rt, c);

    #[cfg(feature = "oxkv")]
    oxkv_bench::zipf_get(&rt, c);

    // Threaded writes need a multi-threaded runtime; everything else stays
    // on the single-threaded one so numbers remain comparable.
    #[cfg(feature = "oxkv")]
    {
        let mt_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(8)
            .enable_all()
            .build()
            .expect("multi-thread runtime");
        oxkv_bench::concurrent_write(&mt_rt, c);
        oxkv_bench::mt_random_get(&mt_rt, c);
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
