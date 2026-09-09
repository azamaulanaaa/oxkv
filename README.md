
# oxkv

A transactional key-value store library written in Rust, with optional WebAssembly bindings for JavaScript interop. Features cursor-based pagination, a Lucene-style query engine that matches stored JSON documents, JSON serialization via `serde_json`, streaming snapshot export/import, optional OpenTelemetry instrumentation, and strict linting. All operations are `async` and take `&self`, and every store is `Send + Sync`, so one store can be shared across threads via `Arc` — the WASM bindings wrap the same backends for JavaScript.

## Features

- **Transaction support** — atomic commit/rollback batches of CRUD operations
- **Cursor-based pagination** — bidirectional traversal (`Next` / `Prev`) with inclusive range cursors and limit control
- **Lucene-style query engine** — filter stored JSON documents with a query language supporting field paths, ranges, wildcards, regex, fuzzy matching, and boolean operators
- **JSON serialization** — extension methods for inserting and retrieving `serde_json::Value` types via JSON, stored as raw bytes
- **WASM bindings** — thread-safe wrappers in `src/wasm/` expose `BTreeStore`, `OxKvStore`, and `CachedOxKvStore` to JavaScript as async promises (`otel` native-only, OXKV snapshot portable across all)
- **Extensible backends** — the crate defines three traits (`GetSet`, `Transaction`, `Store`) that any backend can implement; ships with an in-memory B-tree backend (`btree`, test and bench baseline + WASM baseline) and an LSM backend (`oxkv`, native + wasm) generic over `Storage` + `Cache` (S3/GCS/Azure via `oxkv-s3`)
- **LSM backend** — portable LSM over pluggable `Storage` (in-memory `MemStorage` everywhere including browsers; S3/GCS/Azure/local via [`object_store`](https://docs.rs/object_store) with `oxkv-s3`, OPFS origin-private storage in browsers): epoch-fenced single writer, WAL with RPO=0, `MemTable` + SST (L0/L1) with Bloom + CRC, blob overflow for large values, scan-resistant `S3-FIFO` SST cache (trait, `moka` optional), WAL replay, GC and L0→L1 compaction, group-committed batched writes, lazy pull-merge range scans, read-only `OxKvReader` followers, write-through `CachedOxKvStore` RAM mirror
- **Validation hooks** — reject invalid writes before they reach storage, scoped to a single key, a key prefix, or the whole store
- **Reactivity** — watch keys or prefixes and observe every committed change via channels or observer traits; rolled-back transactions never notify
- **Save/Load** — serialize the entire store contents into a single contiguous `Uint8Array` and reconstruct it from binary data
- **Streaming snapshots** — same wire format as an incremental byte stream: memory stays bounded regardless of store size on both Rust (via `futures::Stream`) and JavaScript (native `ReadableStream`)
- **OpenTelemetry** — opt-in `OtelStore` decorator emitting spans and metrics around every operation; the crate ships API-only, so your application plugs in any SDK/exporter
- **Strict linting** — all warnings and clippy lints are enforced at the crate level

## Core Traits

| Trait | Purpose |
| ------- | --------- |
| [`store::GetSet`] | Basic key-value operations: `get_bytes`, `set_bytes`, `put_bytes` (blind write, no previous-value read), `delete`, `has` (paginated via `gets_bytes`) |
| [`store::Transaction`] | Extends `GetSet` with `commit` and `rollback` for atomic batches |
| [`store::Store`] | Extends `GetSet` with `begin_tx` — starts a write transaction |
| [`store::GetSetExt`] | Convenience methods: `set`, `get` (JSON-serialized) and `gets` (paginated JSON retrieval with optional query filtering) |
| [`store::StoreExt`] | Save/Load the entire store contents as binary; `save_stream`/`load_stream` stream it as byte chunks |
| [`store::Validator`] | Validates writes before they are stored (attach per key, prefix, or globally) |
| [`store::Observer`] | Receives change notifications after they become durable |
| [`store::HookStore`] | Decorator adding validators and change watching to any store |
| [`store::OtelStore`] | Feature-gated decorator adding OpenTelemetry traces and metrics to any store |
| [`store::OxKvStore`] / [`store::OxKvStoreBuilder`] / [`store::OxKvTx`] | Feature-gated (`oxkv`, native + wasm) LSM generic over `Storage` + `Cache` (in-memory `MemStorage`; S3 via `oxkv-s3`); single-writer epoch fencing, WAL + SST + blob overflow |
| [`store::OxKvReader`] / [`store::OxKvRoTx`] | Feature-gated (`oxkv`, native + wasm) read-only follower sharing the `Store` trait — opens without epoch CAS, follows writer WALs, writes rejected, empty-tx commit is a no-op |
| [`store::CachedOxKvStore`] / [`store::CachedTx`] / [`store::WarmMode`] | Feature-gated (`oxkv`, native + wasm) write-through RAM mirror sharing the `Store` trait — zero-I/O reads once warmed, `Eager`/`Background`/`Lazy` fill via `warm`/`warm_step`/`is_warmed`, WAL durability, TTL-checked reads, `refresh`/`check_stale` catch-up, fencing poisons until refresh |
| [`store::StoreError::Fenced`] | Terminal fencing error — another owner acquired the epoch via `ownership.json` CAS |
| [`store::StoreError::CasConflict`] | Conditional-write precondition failed — someone else won the CAS race; retryable unless fencing says otherwise |

## Quick Start

```rust
use oxkv::{BTreeStore, Direction, GetSet, GetSetExt, Store, Transaction};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let store = BTreeStore::default();

    // Insert a raw byte value (returns None for a new key)
    let inserted = store.set_bytes("greeting", b"hello").await.unwrap();
    assert_eq!(inserted, None);

    // Read it back
    let val = store.get_bytes("greeting").await.unwrap();
    assert_eq!(val, Some(b"hello".to_vec()));

    // Cursor-based pagination (all keys)
    let page = store.gets_bytes(None, Direction::Next, (None, None)).await.unwrap();
    for kv in &page {
        println!("{}: {:?}", kv.key, kv.value);
    }

    // Transactional batch
    let tx = store.begin_tx().unwrap();
    tx.set_bytes("a", b"1").await.unwrap();
    tx.set_bytes("b", b"2").await.unwrap();
    tx.commit().await.unwrap();

    // JSON serialization
    use serde_json::json;
    store.set("config", &json!({"theme": "dark"})).await.unwrap();
    let config: serde_json::Value = store.get("config").await.unwrap().unwrap();
}
```

## Querying Stored Documents

Any store can scan its entries and return only the JSON documents that match a
query string, using `gets`. It mirrors `gets_bytes`: same `limit`, `direction`
and cursor semantics — when no query is passed it is a plain pass-through.

```rust,no_run
# #[tokio::main(flavor = "current_thread")]
# async fn main() {
#     let store = oxkv::BTreeStore::default();
use oxkv::{Direction, GetSetExt};

// Find users aged 30-40 tagged "rust", newest keys last, max 10 results
let matches = store
    .gets(
        Some(10),
        Direction::Next,
        (None, None),
        Some("age:[30 TO 40] AND tags:rust"),
    )
    .await
    .unwrap();

for kv in &matches {
    println!("{} -> {}", kv.key, String::from_utf8_lossy(&kv.value));
}
# }
```

### How Matching Works

Scoping selects **which leaf** is examined — not how it matches. Bare terms
and quoted phrases behave identically whether unscoped or field-scoped:
unscoped terms search every leaf in the document; `field:` paths descend into
objects (`address.city`) and fan out across arrays (`tags`).

```rust
use oxkv::{eval, parse};
use serde_json::json;

fn matches(doc: &serde_json::Value, query: &str) -> bool {
    let parsed = parse(query).unwrap();
    eval(&parsed, doc)
}

let doc = json!({
    "bio": "i am born on 2000",
    "lang": "rust",
    "age": 30,
    "tags": ["systems", "kv"],
    "address": { "city": "Berlin" }
});

// Bare terms match word tokens fuzzily (Levenshtein <= 2 by default):
assert!(matches(&doc, "born"));        // token hit
assert!(matches(&doc, "boren"));       // typo within default slop
assert!(matches(&doc, "bio:born"));    // scoped: same matching, one leaf
assert!(matches(&doc, "systms"));      // typo: one letter off "systems"

// Quoted phrases match as case-insensitive substrings:
assert!(matches(&doc, "\"am born on\""));
assert!(matches(&doc, "address.city:\"berlin\""));

// Numbers with parseable targets compare numerically:
assert!(matches(&doc, "age:30"));      // exact even though matching is fuzzy
```

Rules of thumb:

- **Bare term** → any word token of the value within edit distance 2
  (override per-term with `~N`; note transpositions count as two edits).
- **Quoted phrase** → case-insensitive containment anywhere in the value.
- **Wildcards** (`rus*`, `j?va`) → anchored whole-value globs.
- **Regex** (`/pattern/`) → substring search via the regex crate.
- **Numbers** stay precise: a numeric term against a numeric leaf compares
  as a number, not fuzzily as text.
- **Date-shaped values** (`2025-03-08`, timestamps with `Z` or offsets)
  always route to UTC calendar-interval comparison.
- Operators are **uppercase** (`AND`, `OR`, `NOT`) — lowercase `and` is an
  ordinary search term.

### Query Syntax

| Feature | Example | Notes |
| --------- | --------- | ------- |
| Plain term | `rust`, `lang:rust` | fuzzy word-token match (slop 2), so `carrs` still finds `cars`; scoping selects which leaves are searched |
| Field-scoped term | `lang:rust` | dot-separated paths descend into objects (`address.city:Berlin`) and fan out across arrays (`tags:kv`) |
| Quoted phrase | `"memory safe"`, `title:"rust prog"` | case-insensitive substring containment in any scope |
| Wildcards | `name:r*`, `j?va` | `*` and `?`, case-insensitive |
| Regex | `email:/@gmail\.com$/` | Rust `regex` crate syntax |
| Fuzzy | `name:Jon~1` | Levenshtein distance ≤ slop; bare `~` defaults to 2 |
| Boost | `rust^2.5` | parsed but ignored for boolean matching |
| Inclusive range | `age:[30 TO 40]` | numeric bounds also match numeric-looking strings |
| Exclusive range | `date:{2020 TO 2024}` | lexicographic comparison for non-numeric values |
| Calendar date range | `created:[2025-01-01 TO 2025-12-31]`, `created:2025-03` | ISO-8601-shaped bounds compare as UTC calendar intervals instead of text; partial literals cover their whole period, so a day literal matches any timestamp that day; offsets are normalized to UTC and naive times read as UTC; non-date strings keep classic comparison |
| Boolean operators | `a AND b OR c` | `AND` binds tighter than `OR`; `&&`, `\|\|` aliases; a missing operator defaults to `OR` |
| Occurrence prefixes | `+required -excluded NOT banned` | without explicit operators: all `+` must match, no `-`/`NOT` may match, at least one optional clause must match |
| Sub-queries | `(rust OR go) AND age:[18 TO 30]` | parenthesized groups, optionally field-scoped (`tags:(rust OR go)`) |
| Escapes | `a\.b:x` | rarely needed: quotes are literal containers (`"all-in-one"`, `"plus + plus"`); backslash remains for `\"` inside phrases, `\/` inside regex, and dots in field names |

Invalid queries return a `StoreError::Other`; entries whose values are not
valid JSON are skipped during scans (or returned untouched by pass-through
calls without a query).

## Hooks and Reactivity

Wrap any backend in a `HookStore` to validate values before they are stored
and to listen for changes:

```rust
# #[tokio::main(flavor = "current_thread")]
# async fn main() {
use oxkv::{BTreeStore, ChangeEvent, ChangeKind, GetSet, HookStore, Scope, Validator};

struct RequireJson(Scope);

#[async_trait::async_trait]
impl Validator for RequireJson {
    fn scope(&self) -> Scope {
        self.0.clone()
    }

    async fn validate(
        &self,
        _ctx: &dyn oxkv::StoreView,
        key: &str,
        value: &[u8],
    ) -> oxkv::Result<()> {
        serde_json::from_slice::<serde_json::Value>(value)
            .map(|_| ())
            .map_err(|e| format!("key `{key}` requires JSON: {e}").into())
    }
}

let mut store = HookStore::new(BTreeStore::default());

// Only values under "doc:" must be JSON
store.add_validator(RequireJson(Scope::Prefix("doc:".into())));

// Subscribe to changes of a single key
let mut rx = store.watch("user:42");

store.set_bytes("user:42", b"hello").await.unwrap();
let event: ChangeEvent = rx.try_recv().unwrap();
assert_eq!(event.kind, ChangeKind::Set);
assert_eq!(event.old_value, None);
assert_eq!(event.new_value, Some(b"hello".to_vec()));
# }
```

- Validators run before every write, including transactional staging; an
  error rejects the write without touching the underlying store. Validators
  receive a read-only `StoreView` so rules can compare against other keys
  inside a transaction it reflects the transaction's own staged writes.
  Staged writes are re-validated at commit time, so staging-time decisions
  cannot be invalidated by later writes in the same transaction; during
  that pass the key being validated shows its pre-transaction value, so
  absence-based rules behave correctly. Validators are snapshotted when a
  transaction begins; later registrations do not affect open transactions.
- Every `ChangeEvent` carries the key, the change kind, and the old and new
  values when they are observable, so observers never need to re-read the
  store.
- Watchers (`watch`, `watch_prefix`, `watch_all`) receive one event per
  committed change over a bounded channel (256 events); a consumer that
  falls behind misses events rather than stalling writers, and dropping the
  receiver unsubscribes.
- Transactions broadcast once per commit and never on rollback; staged
  events for the same key collapse into the final one while preserving the
  original pre-transaction value.
- For callback-style consumption implement the `Observer` trait instead of
  using channels. Observers receive the same read-only `StoreView`, resolved
  to committed state as of after the change. Matching observers run
  concurrently with each other, but writes await their completion; use
  channels for fire-and-forget reactivity.

Hooks must not call back into the same store: stores guard their state with
locks, so reentrant hook calls can deadlock.

## Streaming Snapshots

`save`/`load` materialize the whole snapshot in memory. For large stores,
stream the same wire format instead:

```rust
# #[tokio::main(flavor = "current_thread")]
# async fn main() {
use futures::StreamExt;
use oxkv::{BTreeStore, GetSet, StoreExt, load_stream};

# let store = BTreeStore::default();
# store.set_bytes("hello", b"world").await.unwrap();
// Serialize lazily: only one page of entries is held at a time.
let mut chunks = store.save_stream();
let mut bytes = Vec::new();
while let Some(chunk) = chunks.next().await {
    bytes.extend_from_slice(&chunk.unwrap());
}

// Restore from any source of byte chunks — boundaries may split anywhere,
// decoding is incremental and writes are staged in one transaction that is
// committed only on success. Chunk errors fold into StoreError via Into.
let dest = BTreeStore::default();
let count = load_stream(
    &dest,
    futures::stream::iter(vec![Ok::<_, oxkv::StoreError>(bytes)]),
)
.await
.unwrap();
assert_eq!(count, 1);
assert_eq!(
    dest.get_bytes("hello").await.unwrap().as_deref(),
    Some(b"world".as_slice())
);
# }
```

Every snapshot starts with an 8-byte header — magic `"OXKV"` plus a
little-endian format version — followed by length-prefixed records
(`[u32 key len][key][u32 value len][value]`). Loaders validate the header
before accepting any record: foreign payloads and unsupported future versions
are rejected with a descriptive error instead of being mis-parsed, so format
evolution is trackable. Chunk boundaries always fall between records, so
chunks concatenate to exactly what `save` returns and each decodes
independently.

## OpenTelemetry (feature `otel`)

Enable the `otel` feature and wrap any backend:

```text
use oxkv::{BTreeStore, OtelStore};

let store = OtelStore::new(BTreeStore::default());
// every operation below now emits spans + metrics; without an SDK installed
// everything resolves to no-ops and the store behaves as a passthrough
```

The crate depends on the OpenTelemetry **API** only — no SDK or exporter. Your
application installs providers globally before the first store operation:

```text
use opentelemetry_sdk::trace::SdkTracerProvider;

let tracer_provider = SdkTracerProvider::builder().build();
opentelemetry::global::set_tracer_provider(tracer_provider);
// same idea for metrics via SdkMeterProvider / set_meter_provider
```

What you get per operation (`get`, `has`, `set`, `delete`, `gets`, `begin_tx`,
`commit`, `rollback`):

- One span named after the operation with `db.system = "oxkv"`,
  `db.operation.name`, `oxkv.key` (single-key ops), `oxkv.existed` and
  `oxkv.items`. Failures record an `exception` event plus `Error` status.
- Spans root at the caller's current span, so store activity nests inside your
  request traces; `commit`/`rollback` become children of their `begin_tx`.
- Metrics under the meter `"oxkv"`: `oxkv.store.operations` counter
  (`db.operation.name`, `oxkv.outcome` = `ok`/`error`) and
  `oxkv.store.operation.duration` histogram in seconds.

Decorators compose: `OtelStore::new(HookStore::new(OxKvStore::builder().with_store(store).build().await?))` measures
the full validation pipeline.

## LSM Backend (feature `oxkv`, native + wasm)

`OxKvStore` is an LSM tree over any [`Storage`](src/store/storage.rs) backend: in-memory `MemStorage` (native + wasm, including the browser `OxKvStore`), or S3-compatible storage (S3, GCS, Azure) and local FS via `object_store` with `oxkv-s3`. It shares the same `GetSet`/`Store`/`Transaction` traits as the other backends, so application code is portable. Snapshot bytes (`OXKV` magic `+` version + records) are identical across `BTreeStore` and `OxKvStore` on every target — `save`/`load` round-trip everywhere.

```rust
# #[tokio::main(flavor = "current_thread")]
# async fn main() {
use std::sync::Arc;
use oxkv::{GetSet, MemStorage, ObjectPath, OxKvStore, Storage, Store, Transaction};

let store: Arc<dyn Storage> = Arc::new(MemStorage::new());
// In prod replace MemStorage with an object_store backend (feature `oxkv-s3`):
// `OxKvStore::builder().with_store(s3).with_prefix(...)`
let kv = OxKvStore::builder()
    .with_store(store)
    .with_prefix(ObjectPath::from("my-app/oxkv"))
    .build()
    .await
    .unwrap();

kv.set_bytes("hello", b"world").await.unwrap();
assert_eq!(kv.get_bytes("hello").await.unwrap().as_deref(), Some(b"world".as_slice()));

// Transactions stage in an overlay and become durable only on commit (WAL RPO=0)
let tx = kv.begin_tx().unwrap();
tx.set_bytes("a", b"1").await.unwrap();
tx.commit().await.unwrap();
# }
```

What it does under the hood:

- **Probe** — on `build()` validates `If-None-Match` / `If-Match` conditional writes (`PutMode::Create` / `Update`) at `{prefix}/probe/canary`; use `.skip_probe(true)` only for stores known to be broken (e.g. B2).
- **Single-writer fencing** — `ownership.json` CAS at `{prefix}/ownership.json` bumps a monotonic `epoch`; every WAL/SST `PUT` is gated and returns `StoreError::Fenced` if superseded.
- **WAL (RPO=0)** — each `set_bytes`/`delete` stages a length-prefixed record (`[u32 key len][key][u32 value len][value]`, tombstone = `u32::MAX`); concurrent blind writes fuse via group commit into one `e{epoch:06}/wal/{seq:08}.log` file holding many framed records, then one manifest CAS appends the id. `MemTable` mutates only after WAL durable; replayed on `build()` (replay loops over records, so batched files need no format changes). Past ~200 WAL ids writes force-flush + GC, so the manifest list stays flat (store and tx paths alike). The bound is deliberately small: every solo write clones, serializes, and CAS-persists the whole list, so list length is a direct per-write tax multiplier.
- **`MemTable` → SST** — buffered writes flush to `e{epoch}/sst/L0/{seq}.sst` when >32 MiB (or forced). Large values overflow to `e{epoch}/blob/{sha256}` and the SST stores a pointer `(blob path, len, crc32)` instead. Reads resolve the pointer with length + CRC verification.
- **Manifest** — `manifest.json` tracks `{epoch, version, wal: [ids], sst: [{id, level, min_key, max_key, size}]}` and is updated via `If-Match` `ETag` CAS with jittered backoff. An in-memory `ManifestCache` (1 s TTL, never held across I/O) and `S3-FIFO` SST cache (256 MiB, weigher by file size, `moka` optional) avoid hot-path GETs. Every SST `GET` verifies `verify_file_crc()`. `.assume_single_writer(true)` skips revalidation polls on fresh cache entries (single-writer prefixes only).
- **GC & compaction** — `gc_wal()` deletes WAL covered by an SST once no reader pins that version (`register_reader`/`unregister_reader` watermark; with no pins all covered WAL is eligible). `compact()` merges L0→L1 when `L0 files ≥4` or `>128 MiB`, building a new `L1/{seq}.sst` with `BTreeMap` newest-wins dedup, CAS-swapping the manifest, then deleting old objects and invalidating the cache. Past 16 L1 files the smallest adjacent pair folds into the merge, bounding the SST list. Both are idempotent via `If-None-Match` + manifest dedup.
- **Read path** — `get_bytes` checks `MemTable` then SSTs newest-first within `[min_key, max_key]`; `gets_bytes` pull-merges `MemTable` + SSTs with tombstone suppression, lazily for forward scans (each file yields only what the page needs) and materialized for backward ones. Both deref blob pointers.

### SST cache

Point lookups go through a 256 MiB scan-resistant `S3-FIFO` cache (`LruCache`, weighed by file size via `SstFile::size`), so hot SSTs are parsed once. Window scans (`gets`) route through the same cache: rotating pages reuse the same files, and the small queue plus ghost absorb one-hit entries without evicting hot point-lookup data (`zipf_get` holds a 0.873 hit ratio either way). Scans also pull-merge lazily — each file yields only the entries the page needs instead of decoding to end-of-file — which took `page_fetch_100` at 1M keys from ~290 ms to ~150 µs.

Hit/miss statistics are tracked with atomics shared across clones: `Cache::stats()` returns hits, misses, inserts, capacity evictions, and `hit_ratio()` (backends that don't track, e.g. `moka`, return `None`). On a store, `OxKvStore::sst_cache_stats()` exposes the same numbers — the `zipf_get` bench prints them alongside timings.

The cache is generic over the `Cache` trait, so native builds needing sharded concurrency can swap in `moka` (admission-filtered `TinyLFU` + segmented LRU) or a custom implementation via `build_with_cache`:

```text
use std::sync::Arc;
use oxkv::{MemStorage, OxKvStore, SstFile};

// Requires oxkv with the `moka` feature (native-only).
let cache = moka::future::Cache::builder()
    .max_capacity(256 * 1024 * 1024)
    .weigher(|_: &String, v: &Arc<SstFile>| u32::try_from(v.size()).unwrap_or(u32::MAX))
    .build();
let kv = OxKvStore::builder()
    .with_store(Arc::new(MemStorage::new()))
    .build_with_cache(cache)
    .await?;
```

Enable it:

```toml
[dependencies]
oxkv = { version = "0.7", features = ["oxkv"] }           # portable LSM core
# oxkv = { version = "0.7", features = ["oxkv", "oxkv-s3"] } # + S3/GCS/Azure via object_store (native)
```

`cargo test --features oxkv` and `cargo bench --features oxkv` exercise it against `MemStorage` (bench uses `skip_probe(true)` so the numbers are comparable to `btree_mem`/`oxkv_mem`).

### Sharing stores across threads

Every store is `Send + Sync` with `&self` operations, so one store can live
in an `Arc` and serve concurrent callers — each task starts its own
transaction via the shared `begin_tx`, and standalone writes serialize
behind a fair write gate instead of CAS-retry-storming the manifest. (The
example uses a current-thread runtime so it runs everywhere including wasm;
on native servers you would typically use a multi-thread runtime.)

```rust
use std::sync::Arc;
use oxkv::{GetSet, MemStorage, ObjectPath, OxKvStore};

fn main() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime")
        .block_on(async {
            let kv = Arc::new(
                OxKvStore::builder()
                    .with_store(Arc::new(MemStorage::new()))
                    .with_prefix(ObjectPath::from("concurrent-doc"))
                    .build()
                    .await
                    .unwrap(),
            );
            let (a, b) = (Arc::clone(&kv), Arc::clone(&kv));
            // No `tokio::spawn`: `join!` polls both futures without a
            // runtime handle, so sharing works on any runtime.
            tokio::join!(
                async move { a.set_bytes("a", b"1").await.unwrap() },
                async move { b.set_bytes("b", b"2").await.unwrap() },
            );
            assert_eq!(
                kv.get_bytes("a").await.unwrap().as_deref(),
                Some(b"1".as_slice())
            );
        });
}
```

Concurrent blind writes batch further via group commit: whoever takes the
gate persists everything staged (up to 256 records) in one WAL file plus
one manifest CAS, and every entry in the batch shares one fate — all
succeed or all fail together. `delete` and transaction commits take the
gate without staging and ride it exclusively.

### Multi-reader

One writer serves many readers via `OxKvReader`, which opens a prefix
without the storage probe or the `ownership.json` epoch CAS — opening one
never fences the writer, so concurrent opens are safe. It implements the
same `Store` trait, with every write rejected at runtime and an empty
transaction commit succeeding as a no-op.

```rust
# #[tokio::main(flavor = "current_thread")]
# async fn main() {
use std::sync::Arc;
use oxkv::{GetSet, MemStorage, ObjectPath, OxKvReader, OxKvStore, Storage};

let backend: Arc<dyn Storage> = Arc::new(MemStorage::new());
let writer = OxKvStore::builder()
    .with_store(Arc::clone(&backend))
    .with_prefix(ObjectPath::from("multi-doc"))
    .build()
    .await
    .unwrap();
writer.put_bytes("hello", b"world").await.unwrap();

let reader = OxKvReader::open(Arc::clone(&backend), ObjectPath::from("multi-doc"))
    .await
    .unwrap();
assert_eq!(
    reader.get_bytes("hello").await.unwrap().as_deref(),
    Some(b"world".as_slice())
);
// Post-open writes appear on the next read — no reopen, no SST flush needed.
writer.put_bytes("late", b"data").await.unwrap();
assert_eq!(
    reader.get_bytes("late").await.unwrap().as_deref(),
    Some(b"data".as_slice())
);
# }
```

Freshness follows the manifest: every read revalidates it and replays newly
listed WAL files, so a write is visible as soon as its manifest CAS lands.
The replay overlay is bounded to the live WAL window — a fully collected
list means all records are SST-covered, so the overlay is dropped. A writer
takeover (new epoch) needs no reader restart, and a scan racing a
compaction reloads the manifest and retries once instead of failing on the
deleted file.

### RAM mirror

When RAM is plentiful, `CachedOxKvStore` pairs the durable core with a
`BTreeStore` holding every resolved key: reads serve from memory while
writes keep WAL durability. Plain reads are zero-I/O once warmed;
the `*_checked` variants revalidate against a staleness bound first.
Catch-up derives from the manifest version plus SST set (WAL appends replay
incrementally, a new ownership epoch or a missed flush window rebuilds),
and `ownership.json` is read only on suspected takeover. A write rejected
with `Fenced` poisons the mirror — reads fail until `refresh` adopts the
new owner. Both store types are `Clone`, so the mirror composes with
`HookStore` and `OtelStore`.

How the mirror fills is a `WarmMode` chosen at open (`Eager` by default):
`Background` returns immediately and converges through explicitly driven
`warm`/`warm_step` scans (drive them from any task or timer), while `Lazy`
skips scanning entirely and fills key-by-key on read misses — hot keys
arrive first and untouched keys cost no RAM. Until warmed, point reads fall
through to the core with fill and scans delegate wholly, so unwarmed
handles stay correct; `is_warmed` reports convergence.

```rust
# #[tokio::main(flavor = "current_thread")]
# async fn main() {
use std::sync::Arc;
use oxkv::{GetSet, MemStorage, ObjectPath, OxKvStore};

let cached = OxKvStore::builder()
    .with_store(Arc::new(MemStorage::new()))
    .with_prefix(ObjectPath::from("mirror-doc"))
    .skip_probe(true)
    .build_cached()
    .await
    .unwrap();
cached.put_bytes("hello", b"world").await.unwrap();
assert_eq!(
    cached.get_bytes("hello").await.unwrap().as_deref(),
    Some(b"world".as_slice())
);
assert!(!cached.check_stale().await.unwrap());
# }
```

## WASM Bindings

The WASM module in `src/wasm/` provides thread-safe wrappers for `BTreeStore`, `OxKvStore` (in-memory LSM), and `CachedOxKvStore` (write-through RAM mirror), exposing every store method to JavaScript as async promises. Snapshots are byte-identical across backends, so bytes saved anywhere restore anywhere.

### Persistent browser storage (OPFS)

`OxKvStore.create()` is memory-only. For data that survives page reloads, open the same engine on origin private storage instead — same API, real files, content-hash etags so fencing and CAS work across sessions:

```js
import init, { OxKvStore } from "./pkg/oxkv.js";

await init();
const durable = await OxKvStore.createPersistent("my-app");
await durable.set("user1", { name: "Ada" });

// ...reload the page — the data is still there:
const reopened = await OxKvStore.createPersistent("my-app");
console.log(await reopened.get("user1")); // { name: "Ada" }
```

Main-thread only (async OPFS handles); cross-tab races resolve last-writer-wins, since OPFS offers no conditional-write primitive on the main thread.

### Cached reads in the browser

`CachedOxKvStore` mirrors every key in memory with the same API plus
staleness knobs — pass a TTL in milliseconds at creation, then poll or
catch up when another tab may have taken the epoch:

```js
import init, { CachedOxKvStore } from "./pkg/oxkv.js";

await init();
const hot = await CachedOxKvStore.create("my-app", 1000);
await hot.set("user1", { name: "Ada" });

if (await hot.checkStale()) {
  await hot.refresh();
}
```

Pass `"background"` or `"lazy"` as the third `create` argument for instant
startup with later convergence (`warm`/`warmStep`, polled via `isWarmed`):

```js
const warming = await CachedOxKvStore.create("my-app", 1000, "background");
await warming.set("user1", { name: "Ada" }); // served immediately
const done = await warming.warmStep(16); // drive from a timer until true
```

Build for WebAssembly:

```bash
wasm-pack build --target web  # or nodejs, bundler, etc.
```

### Querying from JavaScript

```js
import init, { BTreeStore } from "./pkg/oxkv.js";

await init();
const store = new BTreeStore();

await store.set("user1", { name: "Ada", age: 36, tags: ["math"] });
await store.set("user2", { name: "Alan", age: 41, tags: ["code"] });

// Paginated JSON retrieval with an optional Lucene-style query
const results = await store.gets(
    10,
    Direction.Next,
    null,   // start cursor
    null,   // end cursor
    "age:[30 TO 40] AND tags:math",
);
for (const { key, value } of results) {
    console.log(key, value); // value is the parsed JSON document
}
```

### LSM from JavaScript

`OxKvStore` exposes the same API over the LSM engine — in-memory via `create()`,
durable across reloads via `createPersistent()` (OPFS). Snapshots stay portable:
bytes saved by either class restore into the other.

```js
import init, { OxKvStore } from "./pkg/oxkv.js";

await init();
const lsm = await OxKvStore.create();
await lsm.set("user1", { name: "Ada" });

const durable = await OxKvStore.createPersistent("my-app");
await durable.setBytes("k", new TextEncoder().encode("v"));
```

### Streaming Snapshots from JavaScript

Both `BTreeStore` and `OxKvStore` expose `saveStream()`/`loadStream()`.
Snapshot export/import works over native JS streams — pipe straight to a file,
network upload or `IndexedDB` without buffering the whole store:

```js
// Export: ReadableStream of Uint8Array chunks
const stream = store.saveStream();
await stream.pipeTo(WritableStream.from(await fileHandle.createWritable()));

// Import: any ReadableStream of Uint8Array chunks; chunk boundaries are
// arbitrary and nothing is committed unless the entire payload decodes
const fileStream = (await fileHandle.getFile()).stream();
const count = await store.loadStream(fileStream);
console.log(`restored ${count} entries`);
```

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (latest stable)
- [`wasm-pack`](https://rustwasm.github.io/wasm-pack/installer/) — for building and packaging the WASM module
- For testing: [Node.js](https://nodejs.org/) (`mise.toml` manages this automatically)

## Testing

### Native Tests

```bash
cargo test
```

### Wasm Tests (wasm-bindgen-test)

Requires Node.js.

```bash
# Build for WASM with wasm-bindgen-test support
wasm-pack build --target web

# Run tests using the Node.js runtime
wasm-pack test --node
```

## Benchmarking

Criterion benchmarks live in [`benches/kv_bench.rs`](benches/kv_bench.rs) and
cover `btree_mem`, `oxkv_mem` (LSM over in-memory `MemStorage` with `skip_probe(true)`), and `cached_mem` (same core via `build_cached`, read shapes only) at 1K / 100K / 1M scales with a sampling strategy that keeps the full suite in minutes, not hours. `btree` is bench baseline only.

```bash
cargo bench --bench kv_bench                     # everything (tuned to minutes)
cargo bench --bench kv_bench 1000                # quick sweep of the 1k groups
cargo bench --bench kv_bench point_update        # just the changes matrix
cargo bench --bench kv_bench random_get/cached_mem/1000/get  # one cached cell
cargo bench --features oxkv --bench kv_bench       # include OxKv (MemStorage)
```

Query-engine benchmarks live in [`benches/query_bench.rs`](benches/query_bench.rs)
and measure the Lucene-style parser and matcher in isolation — no backend
store involved:

```bash
cargo bench --bench query_bench                      # all query benches
cargo bench --bench query_bench query_parse          # parsing only, per feature
cargo bench --bench query_bench query_match/1000docs # matching a 1k-doc corpus
cargo bench --bench query_bench query_match/regex    # one query kind, both corpora
```

`query_parse/{kind}` parses one representative query string per engine
feature; `query_match/{kind}/{n}docs` evaluates a pre-parsed query over a
generated corpus of 1,000 or 100,000 JSON documents.

The filter is a plain substring match on benchmark names. Always run through
`cargo bench` (which passes `--bench` and selects measurement mode): invoking
the compiled binary directly only smoke-tests each routine once without
recording stats. Results land in
`target/criterion/` as HTML reports; re-running a filter compares against the
previous run and flags regressions/improvements automatically. For A/B work
across commits, save a named baseline on the base commit and compare the
contender against it; baselines live in gitignored `target/`, so they never
leave your machine:

  ```bash
  cargo bench --bench kv_bench -- --save-baseline base   # on the base commit
  cargo bench --bench kv_bench -- --baseline base        # on the contender
  ```

### Workloads

| Group | Scale | Measures |
| --------- | ------- | ----------- |
| `seq_insert/{backend}/{n}` | 1K, 100K | building a store from scratch — every key inserted sequentially (`oxkv` via blind `put_bytes` writes) |
| `seq_delete/{backend}/{n}` | 1K, 100K (cap 100K) | deleting every key from a freshly built store (cap keeps per-iteration rebuilds sane) |
| `random_get/{backend}/{n}` | 1K, 100K, 1M | scattered reads (prime-stride order); at 1M each iteration samples 10K gets out of a 1M-key store so depth is preserved without 1M GETs per iteration; `cached_mem` reads serve from the mirror |
| `page_fetch_100/{backend}/{n}` | 1K, 1M | one paginated range fetch of 100 entries from rotating start cursors; `cached_mem` scans the mirror |
| `point_update/{backend}/{n}items_{m}changes` | 1K×{1,10}, 1M×{1,100,1000} | in-place updates of a few keys inside a large store (oxkv setup quiesced: force-flush, WAL GC, and compaction drain before timing, so iterations measure updates instead of the populate backlog) |
| `tx_commit_batch_1000/{backend}` | 1K | committing a pre-staged 1,000-write transaction (staging is untimed, so this isolates durability cost) |
| `concurrent_write/oxkv_mem/1024` | 1,024 writes (8 tasks × 128) | blind writes from spawned tasks sharing one store (multi-thread runtime): write-gate + group-commit throughput; store rebuilt per iteration in untimed setup |
| `zipf_get/oxkv_mem/10000` | 10K keys, 2K reads/iter | end-to-end skewed reads (Zipf 1.07, fixed seed) over the SSTs from 50 forced flushes — background compaction folds those into roughly a dozen larger files, so the 320 KiB cache holds a mid-range fraction. Whole-SST admission with real parse costs: guards the `fetch_sst` wiring (bypass/invalidation regressions collapse this ratio while `cache_zipf` stays green). Hit ratio printed to stderr. |
| `concurrent_random_get/oxkv_mem/100000` | 100K keys, 10K reads split over 8 tasks | shared-store point reads from spawned tasks (multi-thread runtime, store populated once and reused): read-path scaling while writes serialize |
| `write_stage/oxkv_mem/10000` | 10K staged writes | staged-only writes (mem + WAL buffer, no durability): in-memory floor of the write path |
| `write_wal_put/oxkv_mem/10000` | 10K PUTs | raw `MemStorage` conditional PUTs of WAL-sized payloads under fresh keys: per-write storage cost without LSM work above |
| `write_flush_check/oxkv_mem/{n}staged` | 1K/4K/16K staged entries | one non-force flush check over N staged entries (always `None` below the 32 MiB threshold): size-estimate cost curve; full `put_bytes` ≈ stage + `wal_put` + residual (ownership, manifest, maintenance) |

Cache micro-benchmarks live in [`benches/cache_bench.rs`](benches/cache_bench.rs)
and isolate the [`Cache`](src/store/cache.rs) trait itself (the end-to-end
benches above mostly measure miss paths). Fixed 64-byte values, capacity for
4,096 entries; every group runs against built-in `S3-FIFO` (`lru`) and, with
`--features moka`, the optional backend (`moka`) for A/B comparison:

| Group | Scale | Measures |
| --------- | ------- | ----------- |
| `cache_insert/{impl}/4096` | 4K inserts | insert + eviction-churn throughput |
| `cache_get_hit/{impl}/10000` | 10K gets | pure hit-path reads over resident keys |
| `cache_get_miss/{impl}/10000` | 10K gets | pure miss-path reads over absent keys |
| `cache_zipf/{impl}/16384` | 16K keys (4x capacity), 4K reads/iter | read-through skewed reads (Zipf 1.07, fixed seed): isolated admission-policy quality per impl (hit ratio printed once to stderr); the end-to-end wall-clock counterpart is `zipf_get` above |

Scale strategy (see `benches/kv_bench.rs` header): full-scan writes (`seq_insert`, `seq_delete`) scale linearly so they run at 1K+100K only — 1M depth is still exercised via `random_get`/`point_update`/`page_fetch`, whose per-iteration work is bounded (sampled reads / one page / few updates) against a 1M-key store built once and reused. Tree/SST depth and index size are identical to a full 1M scan; only the repeated per-iteration cost is removed. The oxkv-only groups (`concurrent_write`, `zipf_get`, `concurrent_random_get`) are fixed-scale by design: they measure threading and cache behavior, not scaling.

Setup work (populating stores for read/update benchmarks, staging
transactions) runs in untimed warmup or setup phases, so measured numbers
count only the operation under test. `cached_mem` needs no extra warm step:
the mirror fills synchronously on every write, so populating the store warms
it. Write groups are not repeated for `cached_mem` — its writes share
`oxkv_mem`'s durable path plus one B-tree insert each, which the other
groups already cover.

Indicative read numbers (one machine, release profile) put the mirror next
to `btree_mem` instead of `oxkv_mem`: at 1K, `random_get` measures ~219 µs
(4.6 M/s) against 160 µs for btree and 6.7 ms for oxkv, and `page_fetch_100`
measures ~8.2 µs — on par with btree's 7.8 µs. At 1M depth, `random_get`
samples 10K gets in ~14 ms (726 K/s) versus ~103 ms for oxkv (~7.5×) and
~10 ms for btree. Re-run on your own hardware before citing.

### Runtime notes

- 1K groups complete in ~tens of seconds; 100K full-scan groups use 10 samples with 2 s warmup / 5 s measurement; 1M sampled groups use 10 samples with 2 s warmup / 10 s measurement — the full suite stays in minutes.
- Throughput is reported as `Elements` = ops per iteration (so `random_get/oxkv_mem/1000000` reports 10K, not 1M; `zipf_get` reports 2K, `concurrent_write` 1,024, `concurrent_random_get` 10K).

  ```bash
  cargo bench --bench kv_bench seq_insert/oxkv_mem/100000
  ```

## Building for WebAssembly

```bash
wasm-pack build --target web   # or nodejs, bundler, etc.
```

## Architecture

- `src/wasm/` — manual wasm-bindgen wrappers (`mod` + `btree` + `oxkv` + `cached`) for `BTreeStore` + `OxKvStore` + `CachedOxKvStore` (thread-safe JS-facing types; OXKV snapshot portable across all backends)
- `src/store/mod.rs` — core traits (`GetSet`, `Transaction`, `Store`, `GetSetExt`, `StoreExt`), error types (`StoreError::Fenced`, `StoreError::CasConflict`, `StoreError::NotModified`), and the `lock_ignore_poison` policy helper
- `src/store/btree.rs` — in-memory B-tree backend (`btree`, test/bench + WASM baseline)
- `src/store/lsm/mod.rs` — LSM facade (`oxkv`, native + wasm): re-exports `OxKvStore`/`OxKvTx`/`OxKvReader`/`CachedOxKvStore`/`CachedTx`/`WarmMode`/`SstFile`, shared `MemTable` types + WAL/GC tuning constants
- `src/store/lsm/store.rs` — single-writer `OxKvStore`/`OxKvStoreBuilder`: probe + epoch fencing, WAL gate + group commit, flush/GC/compaction
- `src/store/lsm/tx.rs` — `OxKvTx` staged-overlay transactions (read-your-writes, durable only on commit)
- `src/store/lsm/cached.rs` — `CachedOxKvStore`/`CachedTx` write-through RAM mirror (`Eager`/`Background`/`Lazy` fill, read-through misses, TTL-checked reads, fencing poison)
- `src/store/lsm/reader.rs` — read-only `OxKvReader`/`OxKvRoTx` followers (no epoch CAS, WAL-following reads, stale-SST retry)
- `src/store/lsm/read.rs` — shared point/scan lookup engine (manifest load, SST fetch, blob deref, newest-wins)
- `src/store/lsm/merge.rs` — newest-wins merge algebra (dedup + tombstone suppression, lazy pull-merge scans)
- `src/store/lsm/wal.rs` — WAL record decoding + startup/WAL replay shared by writer and reader
- `src/store/lsm/sst.rs` — SST file format (blocks, Bloom filter, CRC32) with a lazy block-pruned scan iterator
- `src/store/lsm/blob.rs` — blob overflow for large values (`e{epoch}/blob/{hash}` with CRC)
- `src/store/lsm/manifest.rs` — `manifest.json` with `ETag` CAS and `ManifestCache`
- `src/store/lsm/ownership.rs` — `ownership.json` epoch fencing
- `src/store/lsm/probe.rs` — conditional-write probe (`If-None-Match` / `If-Match`)
- `src/store/storage.rs` — `Storage` trait + `MemStorage` (in-memory, every target) + `object_store` impl (S3/memory/local, native `oxkv-s3`) + `OpfsStorage` (browser origin-private storage, wasm)
- `src/store/cache.rs` — `Cache` trait + `LruCache`/`S3FifoCache` (scan-resistant `S3-FIFO`, `moka` optional)
- `src/store/hooks.rs` — `HookStore` decorator providing validation hooks and change notifications
- `src/store/otel.rs` — `OtelStore` decorator emitting OpenTelemetry spans and metrics (feature `otel`)
- `src/query/mod.rs` — query AST types and the pest-based parser (`query/query.pest` grammar)
- `src/query/json.rs` — compiled-query matcher evaluating queries against `serde_json::Value` documents

## License

Dual-licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
