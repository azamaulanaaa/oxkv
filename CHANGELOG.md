# Changelog

All notable changes to this project are documented in this file.

## [0.12.0] - Unreleased

This release is compared to `v0.11.0`. The workspace version is `0.12.0`.

### Added

**Shared 1.0 contract foundation**

- `oxkv-contracts` holds the canonical error and receipt registry. Each error
  code has one wire shape and one internal diagnostic for a failed invariant
  check. The crate also holds the four receipt states and the receipt
  envelope, the eight cursor token types, the reserved system keyspace,
  qualified version axes, provenance, content digest, and content seal.
- `oxkv-key` adds key encoding v1. It gives typed components, system keys, and
  half-open table bounds.
- `oxkv-manifest` adds manifest v1. It gives the schema, the canonical JSON
  subset, validation, and the deterministic descriptor compiler.
- `oxkv-policy` adds policy AST v1. It gives one total evaluator, step
  budgets, and stable explanation bytes.

**Authority write path**

- `oxkv-write` adds versioned write requests. A request carries one client
  identity, one idempotency key, and one base version for each key. The crate
  also holds the authority.
- `oxkv-write` saves and loads the full authority state. `Authority::save_state`
  and `Authority::load_state` cover rows, receipts, the commit version, audit
  records, and checkpoints. The state file has a versioned format, an
  integrity value, and an atomic replace.
- The `oxkv-authority` binary opens a data directory, commits one write, and
  prints the receipt. The binary uses the file witness outbox and local mode.
  Production mode still needs a real `CheckpointSigner`.
- `oxkv-outbox` adds the durable client outbox and the receipt ledger. The
  ledger keys each verdict by client and idempotency key.
- `oxkv-scheduler` adds the per-key FIFO write scheduler. Each request is
  queued, in flight, or terminal. One key has at most one request in flight.
- `oxkv-enforce` adds deterministic commit enforcement, the pinned policy
  version, the projection check, and the hash-linked per-shard audit record.
- `oxkv-projection` adds policy fact projection. It carries an epoch, a
  watermark for each fact source, a version-distance staleness bound, and
  invalidation.
- `oxkv-scope` adds scope declaration, deterministic identity, membership,
  diffs, shard plans, and exact and coverage result modes.
- `oxkv-security` adds authentication, authorization, request signatures, the
  write audit chain, signed checkpoints, an asynchronous witness queue, and a
  separate read access log.

**Replication and replica reads**

- `oxkv-snapshot` adds the v1 `ShardSnapshot` and `ScopeSnapshot` byte
  contract. It uses CRC32C pages, a BLAKE3 directory, and a content seal.
- `oxkv-hydrate` adds resumable hydration. It verifies each page, writes a
  durable journal, swaps generations, and recovers after a crash.
- `oxkv-replica` adds the read-only client replica. It holds a scope mirror, a
  local search index, and a local tab lease.
- `oxkv-wire` adds the sans-IO sync wire protocol v1. It defines frames,
  cursors, resume, and credit flow control.
- `oxkv-rpc` adds the generated RPC surface. It exposes a neutral
  `descriptor/v1` surface and the Connect envelope.
- `oxkv-map` adds static shard map v1. It gives table bounds, routing,
  validation, and a digest.
- `oxkv-repl` adds the replication seam. It gives one driver interface, a
  lease driver and a Raft-like driver, one lease-term fence, and a read
  barrier.
- `oxkv-readpin` adds authority and replica read pins, session minimums,
  bounded-staleness reads, and read-your-writes.
- `oxkv-tier` adds tiered storage placement, authority leases, and content
  seals.

**Search, events, and operations**

- `oxkv-index` adds the per-shard inverted index and the frozen tokenizer
  contract.
- `oxkv-search` adds snapshot-consistent search. It gives one search pin, a
  deterministic merge order, and stable keyset pagination.
- `oxkv-cost` adds the query cost model, explain records, bounded fan-out, and
  the deterministic scatter-gather merge.
- `oxkv-event` adds the `oxkv/event/v1` business event envelope. It gives a
  deterministic event identity and a per-key consumer cursor.
- `oxkv-consumer` adds the reference effect consumer. It gives per-key
  cursors, idempotent effects, retries, and a dead-letter queue.
- `oxkv-rule` adds the immutable policy catalog and the rule lifecycle. The
  lifecycle covers dry run, staged rollout, canary, promotion, and rollback.
- `oxkv-ops` adds portable operations contracts for migration, backup,
  restore, quota, compaction, telemetry, and control.
- `oxkv-kv` adds the ordered key-value substrate. It gives versioned point
  reads, range scans, cursors, and atomic batches.
- `oxkv-hooks` adds the pure pre-write validation and post-write notification
  pipeline.

**Qualification and release evidence**

- `oxkv-qualify` adds the portable qualification suite. It gives the scenario
  matrix, fault injection, golden vectors, the differential comparator, fuzz
  entry points, and the release tiers.
- `oxkv-falsify` adds the five 1.0 falsifier benchmarks. Each falsifier names
  one workload, one threshold, and one action. A breach or a skip fails the
  release gate.
