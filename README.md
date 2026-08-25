
# oxkv

A transactional key-value store library written in Rust, with optional WebAssembly bindings for JavaScript interop. Features cursor-based pagination, a Lucene-style query engine that matches stored JSON documents, JSON serialization via `serde_json`, and strict linting. All operations are async using `futures::lock::Mutex` to enable concurrent access from WASM call sites.

## Features

- **Transaction support** — atomic commit/rollback batches of CRUD operations
- **Cursor-based pagination** — bidirectional traversal (`Next` / `Prev`) with inclusive range cursors and limit control
- **Lucene-style query engine** — filter stored JSON documents with a query language supporting field paths, ranges, wildcards, regex, fuzzy matching, and boolean operators
- **JSON serialization** — extension methods for inserting and retrieving `serde_json::Value` types via JSON, stored as raw bytes
- **WASM bindings** — thread-safe wrappers in `src/wasm.rs` expose every store method to JavaScript as async promises
- **Extensible backends** — the crate defines three traits (`GetSet`, `Transaction`, `Store`) that any backend can implement; ships with an in-memory B-tree backend and a persistent [Redb](https://github.com/cberner/redb) backend
- **Validation hooks** — reject invalid writes before they reach storage, scoped to a single key, a key prefix, or the whole store
- **Reactivity** — watch keys or prefixes and observe every committed change via channels or observer traits; rolled-back transactions never notify
- **Save/Load** — serialize the entire store contents into a single contiguous `Uint8Array` and reconstruct it from binary data
- **Strict linting** — all warnings and clippy lints are enforced at the crate level

## Core Traits

| Trait | Purpose |
| ------- | --------- |
| [`store::GetSet`] | Basic key-value operations: `get_bytes`, `set_bytes`, `delete`, `has` (paginated via `gets_bytes`) |
| [`store::Transaction`] | Extends `GetSet` with `commit` and `rollback` for atomic batches |
| [`store::Store`] | Extends `GetSet` with `begin_tx` — starts a write transaction |
| [`store::GetSetExt`] | Convenience methods: `set`, `get` (JSON-serialized) and `gets` (paginated JSON retrieval with optional query filtering) |
| [`store::StoreExt`] | Save/Load the entire store contents as binary |
| [`store::Validator`] | Validates writes before they are stored (attach per key, prefix, or globally) |
| [`store::Observer`] | Receives change notifications after they become durable |
| [`store::HookStore`] | Decorator adding validators and change watching to any store |

## Quick Start

```rust,ignore
use oxkv::{BTreeStore, store::*};

#[tokio::main]
async fn main() {
    let mut store = BTreeStore::default();

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
    let mut tx = store.begin_tx().unwrap();
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

```rust,ignore
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
```

### Query Syntax

| Feature | Example | Notes |
| --------- | --------- | ------- |
| Plain term (case-insensitive) | `rust` | matches any leaf value anywhere in the document |
| Field-scoped term | `lang:rust` | dot-separated paths descend into objects (`address.city:Berlin`) and fan out across arrays (`tags:kv`) |
| Quoted phrase (exact, case-sensitive) | `title:"Rust Programming"` | whole-value equality |
| Wildcards | `name:r*`, `j?va` | `*` and `?`, case-insensitive |
| Regex | `email:/@gmail\.com$/` | Rust `regex` crate syntax |
| Fuzzy | `name:Jon~1` | Levenshtein distance ≤ slop; bare `~` defaults to 2 |
| Boost | `rust^2.5` | parsed but ignored for boolean matching |
| Inclusive range | `age:[30 TO 40]` | numeric bounds also match numeric-looking strings |
| Exclusive range | `date:{2020 TO 2024}` | lexicographic comparison for non-numeric values |
| Boolean operators | `a AND b OR c` | `AND` binds tighter than `OR`; `&&`, `\|\|` aliases; a missing operator defaults to `OR` |
| Occurrence prefixes | `+required -excluded NOT banned` | without explicit operators: all `+` must match, no `-`/`NOT` may match, at least one optional clause must match |
| Sub-queries | `(rust OR go) AND stars:>0` | parenthesized groups, optionally field-scoped (`tags:(rust OR go)`) |
| Escapes | `a\.b:x` | `\.` addresses a key containing a literal dot; escapes work in terms and field names |

Invalid queries return a `StoreError::Other`; entries whose values are not
valid JSON are skipped during scans (or returned untouched by pass-through
calls without a query).

## Hooks and Reactivity

Wrap any backend in a `HookStore` to validate values before they are stored
and to listen for changes:

```rust,ignore
use oxkv::{BTreeStore, HookStore, Scope};
use oxkv::store::{ChangeEvent, ChangeKind, Scope, Validator};

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
```

- Validators run before every write, including transactional staging; an
  error rejects the write without touching the underlying store. Validators
  receive a read-only `StoreView` so rules can compare against other keys �
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

## WASM Bindings

The WASM module in `src/wasm.rs` provides thread-safe wrappers for `BTreeStore`, exposing every store method to JavaScript as async promises.

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
cover both shipped backends (`btree_mem`, `redb_mem`) at two store sizes:
1,000 and 1,000,000 items.

```bash
cargo bench --bench kv_bench                     # everything (slow - see note)
cargo bench --bench kv_bench 1000                # quick sweep of the 1k groups
cargo bench --bench kv_bench point_update        # just the changes matrix
cargo bench --bench kv_bench 1000000items_100    # one specific cell of the matrix
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

The filter is a plain substring match on benchmark names. Results land in
`target/criterion/` as HTML reports; re-running a filter compares against the
previous run and flags regressions/improvements automatically.

### Workloads

| Group | Measures |
| --------- | ----------- |
| `seq_insert/{backend}/{n}` | building a store from scratch — every key inserted sequentially |
| `random_get/{backend}/{n}` | reading every item in scattered (prime-stride) order |
| `page_fetch_100/…` | one paginated range fetch of 100 entries from rotating start cursors |
| `point_update/{backend}/{n}items_{m}changes` | updating an existing store: 1k-item stores take 1 and 10 changes; 1M-item stores take 1, 100, and 1,000 |
| `tx_commit_batch_1000/…` | committing a pre-staged 1,000-write transaction (staging is untimed, so this isolates durability cost) |
| `seq_delete/{backend}/{n}` | deleting every key from a freshly built store (capped at 100k to keep per-iteration rebuilds sane) |

Setup work (populating stores for read/update benchmarks, staging
transactions) runs in untimed warmup or setup phases, so measured numbers
count only the operation under test.

### Runtime notes

- The 1,000-item groups complete in about a minute total.
- The 1M btree groups take a few minutes each.
- `seq_insert/redb_mem/1000000` is **very slow** (every one of the 1M writes
  commits individually, which is oxkv's durability contract). Run it
  deliberately via its filter when you want that number:

  ```bash
  cargo bench --bench kv_bench seq_insert/redb_mem/1000000
  ```

## Building for WebAssembly

```bash
wasm-pack build --target web   # or nodejs, bundler, etc.
```

## Architecture

- `src/wasm.rs` — manual wasm-bindgen wrappers for `BTreeStore` (thread-safe JS-facing types)
- `src/store/mod.rs` — core traits (`GetSet`, `Transaction`, `Store`, `GetSetExt`, `StoreExt`) and error types
- `src/store/btree.rs` — in-memory B-tree backend with transaction overlay support
- `src/store/redb.rs` — persistent backend built on Redb with transaction isolation
- `src/store/hooks.rs` — `HookStore` decorator providing validation hooks and change notifications
- `src/query/mod.rs` — query AST types and the pest-based parser (`query/query.pest` grammar)
- `src/query/json.rs` — compiled-query matcher evaluating queries against `serde_json::Value` documents

## License

Dual-licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
