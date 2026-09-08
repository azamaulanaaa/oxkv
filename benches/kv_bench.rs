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
//! `OxKv` backend (feature `oxkv`) uses `object_store::memory::InMemory` with
//! `skip_probe(true)` so the numbers are comparable to `btree_mem`/`oxkv_mem`
//! without network I/O. Prefix is unique per store instance to avoid
//! ownership fencing within the same `InMemory` bucket.

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
        .expect("tokio multi-threaded runtime")
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
        // 100K full-scan writes/reads: each iteration is ~seconds, so cut
        // criterion's default 100 samples down to stay in minutes.
        group.sample_size(15);
        group.warm_up_time(Duration::from_secs(2));
        group.measurement_time(Duration::from_secs(10));
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
                let mut store = S::default();
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
                    let mut store = S::default();
                    let mut tx = store.begin_tx().expect("begin_tx");
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
            |mut store| {
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
// OxKv backend (InMemory, skip_probe) — same workload shapes, distinct helpers
// because OxKvStore is not Default and requires async builder.
// ---------------------------------------------------------------------------
#[cfg(feature = "oxkv")]
#[allow(clippy::wildcard_imports)]
mod s3_bench {
    use std::sync::Arc;

    use object_store::memory::InMemory;
    use object_store::path::Path;

    use super::*;

    static S3_CTR: AtomicUsize = AtomicUsize::new(0);

    pub(crate) async fn new_s3_store() -> OxKvStore {
        let id = S3_CTR.fetch_add(1, Ordering::Relaxed);
        OxKvStore::builder()
            .with_store(Arc::new(InMemory::new()))
            .with_prefix(Path::from(format!("bench-{id}")))
            .with_session(format!("bench-sess-{id}"))
            .skip_probe(true)
            .build()
            .await
            .expect("OxKvStore::builder with InMemory")
    }

    pub(crate) fn seq_insert(rt: &tokio::runtime::Runtime, c: &mut Criterion, n: usize) {
        let mut group = c.benchmark_group(format!("seq_insert/s3_mem/{n}"));
        configure(&mut group, n, n);
        let keys: Vec<String> = (0..n).map(key).collect();
        group.bench_function("store", |b| {
            b.iter(|| {
                rt.block_on(async {
                    let mut store = new_s3_store().await;
                    for k in &keys {
                        black_box(store.set_bytes(k, &PAYLOAD).await.expect("set"));
                    }
                });
            });
        });
        group.finish();
    }

    pub(crate) fn random_get(rt: &tokio::runtime::Runtime, c: &mut Criterion, n: usize) {
        let mut group = c.benchmark_group(format!("random_get/s3_mem/{n}"));
        let take = if n >= LARGE { READ_SAMPLES } else { n };
        configure(&mut group, n, take);
        let keys: Vec<String> = (0..n).map(key).collect();
        let order = shuffled(n);
        let mut store: Option<OxKvStore> = None;
        group.bench_function("get", |b| {
            b.iter(|| {
                let s = store.get_or_insert_with(|| {
                    rt.block_on(async {
                        let mut s = new_s3_store().await;
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
        let mut group = c.benchmark_group(format!("page_fetch_{PAGE}/s3_mem/{n}"));
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
                        let mut s = new_s3_store().await;
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
        let mut group = c.benchmark_group("tx_commit_batch_1000/s3_mem");
        configure(&mut group, TX_BATCH, TX_BATCH);
        let keys: Vec<String> = (0..TX_BATCH).map(key).collect();
        group.bench_function("commit", |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let mut store = new_s3_store().await;
                        let mut tx = store.begin_tx().expect("begin_tx");
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
        let mut group = c.benchmark_group(format!("seq_delete/s3_mem/{n}"));
        configure(&mut group, n, n);
        let keys: Vec<String> = (0..n).map(key).collect();
        group.bench_function("delete", |b| {
            b.iter_batched(
                || {
                    rt.block_on(async {
                        let mut s = new_s3_store().await;
                        populate(&mut s, &keys).await;
                        s
                    })
                },
                |mut store| {
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
        let mut group =
            c.benchmark_group(format!("point_update/s3_mem/{items}items_{changes}changes"));
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
                        let mut s = new_s3_store().await;
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
            s3_bench::seq_insert(&rt, c, n);
            s3_bench::seq_delete(&rt, c, n);
        }
    }

    // Reads at all three scales; LARGE samples READ_SAMPLES gets out of a
    // 1M-key store (see `random_get`).
    for &n in &[SMALL, MEDIUM, LARGE] {
        #[cfg(feature = "btree")]
        random_get::<BTreeStore>(&rt, c, "btree_mem", n);

        #[cfg(feature = "oxkv")]
        s3_bench::random_get(&rt, c, n);
    }

    // Range fetch: per-iteration work is one page; store depth varies.
    for &n in &[SMALL, LARGE] {
        #[cfg(feature = "btree")]
        page_fetch::<BTreeStore>(&rt, c, "btree_mem", n);

        #[cfg(feature = "oxkv")]
        s3_bench::page_fetch(&rt, c, n);
    }

    #[cfg(feature = "btree")]
    tx_commit_batch::<BTreeStore>(&rt, c, "btree_mem");

    #[cfg(feature = "oxkv")]
    s3_bench::tx_commit_batch(&rt, c);

    // Changes matrix: (store size, change counts)
    for &(n, counts) in &[(SMALL, &[1usize, 10][..]), (LARGE, &[1, 100, 1_000][..])] {
        for &m in counts {
            #[cfg(feature = "btree")]
            point_update::<BTreeStore>(&rt, c, "btree_mem", n, m);

            #[cfg(feature = "oxkv")]
            s3_bench::point_update(&rt, c, n, m);
        }
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = benchmark
}
criterion_main!(benches);
