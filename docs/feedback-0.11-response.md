# OxKV 0.11 feedback response

## Context

- Feedback version: `oxkv 0.11.0`.
- Response version: `oxkv 0.12.0`.
- The response covers the production-style Rust service report in
  `/tmp/oxkv-0.11-feedback.md`.
- All workspace crates release in lockstep at `0.12.0`.

## Status by item

| Feedback item | Status | Public API |
| --- | --- | --- |
| Packaging | Fixed. All workspace crates share `0.12.0`. The release workflow publishes them in dependency order, and `MIGRATING.md` has a crate compatibility table. Publication is still a release step. | Workspace crates `oxkv`, `oxkv-cached`, `oxkv-otel`, `oxkv-opfs`, `oxkv-query`, `oxkv-s3`, and `oxkv-wasm` |
| Transaction-level CAS | Fixed. Each method stages a write and a precondition. `commit` checks the committed state under one lock or write gate. A conflict applies no staged write. | `TxCas::{set_if_absent, compare_and_set, delete_if}`, `StoreError::TxConflict { key, current }` |
| Scan limits | Fixed. Rust APIs use `Option<usize>`. The WASM boundary keeps `Option<u32>`. | `GetSet::gets_bytes`, `GetSet::gets_keys`, `oxkv_query::QueryExt::gets`, `oxkv_query::QueryViewExt::gets` |
| `gets_keys` override docs | Documented. The contracts name the backends that override the method and the backends that use the default body and read values. | `docs/contracts.md` section 3.4, `GetSet::gets_keys` |
| Reader consistency | Documented. One section states that a reader revalidates the manifest on each read, replays new WAL files, and does not freeze a view. | `OxKvReader`, `docs/contracts.md` section 4.6 |
| `Storage` cause | Fixed. A storage error now holds a message and a structured cause. Retry classification is unchanged. | `StorageError { message, cause }`, `StorageCause`, `StoreError::storage`, `StoreError::storage_with_cause`, `StoreError::storage_cause` |
| `retry_after` | Documented. The method returns `None` for every error on this branch. `None` does not mean "retry now". | `StoreError::retry_after` |
| `put_bytes` through a generic transaction | Fixed. The transaction associated type requires `Sync`. A generic adapter can call `put_bytes`. | `Store::Transaction: Transaction + Send + Sync`, `Transaction::put_bytes` |
| Stable snapshot | Fixed. The call copies every key and value one time into a read-only snapshot. The copy uses memory equal to the store size. | `StoreExt::snapshot`, `Snapshot`, `SnapshotTx` |
| TTL and retention | Intentionally limited. There is no expiry metadata and no background job. The new helpers remove old keys when the caller runs them. | `ScanExt::scan_older_than`, `BatchExt::delete_prefix_before` |
| Durable change feed | Fixed. The feed writes `{prefix}/{seq:020}` and keeps the counter at `{prefix}/next`. Delivery is at-least-once. It is not automatic change capture. | `ChangeFeed`, `FeedEntry`, `ChangeFeed::{next_seq, append, append_in_tx, since, prune}` |
| Metrics and scoped handles | Fixed. The counters are client-side. The scoped handle maps keys and keeps scans inside the scope. | `StatsStore`, `StoreStats`, `StatsStore::{stats, reset, into_inner}`, `ScopedStore`, `ScopedTx`, `StoreExt::scoped` |
| Batch helpers | Fixed. The chunked delete has a name that shows the non-atomic behavior. `get_many` cost is documented. | `BatchExt::delete_prefix_chunked`, `BatchExt::delete_prefix_before`, `BatchExt::get_many` |
| Load size | Fixed. A chunked load commits one transaction per page. The one-transaction load keeps its documented memory and object-size cost. | `load_stream_chunked`, `StoreExt::load_chunked` |

## Checks

