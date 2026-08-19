

# rust-kv

A transactional key-value store library written in Rust, with optional WebAssembly bindings for JavaScript interop. Features cursor-based pagination, JSON serialization via `serde_json`, and strict linting. All operations are async using `futures::lock::Mutex` to enable concurrent access from WASM call sites.

## Features

- **Transaction support** — atomic commit/rollback batches of CRUD operations
- **Cursor-based pagination** — bidirectional traversal (`Next` / `Prev`) with inclusive range cursors and limit control
- **JSON serialization** — extension methods for inserting and retrieving `serde_json::Value` types via JSON, stored as raw bytes
- **WASM bindings** — thread-safe wrappers in `src/wasm.rs` expose every store method to JavaScript as async promises
- **Extensible backends** — the crate defines three traits (`GetSet`, `Transaction`, `Store`) that any backend can implement (e.g., Redb)
- **Save/Load** — serialize the entire store contents into a single contiguous `Uint8Array` and reconstruct it from binary data
- **Strict linting** — all warnings and clippy lints are enforced at the crate level

## Core Traits

| Trait | Purpose |
|-------|---------|
| [`store::GetSet`] | Basic key-value operations: `get_bytes`, `set_bytes`, `delete`, `exists` (paginated via `gets_bytes`) |
| [`store::Transaction`] | Extends `GetSet` with `commit` and `rollback` for atomic batches |
| [`store::Store`] | Extends `GetSet` with `begin_tx` — starts a write transaction |
| [`store::GetSetExt`] | Convenience methods: `set`, `get` (JSON-serialized) |
| [`store::StoreExt`] | Save/Load the entire store contents as binary |

## Quick Start

```rust,ignore
use rust_kv::{BTreeStore, store::*};

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

## WASM Bindings

The WASM module in `src/wasm.rs` provides thread-safe wrappers for `BTreeStore`, exposing every store method to JavaScript as async promises.

Build for WebAssembly:

```bash
wasm-pack build --target web  # or nodejs, bundler, etc.
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
- `src/store/mod.rs` — core traits (`GetSet`, `Transaction`, `Store`) and error types
- `src/store/btree.rs` — in-memory B-tree backend with transaction overlay support

## License

MIT OR Apache-2.0

