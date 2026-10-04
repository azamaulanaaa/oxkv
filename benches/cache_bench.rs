//! Micro-benchmarks for the [`Cache`](oxkv::Cache) trait itself: insert,
//! hit, miss, and skewed (`Zipf`) read pressure. Each read pressure run uses
//! the built-in scan-resistant `S3-FIFO` (`lru`) and, with `--features moka`,
//! the optional `moka` backend (`moka`). The two backends serve an A/B
//! comparison of admission policy.
//!
//! The end-to-end benches in `kv_bench` mostly measure cache *miss* paths
//! (SST parse and insert). These benches isolate the data structure. Values are
//! fixed at 64 bytes. Weight equals byte length. Capacity holds 4,096 entries,
//! and the `zipf` working set is 4x that capacity. Therefore admission policy,
//! not capacity, decides the hit ratio. Hit ratios print once per impl to stderr.
//!
//! Run with `cargo bench --bench cache_bench`.

// Only `missing_docs`: a bench binary exposes no API surface, and the sole
// public item is the generated entry fn of `criterion_group!`. Every code check
// (pedantic casts, correctness, dead code) stays at full deny.
#![allow(missing_docs)]

use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use oxkv::{Cache, LruCache};

/// Value size. This size also acts as the per-entry weight through `Vec::len`.
const VALUE_LEN: usize = 64;
/// Entries that fit in the cache under test.
const CAPACITY_ENTRIES: usize = 4_096;
/// Total keys in the skewed working set (4x capacity).
const ZIPF_KEYS: usize = 16_384;
/// Reads per `zipf` iteration.
const ZIPF_READS: usize = 4_096;
/// Zipf skew. The distribution is head-heavy, and its tail keeps changing.
const ZIPF_SKEW: f64 = 1.07;

fn key(i: usize) -> String {
    format!("key:{i:07}")
}

fn value() -> Vec<u8> {
    vec![b'x'; VALUE_LEN]
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
}

/// Fixed-seed xorshift. The order is deterministic across runs, and the
/// function needs no `rand` dependency.
/// The casts are absent on purpose: ranks fit `f64` exactly via an incrementing
/// counter, and the draw uses the upper 32 bits (`f64::from(u32)` is exact).
fn zipf_order(total: usize, len: usize, skew: f64) -> Vec<usize> {
    let mut weights = Vec::with_capacity(total);
    let mut cumulative = 0.0;
    let mut rank: f64 = 0.0;
    for _ in 0..total {
        rank += 1.0;
        cumulative += 1.0 / rank.powf(skew);
        weights.push(cumulative);
    }
    let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut order = Vec::with_capacity(len);
    for _ in 0..len {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let hi = u32::try_from(rng >> 32).unwrap_or(u32::MAX);
        let draw = f64::from(hi) / f64::from(u32::MAX) * cumulative;
        let idx = weights.partition_point(|&w| w < draw).min(total - 1);
        order.push(idx);
    }
    order
}

fn lru() -> LruCache<String, Vec<u8>> {
    LruCache::new(CAPACITY_ENTRIES * VALUE_LEN, |_: &String, v: &Vec<u8>| {
        u32::try_from(v.len()).unwrap_or(u32::MAX)
    })
}

#[cfg(feature = "moka")]
fn moka() -> moka::future::Cache<String, Vec<u8>> {
    moka::future::Cache::builder()
        .max_capacity((CAPACITY_ENTRIES * VALUE_LEN) as u64)
        .weigher(|_: &String, v: &Vec<u8>| u32::try_from(v.len()).unwrap_or(u32::MAX))
        .build()
}

