# Migrating from 0.12 to 1.0

OxKV 1.0 removes the `/regex/` term from the query filter. The 1.0 search
language has no regular expressions. A query with `/regex/` fails with
`policy.regex_not_supported`.

Before:

```rust
let rows = store
    .gets(None, Direction::Next, (None, None), Some(r"id:/doc-\d+/"))
    .await?;
```

After:

```rust
let rows = store
    .gets(None, Direction::Next, (None, None), Some("id:doc-*"))
    .await?;
```

The `*` and `?` wildcard forms still work. The `oxkv-index` and `oxkv-search`
crates hold the 1.0 search contract. The `oxkv-query` filter is a legacy
convenience layer. All workspace crates release in lockstep at 1.0.0.

# Migrating from 0.11 to 0.12

OxKV 0.12 changes three existing signatures and adds new APIs. The sections
below show the old shape and the new shape.

All workspace crates release in lockstep at 0.12.0. Update the core crate and
every adapter crate together.

## 1. Scan limits use `usize`

`GetSet::gets_bytes`, `GetSet::gets_keys`, `oxkv_query::QueryExt::gets`, and
`oxkv_query::QueryViewExt::gets` now take `Option<usize>`.

Before:

```rust
let page = store
    .gets_bytes(Some(100u32), Direction::Next, (None, None))
    .await?;
```

After:

```rust
let page = store
    .gets_bytes(Some(100usize), Direction::Next, (None, None))
    .await?;
```

The JavaScript and WASM boundary still takes `Option<u32>`. wasm-bindgen does
not support `usize`, so the binding converts the value at the boundary. Remove
any `as u32` cast from a Rust call site. A limit that fits in `u32` behaves as
it did in 0.11.

## 2. `StoreError::Storage` holds a structured cause

`StoreError::Storage` now holds a `StorageError` value. The value has a
`message` field and a `cause` field.

Before:

```rust
match error {
    StoreError::Storage(message) => eprintln!("storage: {message}"),
    other => return Err(other),
}
```

After:

```rust
match error {
    StoreError::Storage(storage) => {
        eprintln!("storage: {} ({:?})", storage.message, storage.cause)
    }
    other => return Err(other),
}
```

`StoreError::storage_cause()` returns `Option<StorageCause>`. `StorageCause` is
`NotFound`, `Network`, `Io`, `Corrupt`, `LeaderLost`, or `Other`. Every cause
still maps to `ErrorKind::Transient`, so retry classification does not change.
`StoreError::storage(message)` and
`StoreError::storage_with_cause(message, cause)` build the new value.

## 3. `StoreError::TxConflict` is new

`TxCas` reports a failed precondition at commit with
`StoreError::TxConflict { key, current }`. An exhaustive `match` on
`StoreError` must add this variant. The variant maps to
`ErrorKind::Conflict` and is retryable. On a conflict, no staged write
applies.

## 4. New APIs

- `TxCas` adds `set_if_absent`, `compare_and_set`, and `delete_if` to a
  transaction. Each method stages a write and a precondition. `commit` checks
  the preconditions against committed state.
- `StoreExt::snapshot` returns a read-only `Snapshot`. The call copies every
  key and value one time. The copy uses memory equal to the store size.
- `ScanExt::scan_older_than` returns keys below an exclusive cutoff in one
  bounded `Page<String>`.
- `BatchExt::delete_prefix_before` deletes keys below an exclusive cutoff in
  chunks. A failure between chunks leaves the earlier chunks deleted.
- `ChangeFeed` and `FeedEntry` provide a durable append-only event log with a
  cursor and chunked pruning. Delivery is at-least-once.
- `StatsStore` and `StoreStats` provide client-side operation counters.
  `stats`, `reset`, and `into_inner` manage the counters.
- `ScopedStore` and `StoreExt::scoped` map every key to `{prefix}/{key}` and
  keep scans inside the scope.
- `StoreExt::load_chunked` and `load_stream_chunked` commit one transaction
  every `page_size` records. A failure after a commit leaves the earlier pages
  committed.
- `ChangeFeed::append_in_tx` now requires `T: Transaction + Sync`. It uses
  `put_bytes` because the event key is new.

## 5. Batch helper names

`BatchExt::delete_prefix_chunked` is the current name for the chunked delete.
`BatchExt::delete_prefix` remains as a deprecated alias. Both commit one chunk
per transaction and neither is atomic across chunks. Call
`delete_prefix_before` when the cutoff is a key bound.

# Migrating from 0.10 to 0.11

OxKV 0.11 moves three public items from the core crate to separate crates. This
guide shows the old shape and the new shape.

All workspace crates release in lockstep. The version of each adapter crate
matches the version of `oxkv`.

## 1. The builder takes a `Storage` value

`OxKvStoreBuilder::with_object_store` is gone. Use `with_store` with an
`ObjectStoreStorage` value.

Before:

```rust
use oxkv::OxKvStore;

let kv = OxKvStore::builder()
    .with_object_store(s3)
    .build()
    .await?;
```

After:

```rust
use std::sync::Arc;
use oxkv::{OxKvStore, Storage};
use oxkv_s3::ObjectStoreStorage;

let backend: Arc<dyn Storage> = Arc::new(ObjectStoreStorage::new(s3));
let kv = OxKvStore::builder()
    .with_store(backend)
    .build()
    .await?;
```

Add the adapter crate:

```console
cargo add oxkv-s3
```

## 2. Cached mirror types live in `oxkv-cached`

`oxkv::CachedOxKvStore` and `oxkv::WarmMode` are gone from the core crate. Use
`oxkv_cached::CachedOxKvStore` and `oxkv_cached::WarmMode`.

Before:

```rust
use oxkv::{CachedOxKvStore, WarmMode};

let cached = CachedOxKvStore::open_with_mode(writer, WarmMode::Lazy).await?;
```

After:

```rust
use oxkv_cached::{CachedOxKvStore, WarmMode};

let cached = CachedOxKvStore::open_with_mode(writer, WarmMode::Lazy).await?;
```

Add the adapter crate:

```console
cargo add oxkv-cached
```

## 3. OpenTelemetry lives in `oxkv-otel`

`oxkv::OtelStore` is gone. The core `otel` feature is also gone. Use
`oxkv_otel::OtelStore`.

Before:

```toml
[dependencies]
oxkv = { version = "0.10", features = ["otel"] }
```

```rust
use oxkv::OtelStore;

let store = OtelStore::new(inner);
```

After:

```toml
[dependencies]
oxkv = "0.11"
oxkv-otel = "0.11"
```

```rust
use oxkv_otel::OtelStore;

let store = OtelStore::new(inner);
```

## Crate compatibility

All workspace crates release in lockstep. Each adapter crate uses the same
version as `oxkv`.

| Crate | Purpose | Version |
| --- | --- | --- |
| `oxkv` | Core traits, the B-tree backend, and the LSM backend | Core crate |
| `oxkv-query` | Lucene-style query engine for stored JSON documents | Matches `oxkv` |
| `oxkv-opfs` | Browser OPFS storage backend | Matches `oxkv` |
| `oxkv-s3` | S3, GCS, Azure, and local filesystem storage with `object_store` | Matches `oxkv` |
| `oxkv-otel` | OpenTelemetry span and metric decorator | Matches `oxkv` |
| `oxkv-cached` | Write-through RAM mirror | Matches `oxkv` |
| `oxkv-wasm` | WebAssembly bindings for JavaScript | Matches `oxkv` |
