

# oxkv

A transactional key-value store library written in Rust, with optional WebAssembly bindings for JavaScript interop. Features cursor-based pagination, a Lucene-style query engine that matches stored JSON documents, JSON serialization via `serde_json`, and strict linting. All operations are async using `futures::lock::Mutex` to enable concurrent access from WASM call sites.

## Features

- **Transaction support** — atomic commit/rollback batches of CRUD operations
- **Cursor-based pagination** — bidirectional traversal (`Next` / `Prev`) with inclusive range cursors and limit control
- **Lucene-style query engine** — filter stored JSON documents with a query language supporting field paths, ranges, wildcards, regex, fuzzy matching, and boolean operators
- **JSON serialization** — extension methods for inserting and retrieving `serde_json::Value` types via JSON, stored as raw bytes
- **WASM bindings** — thread-safe wrappers in `src/wasm.rs` expose every store method to JavaScript as async promises
- **Extensible backends** — the crate defines three traits (`GetSet`, `Transaction`, `Store`) that any backend can implement; ships with an in-memory B-tree backend and a persistent [Redb](https://github.com/cberner/redb) backend
- **Save/Load** — serialize the entire store contents into a single contiguous `Uint8Array` and reconstruct it from binary data
- **Strict linting** — all warnings and clippy lints are enforced at the crate level

## Core Traits

| Trait | Purpose |
|-------|---------|
| [`store::GetSet`] | Basic key-value operations: `get_bytes`, `set_bytes`, `delete`, `exists` (paginated via `gets_bytes`) |
| [`store::Transaction`] | Extends `GetSet` with `commit` and `rollback` for atomic batches |
| [`store::Store`] | Extends `GetSet` with `begin_tx` — starts a write transaction |
| [`store::GetSetExt`] | Convenience methods: `set`, `get` (JSON-serialized) and `gets` (paginated JSON retrieval with optional query filtering) |
| [`store::StoreExt`] | Save/Load the entire store contents as binary |

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
|---------|---------|-------|
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

## Building for WebAssembly

```bash
wasm-pack build --target web   # or nodejs, bundler, etc.
```

## Architecture

- `src/wasm.rs` — manual wasm-bindgen wrappers for `BTreeStore` (thread-safe JS-facing types)
- `src/store/mod.rs` — core traits (`GetSet`, `Transaction`, `Store`, `GetSetExt`, `StoreExt`) and error types
- `src/store/btree.rs` — in-memory B-tree backend with transaction overlay support
- `src/store/redb.rs` — persistent backend built on Redb with transaction isolation
- `src/query/mod.rs` — query AST types and the pest-based parser (`query/query.pest` grammar)
- `src/query/json.rs` — compiled-query matcher evaluating queries against `serde_json::Value` documents

## License

MIT OR Apache-2.0

