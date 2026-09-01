#![allow(missing_docs)]

//! Benchmarks for storing and manipulating key-value data at 1,000-item and
//! 1,000,000-item scales across both shipped backends.
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
//!
//! Notes on scale: at 1,000,000 items the setup work dominates wall-clock, so
//! large-scale groups use fewer samples and longer measurement windows.
//! Deletion is capped at 100,000 keys because its per-iteration setup must
//! rebuild the full store; capping keeps total suite runtime sane while still
//! exercising delete at meaningful depth.

use std::hint::black_box;
use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
#[cfg(feature = "btree")]
use oxkv::BTreeStore;
#[cfg(feature = "redb")]
use oxkv::RedbStore;
use oxkv::{Direction, GetSet, Store, Transaction};

const SMALL: usize = 1_000;
const LARGE: usize = 1_000_000;
const DELETE_CAP: usize = 100_000;
const TX_BATCH: usize = 1_000;
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

fn configure(group: &mut criterion::BenchmarkGroup<'_, WallTime>, elements: usize) {
    group.throughput(Throughput::Elements(
        elements.try_into().expect("element count fits u64"),
    ));
    if elements >= LARGE {
        group.sample_size(10);
        group.measurement_time(Duration::from_secs(20));
    }
}

fn seq_insert<S>(rt: &tokio::runtime::Runtime, c: &mut Criterion, backend: &str, n: usize)
where
    S: GetSet + Store + Default,
{
    let mut group = c.benchmark_group(format!("seq_insert/{backend}/{n}"));
    configure(&mut group, n);
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
    configure(&mut group, n);
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
                for i in &order {
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
    configure(&mut group, usize::try_from(PAGE).expect("page size fits"));
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
    configure(&mut group, TX_BATCH);
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
    configure(&mut group, n);
    if requested > DELETE_CAP {
        // Setup rebuilds the whole store per iteration; cap keeps runtime sane.
        group.sample_size(10);
        group.measurement_time(Duration::from_secs(20));
    }
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
    configure(&mut group, changes);
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
fn benchmark(c: &mut Criterion) {
    let rt = runtime();

    for &n in &[SMALL, LARGE] {
        #[cfg(feature = "btree")]
        {
            seq_insert::<BTreeStore>(&rt, c, "btree_mem", n);
            random_get::<BTreeStore>(&rt, c, "btree_mem", n);
            page_fetch::<BTreeStore>(&rt, c, "btree_mem", n);
            seq_delete::<BTreeStore>(&rt, c, "btree_mem", n);
        }
        #[cfg(feature = "redb")]
        {
            seq_insert::<RedbStore>(&rt, c, "redb_mem", n);
            random_get::<RedbStore>(&rt, c, "redb_mem", n);
            page_fetch::<RedbStore>(&rt, c, "redb_mem", n);
            seq_delete::<RedbStore>(&rt, c, "redb_mem", n);
        }
    }

    #[cfg(feature = "btree")]
    tx_commit_batch::<BTreeStore>(&rt, c, "btree_mem");
    #[cfg(feature = "redb")]
    tx_commit_batch::<RedbStore>(&rt, c, "redb_mem");

    // Changes matrix: (store size, change counts)
    for &(n, counts) in &[(SMALL, &[1usize, 10][..]), (LARGE, &[1, 100, 1_000][..])] {
        for &m in counts {
            #[cfg(feature = "btree")]
            point_update::<BTreeStore>(&rt, c, "btree_mem", n, m);
            #[cfg(feature = "redb")]
            point_update::<RedbStore>(&rt, c, "redb_mem", n, m);
        }
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = benchmark
}
criterion_main!(benches);
