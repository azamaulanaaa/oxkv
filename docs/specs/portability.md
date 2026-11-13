# Native and wasm32 portability contract

| Field | Value |
| --- | --- |
| Status | Proposed |
| Date | 2026-11-12 |
| Scope | The `oxkv-portable` crate, the adapter boundary, and the parity evidence |
| Normative source | `specs.md` section 7.21 and section 13 |

## 1. One semantic core

The `oxkv-portable` crate holds one semantic core. The core compiles for native and for `wasm32-unknown-unknown`. It has no target conditional on a semantic path. An adapter selects the platform.

The following modules are semantic. The static check scans every one.

| Module | Responsibility |
| --- | --- |
| `src/encode.rs` | Canonical fixed-width integer and length encoding. |
| `src/artifact.rs` | Canonical key, page, snapshot, and business event bytes. |
| `src/verdict.rs` | The pure write verdict and the canonical receipt envelope. |
| `src/core.rs` | The operation state machine and observable results. |
| `src/parity.rs` | The transcript, the two executors, and the byte comparator. |

A semantic module does not read a clock, draw randomness, read the environment, read a locale, use floating point, or use `unsafe`. The core reads time and entropy through interfaces only.

## 2. Adapter boundary

`src/adapter.rs` is the only platform boundary. An adapter supplies one capability. It does not decide a verdict and does not shape a byte.

| Trait | Capability |
| --- | --- |
| `Storage` | Ordered key-value reads, range scans, cursor pagination, one atomic batch commit, and a declared durability. |
| `Clock` | The current time in milliseconds. |
| `Entropy` | A byte fill for a non-semantic nonce. |
| `Scheduler` | The number of independent scheduling lanes. |
| `Transport` | Opaque frame movement. |

The crate ships portable adapters. `MemoryStorage` keeps the data. `ScriptedClock` and `DeterministicEntropy` give reproducible test input. `InProcessTransport` returns the input frame unchanged. `FixedScheduler` reports a fixed lane count.

A production native adapter and a production browser adapter implement the same traits. The adapter maps the interface onto a file, an object store, an OPFS handle, a real clock, a system random source, and a network socket. It does not encode a key. It does not encode a page. It does not choose a receipt state.

## 3. Canonical bytes

Every integer in a frozen artifact has a fixed width. Every integer uses big-endian byte order. Every byte string and text value has a `u64` length prefix. The format has no floating-point field.

A key uses `0x01 || table-name || 0x00 || component...`. A snapshot has a canonical text preamble and a binary page body. A page carries a deterministic `page_id`, a `page_sequence`, a commit version, prefix bounds, records, and a content digest. A business event carries the pinned policy version and the origin request identity. An event identity covers the shard, key, commit version, rule identity, and emission index.

## 4. Parity harness

One input transcript runs through two executors.

- `SequentialExecutor` applies every operation in input order.
- `InterleavedExecutor` interleaves independent table work. It keeps per-table order. It uses no OS thread.

The harness compares the observations and the encoded observation bytes. The lane count changes latency only. It does not change a result. The same harness builds for wasm32 with the no-thread executor.

`tests/parity.rs` proves these claims:

- Two executors produce identical bytes for one key, page, snapshot, cursor, event, receipt, and transport sequence.
- A snapshot decodes and re-encodes to the same bytes.
- A scripted clock and deterministic entropy reproduce a run.
- A lane count does not change a run.
- A hostile adapter cannot change a verdict.

`tests/vectors/sequence.json` is the golden vector for the ordered sequence.

## 5. Static check

`tests/static_check.rs` reads the five semantic modules. The check fails on a direct clock, entropy, environment, or locale read. The check fails on a platform conditional, a floating-point field, an I/O call, a thread call, or an `unsafe` block.

The check is deterministic. It uses `std::fs` only inside the test target. A negative test supplies each forbidden pattern and asserts one failure.

## 6. Verification

Run these commands from the worktree.

```text
cargo test -p oxkv-portable
cargo clippy -p oxkv-portable --all-targets -- -D warnings
cargo check -p oxkv-portable --target wasm32-unknown-unknown
cargo fmt --check -p oxkv-portable
```

A missing target run does not pass the gate.