All commands below ran in `/tmp/worktree/docs-report` and passed.

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | Passed. No output. |
| `cargo test --workspace --all-features --locked` | Passed. 364 unit and integration tests and 23 doctests. 0 failed. |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed. No warnings. |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked` | Passed. Generated the workspace documentation. |
| `.githooks/ci-local.sh release-config` | Passed. `PASS: release workflow gates publishing on CI checks.` The command needs `safe.directory` for this worktree. |
| `git diff --check` | Passed. No whitespace errors. |

Two targeted test runs also passed:

- `cargo test --all-features --lib is_not_found`: 2 passed, 0 failed.
- `cargo test --all-features --lib change_feed`: 8 passed, 0 failed.

After the version bump and the report merge, the same full workspace commands
also passed on the integrated `release/0.12` branch. The test run reported 242
core, 15 cached, 10 otel, 94 query, and 3 s3 unit and integration tests, plus
23 doctests. 0 failed. `cargo fmt --all --check`, clippy, rustdoc, and
`git diff --check` passed.

### Packaging dry runs

`cargo publish --dry-run` passed for all seven crates on `release/0.12`. The
run used the workspace path patch that the release workflow uses.

| Crate | Result |
| --- | --- |
| `oxkv` | Passed |
| `oxkv-query` | Passed |
| `oxkv-opfs` | Passed |
| `oxkv-s3` | Passed |
| `oxkv-otel` | Passed |
| `oxkv-cached` | Passed |
| `oxkv-wasm` | Passed |

## Breaking changes

- `GetSet::gets_bytes`, `GetSet::gets_keys`, `QueryExt::gets`, and
  `QueryViewExt::gets` take `Option<usize>`. A call site with an explicit
  `u32` value must change the type or remove the cast. `None` and an
  untyped `Some(10)` still work.
- `StoreError::Storage(String)` becomes `StoreError::Storage(StorageError)`.
  A match that binds the old string must read `storage.message`.
- `StoreError::TxConflict` is new. An exhaustive `match` on `StoreError`
  must add it.
- `Store::Transaction` now requires `Sync` in addition to `Send`. A custom
  store must use a transaction type that is `Sync`.
- `ChangeFeed::append_in_tx` now requires `T: Transaction + Sync`. It uses
  `put_bytes` because the event key is new.
- `BatchExt::delete_prefix` is removed. Use `BatchExt::delete_prefix_chunked`.
- The WASM and JavaScript limit type does not change. It stays `Option<u32>`.

## Remaining limits and open decisions

- TTL remains absent. The retention helpers are cleanup calls that the
  application schedules. They do not record expiry and do not run in the
  background.
- A streaming export is a logical dump, not a point-in-time backup. Use
  `StoreExt::snapshot` for a fixed view. The snapshot uses memory equal to the
  store size.
- `ChangeFeed` gives at-least-once delivery. A crash between counter update
  and event write leaves a gap. A consumer must be idempotent and tolerate a
  gap.
- `StatsStore` counts client-side API calls. It does not count S3 requests,
  WAL objects, or compaction backlog. Cache hit and miss data stays in
  `Cache::stats`.
- `BatchExt::get_many` reads keys one at a time. The store has no server-side
  multi-get.
- `load_chunked` is not atomic across pages. A failure after a commit leaves
  the earlier pages committed. The one-transaction `load` and `load_stream`
  are atomic but not bounded in memory or object size.
- `retry_after` still returns `None` for every error. No backend supplies a
  server delay hint.
- Publication of the crates remains a release action. The branch makes the
  version and workflow changes that the repository controls.

The implementation follows the design note. No public name differs from the
design note.

## Lessons learned

- Packaging is part of the public API. A README command that fails is a
  product defect. A user who cannot run `cargo add oxkv-s3` cannot use the
  feature, even when the source is correct.
- Contract text belongs with the API. Users had to read source to learn which
  backend overrides `gets_keys` and what reader consistency means. The
  contracts now state both next to the trait and the reader.
- `Send` was not enough for generic transaction code. An async default method
  needs `Sync`, and a generic adapter test catches it. The compiler error was
  E0277 on `<S as Store>::Transaction`.
- A streaming export is not a backup. Name and document the difference. A
  materialized snapshot is a different cost and must say so. The export pages
  live reads. The snapshot copies every pair.
- Names should show behavior. `delete_prefix_chunked` makes non-atomicity
  visible.
- A string error class forces callers to parse messages. A structured cause
  is required for operational decisions. `StorageCause` lets a caller tell a
  missing object from a corrupt object without text matching.
- Single-key CAS and transaction CAS are different contracts. State when a
  precondition is checked and what a conflict leaves behind. `Cas` reports a
  conflict at the call. `TxCas` reports it at commit and leaves no staged
  write.
- Use `usize` at the Rust API boundary and keep `u32` only at the JavaScript
  and wire-format boundaries. The casts in user code were a symptom of the
  wrong type on the public surface.
- A one-transaction load is simple but not bounded. A chunked load trades
  atomicity for memory. The docs must state both sides of that trade.
- Cross-cutting API migrations conflict with parallel feature branches. Run
  them late or reserve a merge window. The limit-type change touched every
  adapter and test that forwarded a page size.