- `oxkv-freeze` adds the 1.0 freeze register. It lists five core contracts and
  twelve annexes, the freeze states, the reader window, and the support
  policy.
- `oxkv-portable` adds the native and wasm32 parity contract. It gives one
  semantic core, injected platform adapters, and a byte comparator for two
  executors.

**Core store API**

- `TxCas::set_if_absent`, `TxCas::compare_and_set`, and
  `TxCas::delete_if` stage a write and a precondition. A failed precondition
  returns `StoreError::TxConflict` and applies no staged write.
- `StoreExt::snapshot` returns a read-only `Snapshot`. The call copies every
  key and value one time.
- `ChangeFeed` and `FeedEntry` give a durable append-only event log with a
  cursor and chunked pruning. Delivery is at-least-once.
- `StatsStore` and `StoreStats` count client-side calls, bytes, scan rows,
  transaction results, and errors.
- `ScopedStore` and `StoreExt::scoped` map keys to `{prefix}/{key}` and keep
  scans inside the scope.
- `ScanExt::scan_older_than`, `BatchExt::delete_prefix_before`,
  `StoreExt::load_chunked`, and `StoreExt::load_stream_chunked` add bounded
  cleanup and chunked load helpers.

### Changed

- The workspace version is `0.12.0`. The workspace crates share this version.
- Only `oxkv` is published to crates.io in `0.12.0`. The adapter crates and
  the 1.0 contract crates set `publish = false` and remain source-only until
  1.0. `oxkv-wasm` still ships as the `oxkv` npm package.
- Rust scan limits use `Option<usize>`. The WASM boundary keeps `Option<u32>`
  and converts the value. A limit that fits in `u32` behaves as it did in
  `0.11`.
- `StoreError::Storage` holds `StorageError { message, cause }`.
  `StoreError::storage_cause` returns `StorageCause`. Every cause still maps
  to `ErrorKind::Transient`.
- `Store::Transaction` requires `Sync` in addition to `Send`.
- `BatchExt::delete_prefix_chunked` is the current name for the chunked
  delete.
- The legacy Lucene query filter remains supported. `oxkv-query` is a separate
  crate and is not part of the 1.0 search contract. The 1.0 contract is
  `oxkv-index` and `oxkv-search`.
- The release gate runs the qualification matrix and a browser matrix on
  headless Chrome and headless Firefox. A failed Chrome leg or a failed
  Firefox leg blocks the tag.
- The qualification tier policy sets the browser, fuzz, and old-artifact work
  for pull request, nightly, and release candidate runs. The release candidate
  tier runs the full cross product and gates the release.

### Fixed

- The operations contracts complete the backup, restore, and fence checks.
- Hydration adds provenance and bounds the wire session.
- Versioned writes enforce the base version of each row and the projection
  boundary.
- Scope excludes system keys and enforces scope admission.
- The legacy query filter supports regular expressions again. The 1.0 search
  contract still rejects a regular expression with
  `policy.regex_not_supported`.
- Artifact content digests use raw BLAKE3.

### Removed

- `BatchExt::delete_prefix` is removed. Use
  `BatchExt::delete_prefix_chunked`.

### Security

- A production write authority requires a real `CheckpointSigner`. A
  production configuration with `StructuralSigner` fails at startup.
- The authority appends one signed audit record for each terminal verdict. It
  sends signed checkpoints to an asynchronous witness outbox after the commit.
- The security boundary runs authentication before authorization. It records
  one audit record for each write verdict and keeps a separate read access log.
- Policy evaluation rejects a regular expression and charges named step
  budgets. Secret key material stays out of policy and audit output.
- Request signatures bind the body, the idempotency key, the base version, a
  nonce, and an expiry.

### Documentation

- `MIGRATING.md` gives the 0.11 to 0.12 guide and the 0.12 to 1.0 guide.
- `docs/contracts.md` states the transaction, durability, scan, fencing,
  storage, error, and memory contracts.
- `docs/specs/` adds the dependency DAG, falsifier, freeze, portability, and
  qualification specifications.
- `docs/readpin.md`, `docs/scheduler.md`, and
  `docs/feedback-0.11-response.md` document the read pin, the scheduler, and
  the 0.11 feedback status.
- The README covers the new APIs, the legacy filter boundary, and the 1.0
  search contract.

### Breaking changes

- `GetSet::gets_bytes`, `GetSet::gets_keys`, `QueryExt::gets`, and
  `QueryViewExt::gets` take `Option<usize>`. Remove a `u32` cast at a Rust
  call site. The WASM boundary does not change.
- `StoreError::Storage` holds `StorageError`, not `String`. A match that binds
  the old string must read `storage.message`.
- `StoreError::TxConflict` is new. An exhaustive match on `StoreError` must
  add the variant.
- `Store::Transaction` requires `Sync`. A custom store must use a transaction
  type that is `Sync`.
- `ChangeFeed::append_in_tx` requires `T: Transaction + Sync`.
- `BatchExt::delete_prefix` is removed. Use
  `BatchExt::delete_prefix_chunked`.

[0.12.0]: https://github.com/azamaulanaaa/oxkv/compare/v0.11.0...HEAD
