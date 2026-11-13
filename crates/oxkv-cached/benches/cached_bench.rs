#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

//! Benchmarks for the `cached_mem` read path.
//!
//! These groups measure `CachedOxKvStore` over an in-memory `MemStorage`.
//! Every write mirrors into RAM, so the benchmark warms the mirror while it
//! populates the store. Only the read shapes run here. Writes share the
//! `oxkv_mem` durable path plus one B-tree insert each, which the core
//! `kv_bench` groups already cover.
//!
//! Run every benchmark with `cargo bench -p oxkv-cached --bench cached_bench`.
//! Run one group with
//! `cargo bench -p oxkv-cached --bench cached_bench random_get/cached_mem/1000`.
//!
//! Workloads:
//! - `random_get/cached_mem/{n}`: point reads in a prime-stride order.
//! - `page_fetch_100/cached_mem/{n}`: one paginated range fetch of 100 entries.
//!
//! Scale strategy:
//! - The 1M groups build the store once and reuse it. Each iteration samples
//!   10K gets or one page, so depth is preserved without 1M operations per
//!   iteration.
//! - Large groups use fewer samples with short warmup and measurement windows.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use oxkv::{Direction, GetSet, MemStorage, ObjectPath, OxKvStore};
use oxkv_cached::{BuildCachedExt, CachedOxKvStore};

const SMALL: usize = 1_000;
const MEDIUM: usize = 100_000;
const LARGE: usize = 1_000_000;
/// Point reads measured per iteration against a LARGE store.
const READ_SAMPLES: usize = 10_000;
const PAGE: usize = 100;
const PAYLOAD: [u8; 64] = [b'x'; 64];
/// Prime stride used for deterministic pseudo-random key selection.
const STRIDE: usize = 7919;

static CACHED_CTR: AtomicUsize = AtomicUsize::new(0);

fn key(i: usize) -> String {
    format!("key:{i:07}")
}

/// Deterministic permutation of `0..n` via a prime stride.
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

/// Tunes sampling by store size while reporting throughput by ops per
/// iteration.
fn configure(group: &mut criterion::BenchmarkGroup<'_, WallTime>, store: usize, ops: usize) {
    group.throughput(Throughput::Elements(
        ops.try_into().expect("element count fits u64"),
    ));
    if store >= LARGE {
        group.sample_size(10);
        group.warm_up_time(Duration::from_secs(2));
        group.measurement_time(Duration::from_secs(10));
    } else if store >= MEDIUM || ops >= MEDIUM {
        group.sample_size(10);
        group.warm_up_time(Duration::from_secs(2));
        group.measurement_time(Duration::from_secs(5));
    }
}

async fn new_cached_store() -> CachedOxKvStore {
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

fn random_get(rt: &tokio::runtime::Runtime, c: &mut Criterion, n: usize) {
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

fn page_fetch(rt: &tokio::runtime::Runtime, c: &mut Criterion, n: usize) {
    let mut group = c.benchmark_group(format!("page_fetch_{PAGE}/cached_mem/{n}"));
    configure(&mut group, n, PAGE);
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

fn benchmark(c: &mut Criterion) {
    let rt = runtime();

    for &n in &[SMALL, MEDIUM, LARGE] {
        random_get(&rt, c, n);
    }

    for &n in &[SMALL, LARGE] {
        page_fetch(&rt, c, n);
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = benchmark
}
criterion_main!(benches);