/// Sequential inserts of every key. The group measures insert throughput and
/// eviction-churn throughput.
fn insert<C>(
    rt: &tokio::runtime::Runtime,
    c: &mut Criterion,
    name: &str,
    make: impl Fn() -> C + Copy,
) where
    C: Cache<String, Vec<u8>>,
{
    let n = CAPACITY_ENTRIES;
    let mut group = c.benchmark_group(format!("cache_insert/{name}"));
    group.throughput(Throughput::Elements(n as u64));
    group.bench_function(BenchmarkId::from_parameter(n), |b| {
        b.iter_batched(
            make,
            |cache| {
                rt.block_on(async {
                    for i in 0..n {
                        cache.insert(key(i), value()).await;
                    }
                    black_box(());
                });
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

/// Repeated reads over resident keys: pure hit-path throughput.
fn get_hit<C>(
    rt: &tokio::runtime::Runtime,
    c: &mut Criterion,
    name: &str,
    make: impl Fn() -> C + Copy,
) where
    C: Cache<String, Vec<u8>>,
{
    const N: usize = 10_000;
    let mut group = c.benchmark_group(format!("cache_get_hit/{name}"));
    group.throughput(Throughput::Elements(N as u64));
    group.bench_function(BenchmarkId::from_parameter(N), |b| {
        b.iter_batched(
            || {
                let cache = make();
                rt.block_on(async {
                    for i in 0..n_keys() {
                        cache.insert(key(i), value()).await;
                    }
                });
                cache
            },
            |cache| {
                rt.block_on(async {
                    for i in 0..N {
                        let hit = cache.get(&key(i % n_keys())).await.is_some();
                        black_box(hit);
                    }
                });
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

/// Repeated reads over absent keys: pure miss-path throughput.
fn get_miss<C>(
    rt: &tokio::runtime::Runtime,
    c: &mut Criterion,
    name: &str,
    make: impl Fn() -> C + Copy,
) where
    C: Cache<String, Vec<u8>>,
{
    const N: usize = 10_000;
    let mut group = c.benchmark_group(format!("cache_get_miss/{name}"));
    group.throughput(Throughput::Elements(N as u64));
    group.bench_function(BenchmarkId::from_parameter(N), |b| {
        b.iter_batched(
            make,
            |cache| {
                rt.block_on(async {
                    for i in 0..N {
                        let hit = cache.get(&key(n_keys() + i)).await.is_some();
                        black_box(hit);
                    }
                });
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

/// Skewed reads over a 4x-capacity working set. Admission policy decides the
/// hit ratio. The cache is shared across invocations. It builds once, and it
/// starts empty. A read-through insert runs on each miss. Therefore the warmup of
/// `criterion` drives the cache to a steady state. The benchmark counts hits
/// locally (`is_some`), so both backends report comparably. `moka` tracks no
/// stats, which does not change that comparison.
fn zipf<C>(
    rt: &tokio::runtime::Runtime,
    c: &mut Criterion,
    name: &str,
    make: impl Fn() -> C + Copy,
    reported: &'static std::sync::Once,
) where
    C: Cache<String, Vec<u8>>,
{
    let keys: Vec<String> = (0..ZIPF_KEYS).map(key).collect();
    let order = zipf_order(ZIPF_KEYS, ZIPF_READS * 8, ZIPF_SKEW);
    let mut group = c.benchmark_group(format!("cache_zipf/{name}/{ZIPF_KEYS}"));
    group.throughput(Throughput::Elements(ZIPF_READS as u64));
    let mut cache: Option<C> = None;
    group.bench_function("get", |b| {
        let cache = cache.get_or_insert_with(make);
        // The rotation stays fixed across invocations, which keeps the cycles
        // comparable. The skew lives in `order` itself.
        let mut j = 0usize;
        let mut run_hits = 0u64;
        let mut run_gets = 0u64;
        b.iter(|| {
            let hits = rt.block_on(async {
                let mut hits = 0u64;
                for _ in 0..ZIPF_READS {
                    let idx = order[j % order.len()];
                    j += 1;
                    // Read-through: a miss populates the cache. The warmup therefore
                    // converges to the steady-state hot set instead of missing
                    // forever.
                    if cache.get(&keys[idx]).await.is_some() {
                        hits += 1;
                    } else {
                        cache.insert(keys[idx].clone(), value()).await;
                    }
                }
                hits
            });
            black_box(hits);
            run_hits += hits;
            run_gets += ZIPF_READS as u64;
        });
        reported.call_once(|| {
            // The value is an integer permille. This form is exact. It
            // needs no float casts.
            let permille = run_hits * 1_000 / run_gets.max(1);
            eprintln!(
                "[cache_zipf/{name}] hit_ratio={}.{:03} hits={run_hits} misses={}",
                permille / 1_000,
                permille % 1_000,
                run_gets - run_hits
            );
        });
    });
    group.finish();
}

/// Keys that fit without eviction. The hit bench and the miss bench use them as
/// a base.
const fn n_keys() -> usize {
    CAPACITY_ENTRIES / 2
}

static ZIPF_LRU_REPORTED: std::sync::Once = std::sync::Once::new();
#[cfg(feature = "moka")]
static ZIPF_MOKA_REPORTED: std::sync::Once = std::sync::Once::new();

/// Registers every cache group: insert, hit, miss, and skewed reads. Each
/// group runs against the built-in `S3-FIFO` and, with `--features moka`, against
/// `moka`.
fn benchmark(c: &mut Criterion) {
    let rt = runtime();
    insert(&rt, c, "lru", lru);
    get_hit(&rt, c, "lru", lru);
    get_miss(&rt, c, "lru", lru);
    zipf(&rt, c, "lru", lru, &ZIPF_LRU_REPORTED);
    #[cfg(feature = "moka")]
    {
        insert(&rt, c, "moka", moka);
        get_hit(&rt, c, "moka", moka);
        get_miss(&rt, c, "moka", moka);
        zipf(&rt, c, "moka", moka, &ZIPF_MOKA_REPORTED);
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = benchmark
}
criterion_main!(benches);
