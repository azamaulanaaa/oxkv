# OxKV contracts

This document states the behavior that the code on this branch provides. It
covers the in-memory B-tree backend and the durable LSM backend. Each claim
names the source file and function that implements it.

The crate version is 0.12.0. The contracts below describe this branch only.
They do not describe requested features that are absent from this branch. The
legacy Lucene-style query filter lives in the standalone `oxkv-query` crate at
`crates/oxkv-query`. The 1.0 release still supports it. It is not part of the
1.0 search contract.

## Search surface

The 1.0 search contract is `oxkv-index` for the tokenizer and inverted index,
and `oxkv-search` for the pinned search read. The `oxkv-query` crate is the
legacy filter. It is not part of the 1.0 search contract.

The `oxkv-wasm` bindings depend on `oxkv-query`. The WASM `gets` methods take
an optional query argument. The query argument uses the legacy filter. Without
a query, the methods return a plain paginated JSON scan. With a query, the
methods return only the rows whose JSON document matches the query. The
`getsBytes` methods return raw bytes and take no query.

The legacy filter supports field paths, ranges, wildcards, fuzzy terms,
boolean operators, and regular expressions. A `/pattern/` term matches with
the `regex` crate. See `crates/oxkv-query/src/json.rs`, `compile_term` and
`match_string_kind`.

The 1.0 search contract has no regular expressions. A policy with a regex
fails with `policy.regex_not_supported`. See `crates/oxkv-policy/src/ast.rs`,
`Policy::new`. The `*` and `?` wildcard forms still work in the legacy
filter. A later release can add regular expressions to the 1.0 contract with a
new contract version.

## Terms

| Term | Meaning |
| --- | --- |
| `BTreeStore` | In-memory backend. It uses a `BTreeMap` behind `RwLock`. |
| `OxKvStore` | Durable LSM backend over a `Storage` object. It owns one writer epoch. |
| `OxKvTx` | Transaction handle for `OxKvStore`. |
| `OxKvReader` | Read-only follower over the same prefix. It never takes ownership. |
| `MemTable` | In-memory map of writes that are not yet in an SST. |
| WAL | Write-ahead log object at `e{epoch}/wal/{seq}.log`. |
| SST | Sorted string table at `e{epoch}/sst/L{level}/{seq}.sst`. |
| Manifest | `manifest.json`. It lists the live WAL and SST objects. |

## 1. Transactions

### 1.1 B-tree transactions

`BTreeStore::begin_tx` creates a `BTreeTx`. The transaction holds an overlay
`BTreeMap<String, Option<Vec<u8>>>`. A value of `None` is a tombstone. See
`src/store/btree.rs`, `Store for BTreeStore::begin_tx` and `struct BTreeTx`.

| Property | Contract |
| --- | --- |
| Staging | `set_bytes` and `delete` write only the overlay. The shared map does not change. |
| Read your own writes | `get_bytes` and `has` check the overlay first. An overlay hit, including a tombstone, hides the shared map value. |
| Reads of other keys | The transaction reads the current shared map under a read lock. There is no transaction snapshot. |
| Other readers | A reader outside the transaction cannot see overlay writes before commit. |
| Commit | `BTreeTx::commit` applies the overlay under one write lock. It inserts `Some` values and removes `None` values. |
| Conditional writes | `TxCas` stages a write and a precondition. `commit` checks every precondition against the shared map under the same write lock that applies the overlay. A failure returns `StoreError::TxConflict` and applies no overlay entry. |
| Rollback | `BTreeTx::rollback` does nothing. The overlay is dropped with the handle. |
| Conflict detection | None by default. `commit` does not compare versions or re-check keys. `TxCas` adds opt-in preconditions for the keys that the caller names. |
| Same-key race | Two transactions can stage the same key. The transaction that commits last overwrites the earlier value. No error is returned. |

`commit` re-validates staged `Set` events before it takes the write lock. The
validator sees the transaction's final state through `RevalidateView`. The
view reports the pre-transaction value for the key under validation. See
`src/store/btree.rs`, `Transaction for BTreeTx::commit`, and
`src/store/hooks.rs`, `RevalidateView` and `validate_key`.

The validator list is captured when the transaction begins. A validator added
after `begin_tx` does not run for that transaction. See
`src/store/btree.rs`, `Store for BTreeStore::begin_tx`.

The re-validation pass runs before the write lock. Another transaction can
change a related key between validation and commit. A validator is not a
conflict detector.

### 1.2 LSM transactions

`OxKvStore::begin_tx` creates an `OxKvTx`. It shares the `MemTable`, WAL
sequence counter, WAL buffer, manifest cache, write gate, pending-write queue,
and SST cache with the parent store. See `src/store/lsm/store.rs`,
`Store for OxKvStore::begin_tx`.

| Property | Contract |
| --- | --- |
| Staging | `set_bytes`, `put_bytes`, and `delete` write only the transaction overlay. |
| Read your own writes | `get_bytes` checks the overlay first. Then it reads the shared `MemTable` and the SSTs. |
| Reads of other keys | The transaction reads the current shared state. There is no transaction snapshot. |
| Other readers | They cannot see overlay writes before commit. |
| Commit, empty | An empty overlay with no preconditions returns `Ok(())` before the write gate. |
| Commit, non-empty | The transaction writes one WAL object that holds all staged records. One manifest CAS lists that object. Then the `MemTable` changes. |
| Conditional writes | `TxCas` stages a write and a precondition. `commit` holds the write gate and checks every precondition against committed state. A failure returns `StoreError::TxConflict` and applies no overlay entry. |
| Rollback | `OxKvTx::rollback` does nothing. The overlay is dropped with the handle. |
| Conflict detection | None by default. `commit` does not compare versions or re-check keys. `TxCas` adds opt-in preconditions for the keys that the caller names. |
| Same-key race | Commits serialize on the write gate. The later commit gets the higher WAL sequence. Replay applies WAL files in manifest order. The later value wins. No error is returned. |

`OxKvTx::commit` runs these steps in order. See `src/store/lsm/tx.rs`,
`Transaction for OxKvTx::commit`.

1. Re-validate every staged `Set` event.
2. Take the `write_gate`.
3. Take ownership of the transaction overlay.
4. Drain writes staged on the parent store with `stage_set` or `stage_delete`.
5. Check every `TxCas` precondition against committed state. A failure
   returns `StoreError::TxConflict` before any write.
6. Encode all overlay updates into one WAL payload.
7. Call `append_wal_locked`. This writes one WAL object with
   `PutMode::Create`, checks ownership, and appends the WAL id to the manifest
   with a CAS.
8. Apply the updates to the `MemTable` only after the WAL and manifest steps
   succeed.

The commit is atomic for visibility. A reader sees either no staged key or the
whole transaction, because the manifest lists one WAL object. If a step fails,
the `MemTable` is not changed. The WAL object can still exist without a
manifest entry. A later `gc_orphans` can remove it. See
`src/store/lsm/store.rs`, `append_wal_locked`, `drain_staged_locked`, and
`gc_orphans_inner`.

The validator list is captured when the transaction begins. See
`src/store/lsm/store.rs`, `Store for OxKvStore::begin_tx`. Each staged `Set` is
re-validated at commit against the transaction's final state through
`RevalidateView`. See `src/store/lsm/tx.rs`, `Transaction for OxKvTx::commit`,
and `src/store/hooks.rs`, `RevalidateView`.

The LSM re-validation pass runs before the write gate. Another writer can
change a related key between validation and commit. A validator is not a
conflict detector.

### 1.3 Isolation level

There is no serializable isolation and no snapshot isolation on either
backend. There is no default optimistic conflict detection and no default
transaction abort on a write-write race. Both backends provide
read-your-own-writes and atomic visibility at commit. Reads outside the overlay
read current committed state. Two calls in one transaction can therefore
observe different committed states. The same-key result is last-commit-wins.

`TxCas` adds opt-in optimistic conflict detection. A caller
names a key and an expected committed value when it stages the write. `commit`
checks every named key under the write lock or write gate. If one check fails,
the commit returns `StoreError::TxConflict` and applies no staged write. A
transaction without `TxCas` keeps last-commit-wins behavior.

There is no transaction snapshot isolation. A read outside the overlay sees
the current committed state. A materialized snapshot is available through
`StoreExt::snapshot`. That method copies every key and value one time and
returns a read-only `Snapshot`. A later write or delete does not change the
snapshot. The snapshot uses memory equal to the size of all keys and values in
the store. It is not transaction isolation.

### 1.4 Transaction size

Neither overlay has a size cap in the code. `BTreeTx` and `OxKvTx` both use an
unbounded `BTreeMap`. The LSM commit payload is one WAL object. Its size grows
with the overlay. WAL records use 32-bit key and value lengths. A key or value
of 4 GiB or more fails to encode. `encode_record` returns
`StoreError::Serialization`. The LSM write paths wrap that error as
`StoreError::Storage`. See `src/store/mod.rs`, `encode_record`, and
`src/store/lsm/store.rs`, `encode_ops` and `GetSet for OxKvStore::put_bytes`.

The 256-entry group-commit cap does not apply to a transaction commit. A
transaction commit holds the write gate alone and writes its own WAL object.
See `src/store/lsm/mod.rs`, `MAX_GROUP_WRITES`, and
`src/store/lsm/store.rs`, `Transaction for OxKvTx::commit`.

The load helpers use the same overlay. `load` and `load_stream` stage the whole
payload in one transaction. Decoding memory stays bounded by the largest
pending record, but the overlay grows with the total payload. The commit writes
one WAL object. A large backup therefore costs memory and one large object.
`load_chunked` and `load_stream_chunked` commit one transaction every
`page_size` records. A failure after a commit leaves the earlier pages
committed. A page size of zero returns `StoreError::Other`. See
`src/store/mod.rs`, `load_stream` and `load_stream_chunked`.

## 2. Durability

### 2.1 B-tree durability

`BTreeStore` is in memory only. `set_bytes`, `put_bytes`, `delete`, and
`commit` change the shared `BTreeMap` and nothing else. A process exit loses
all data. There is no WAL, no fsync, and no storage error path. See
`src/store/btree.rs`, `GetSet for BTreeStore` and `Transaction for BTreeTx`.

### 2.2 LSM durability

The LSM backend reports success only after it makes the write durable. A
successful call has this order.

1. The WAL object is written with `PutMode::Create`.
2. `ownership.json` still names this epoch and session.
3. The manifest CAS lists the WAL id.

The `MemTable` changes after step 3. See `src/store/lsm/store.rs`,
`append_wal_locked`.

| Operation | RPO on `Ok` | Source |
| --- | --- | --- |
| `put_bytes` | 0 | `GetSet for OxKvStore::put_bytes`, `persist_batch` |
| `set_bytes` | 0 | `GetSet for OxKvStore::set_bytes`. It reads the previous value, then calls `put_bytes`. |
| `delete` | 0 | `GetSet for OxKvStore::delete` |
| Transaction `commit` | 0 | `Transaction for OxKvTx::commit` |
| `stage_set` | Not durable yet | `OxKvStore::stage_set` |
| `stage_delete` | Not durable yet | `OxKvStore::stage_delete` |
| `get_bytes`, `has`, `gets_bytes` | Not applicable | Read paths |

RPO=0 here means that an `Ok` result follows a successful WAL PUT and a
successful manifest CAS against the configured `Storage`. It does not add a
consistency wait that the storage backend does not provide. See section 5.

A failed call can have an unknown outcome. The WAL PUT can succeed and the
manifest CAS can then fail. The write is not visible because the manifest does
not list it. The caller must re-read before a retry. See section 6.

### 2.3 Group commit

`put_bytes` stages one encoded record in the shared pending queue. It then
takes the fair `write_gate`. The gate holder drains up to 256 entries
(`MAX_GROUP_WRITES`) into one WAL object and one manifest CAS. Every entry in
the batch gets the same result. The batch is all-or-nothing. See
`src/store/lsm/store.rs`, `GetSet for OxKvStore::put_bytes` and
`persist_batch`, and `src/store/lsm/mod.rs`, `MAX_GROUP_WRITES`.

`delete` and transaction commits do not join the batch. They take the gate
alone and use their own WAL object.

### 2.4 Staged writes

`stage_set` and `stage_delete` do not access storage. They update the
`MemTable` and push an operation onto the shared WAL buffer. They return no
error and take no gate. The data is durable only after a later durable path
drains the buffer. See `src/store/lsm/store.rs`, `OxKvStore::stage_set` and
`OxKvStore::stage_delete`.

These paths drain the WAL buffer before they allocate their own WAL sequence.
See `src/store/lsm/store.rs`, `drain_staged_locked`.

- `flush`
- `put_bytes` through `persist_batch`
- `delete`
- transaction `commit`

`flush` takes the write gate and drains the buffer. See
`src/store/lsm/store.rs`, `OxKvStore::flush`.

A forced MemTable-to-SST flush can make the current `MemTable` state durable
without draining the WAL buffer. The buffer entry stays queued for a later
durable path. See `src/store/lsm/store.rs`, `flush_mem_to_sst_inner`.

### 2.5 Flush and write order

All durable write paths take the same fair `write_gate`. Each path drains
staged operations before it allocates its own WAL sequence. Therefore a
staged operation that happened before a write gets the lower WAL sequence and
wins on replay. See `src/store/lsm/store.rs`, `drain_staged_locked` and
`append_wal_locked`.

The gate makes WAL sequence allocation order equal program order. It also
serializes manifest CAS retries instead of letting writers storm the manifest.

## 3. Scans

### 3.1 Return type and cursors

`gets_bytes` returns a materialized `Vec<KeyValue>`. It does not return a
cursor or a live iterator. See `src/store/mod.rs`, `GetSet::gets_bytes`.

```text
gets_bytes(limit: Option<usize>, direction: Direction, cursor: (Option<String>, Option<String>))
    -> Vec<KeyValue>
```

| Direction | Cursor `(start, end)` | Result |
| --- | --- | --- |
| `Next` | `(Some(a), Some(b))` | `a..=b`, ascending. Empty if `a > b`. |
| `Next` | `(Some(a), None)` | From `a`, inclusive, to the end. |
| `Next` | `(None, Some(b))` | From the beginning to `b`, inclusive. |
| `Next` | `(None, None)` | All keys, ascending. |
| `Prev` | `(Some(a), Some(b))` | From `a` down to `b`, inclusive. Empty if `a < b`. |
| `Prev` | `(Some(a), None)` | From `a`, inclusive, down to the beginning. |
| `Prev` | `(None, Some(b))` | Empty. There is no start point. |
| `Prev` | `(None, None)` | Empty. There is no start point. |

Both bounds are inclusive. See `src/store/btree.rs`, `build_next` and
`build_prev`, and `src/store/mod.rs`, `GetSet::gets_bytes` documentation.

The caller must keep its own cursor. A page does not carry the next cursor.
The usual pattern is to pass the last returned key as the next `start` and
skip the first row of the next page, because the bound is inclusive. The
streaming save path uses this pattern. See `src/store/mod.rs`, `SaveStream`.

```text
let mut start = Some(prefix_start)
loop {
    let page = store.gets_bytes(Some(256), Next, (start.clone(), Some(prefix_end)))
    let first = if start_seen { 1 } else { 0 }
    for row in page[first..] { process(row) }
    if page.len() < 256 { break }
    start = Some(page.last().key.clone())
}
```

A caller can also build a strict successor key. For a `String` key, append
`"\0"` to the last key. That string is greater than the last key and less
than every longer key with the same prefix.

### 3.2 B-tree scan behavior

`BTreeStore::gets_bytes` takes the map read lock and builds the full page
inside that lock. It clones each key and value. A writer needs the write lock,
so a writer waits for the scan to finish. With `limit = None`, the read lock
stays held until every matching row is cloned. See
`src/store/btree.rs`, `GetSet for BTreeStore::gets_bytes`, `build_next`, and
`build_prev`.

A `BTreeTx` scan holds the store read lock and the overlay mutex while it
merges the two sides. It also clones every returned row. See
`src/store/btree.rs`, `GetSet for BTreeTx::gets_bytes`.

### 3.3 LSM scan behavior

The LSM read path does not hold a table guard across SST I/O. It clones the
matching `MemTable` rows under a brief read lock. It then releases the guard
and reads SSTs. See `src/store/lsm/store.rs`, `OxKvStore::gets_bytes`, and
`src/store/lsm/read.rs`, `range_lookup`.

A forward (`Next`) scan uses a lazy k-way pull merge. The merge keeps one
entry per source in a heap. It stops when it reaches `limit` live keys. It
does not decode the unread tail of a file. See
`src/store/lsm/merge.rs`, `pull_merge_next`, and `src/store/lsm/read.rs`,
`scan_manifest`.

A backward (`Prev`) scan materializes the overlapping SST ranges, resolves
blob values, and then merges. See `src/store/lsm/read.rs`, `scan_manifest`.

Writers do not wait for scan I/O on the LSM backend. A scan can run while a
writer commits. The scan is not a stable snapshot. It uses the `MemTable`
snapshot from the start of the call, the manifest from the start of the call,
and any SST data that still exists. A compaction that removes a listed SST
causes one retry against a fresh manifest. See
`src/store/lsm/read.rs`, `retry_once_not_found`.

### 3.4 Scan memory

`GetSet::gets_bytes` and `ScanExt::scan` with `values = true` materialize every
returned key and value in one `Vec`. See `src/store/mod.rs`,
`struct KeyValue` and `GetSet::gets_bytes`.

`GetSet::gets_keys` and `ScanExt::scan_keys` return keys only. `ScanExt::scan`
with `values = false` returns `KeyValue` rows with an empty `value` field.
`ScanExt::count_prefix` counts keys up to a maximum. These methods let a caller
avoid the output value copies. See `src/store/mod.rs`, `GetSet::gets_keys`, and
`src/store/scan.rs`.

The public backends that override `gets_keys` are `BTreeStore`, `BTreeTx`,
`OxKvStore`, and `OxKvTx`. `Hooked`, `CachedOxKvStore`, `CachedTx`,
`OtelStore`, `OtelTx`, `OxKvReader`, and `OxKvRoTx` use the default body. The
default body calls `gets_bytes`, so it reads values. A custom backend that does
not override also reads values.

`ScanExt` reads one bounded page at a time. Each page carries an
`Option<String>` cursor in `Page::next`. `KeyPrefix::upper_bound` returns an
exclusive upper bound that handles UTF-8 scalars and an empty prefix. See
`src/store/scan.rs`, `KeyPrefix`, `Page`, and `ScanExt`.

The LSM keys-only path skips blob dereference. It skips output value copies. It
still decodes the value bytes of each SST row that it reads. It still copies
`MemTable` values into the merge layers. A large value in the `MemTable`
therefore still occupies memory during a keys-only scan. See
`src/store/lsm/read.rs`, `range_keys`, and `src/store/lsm/merge.rs`.

`limit = None` means all matching keys. It can exhaust memory. It can also
hold the B-tree read lock for a long time. Use a finite limit and a cursor for
paging. The limit type is `Option<usize>`. A limit that fits in `u32` behaves
as it did before. The WASM binding keeps `Option<u32>` at the JavaScript
boundary and converts it to `usize`.

The LSM value path resolves blob pointers while it builds the result. A large
value therefore occupies memory in the result `Vec`. See
`src/store/lsm/read.rs`, `resolve_value`.

## 4. Single writer and fencing

### 4.1 Ownership record

The writer owns `{prefix}/ownership.json`. It holds these fields. See
`src/store/lsm/ownership.rs`, `struct OwnershipRecord`.

| Field | Contract |
| --- | --- |
| `epoch` | Monotonic number. A successful acquire increments it. The first acquire uses 1. |
| `owner_session` | Session id of the owner. |
| `lease_expiry_ms` | Optional field. The current code always writes `None`. |
| `manifest_etag` | Optional debug field. The current code always writes `None`. |

`acquire_ownership` reads the current record and its version. It uses
`PutMode::Create` when the file is absent. It uses `PutMode::Update` with the
observed version when the file exists. A CAS conflict becomes
`StoreError::Fenced`. See `src/store/lsm/ownership.rs`,
`acquire_ownership`.

`OxKvStoreBuilder::build` and `build_with_cache` call this CAS after the
probe. See `src/store/lsm/store.rs`, `build_with_cache`.

### 4.2 What a stale session sees

A superseded writer fails its durable paths with `StoreError::Fenced`. The
check reads `ownership.json` after the WAL PUT in `append_wal_locked`. It
compares both epoch and session. The SST flush and compaction paths perform
the same check. See `src/store/lsm/store.rs`, `append_wal_locked`,
`flush_mem_to_sst_inner`, and `compact_inner`.

`Fenced` is terminal. The current process must stop writing. It must restart
through a new `ownership.json` CAS if it should write again. See
`src/store/mod.rs`, `StoreError::Fenced` documentation.

A stale writer can still complete some reads. Reads do not check ownership.
Only the write paths fence.

The writer epoch is internal. `OxKvStore::epoch()` is compiled only under
`cfg(test)`. `OxKvReader::epoch()` is public and reports the epoch observed at
open or refresh time. See `src/store/lsm/store.rs`, `impl OxKvStore`, and
`src/store/lsm/reader.rs`, `OxKvReader::epoch`.

### 4.3 `assume_single_writer`

`OxKvStoreBuilder::assume_single_writer(true)` sets the manifest cache to
skip revalidation polls on a TTL-fresh entry. It does not skip the probe. It
does not skip the ownership CAS. It does not remove ownership checks or
manifest CAS conflicts. A takeover is still detected. See
`src/store/lsm/store.rs`, `assume_single_writer`, and
`src/store/lsm/manifest.rs`, `ManifestCache::set_skip_revalidation`.

Use it only when one writer owns the prefix. The default is `false`.

### 4.4 `check_stale`

`check_stale` is a method on `CachedOxKvStore`, not on `OxKvStore`. It polls
the manifest with a zero TTL. It compares the manifest version and the SST id
set with the mirror state. It does not read `ownership.json`. A poisoned
mirror reports stale. See `crates/oxkv-cached/src/lib.rs`,
`CachedOxKvStore::check_stale`.

`CachedOxKvStore::refresh` performs the catch-up. A new epoch or an SST set
change rebuilds the mirror from a scan. A WAL append replays incrementally.
A write that returns `Fenced` poisons the mirror. Reads then fail with the same
`Fenced` error until `refresh` succeeds. See `crates/oxkv-cached/src/lib.rs`,
`CachedOxKvStore::refresh`, `is_poisoned`, and `note_fenced`.

### 4.5 Readers

`OxKvReader::open` does not run the probe and does not take the epoch CAS. It
reads `ownership.json` only to record the epoch for debugging and refresh. It
does not write ownership. Opening a reader cannot fence the writer. See
`src/store/lsm/reader.rs`, `OxKvReader::open` and `open_with_cache`.

The reader implements `Store`, but every write returns
`StoreError::Other("read-only store: writes are rejected")`. An empty
transaction commit is a no-op. See `src/store/lsm/reader.rs`,
`GetSet for OxKvReader` and `Transaction for OxKvRoTx`.

### 4.6 Reader consistency

`OxKvReader` does not freeze a view. Each read revalidates the manifest and
replays newly listed WAL files. A reader therefore sees a committed write after
the manifest CAS lands. Two reads on one reader can observe different states. A
scan that races compaction reloads the manifest and retries once. A fresh
reader sees post-open writes on the next read. See
`src/store/lsm/reader.rs`, `ensure_replayed` and `retry_once_not_found`, and
section 5.4.

For a fixed view, call `StoreExt::snapshot` and then read from the snapshot. A
caller that must page the live store can use windowed scans and keep its own
cursor. See section 8.3.

## 5. S3 and object storage

The engine speaks only to the `Storage` trait. The trait has five methods:
`get`, `get_opts`, `put_opts`, `delete`, and `list`. See
`src/store/storage.rs`, `trait Storage`. Browser OPFS is a `Storage`
implementation in `crates/oxkv-opfs`, `OpfsStorage`.

The trait contract requires two error forms. A missing object is
`StoreError::Storage` with `StorageCause::NotFound` and `not found` in the
message. A failed conditional write is `StoreError::CasConflict`. See
`src/store/storage.rs`, module documentation.

### 5.1 Probe

`OxKvStoreBuilder::build` runs `probe_store` unless `skip_probe(true)` is set.
The probe uses `{prefix}/probe/canary` and checks four cases. See
`src/store/lsm/probe.rs`, `probe_store`.

1. A create succeeds.
2. A second create fails with `CasConflict`.
3. An update with a stale ETag fails with `CasConflict`.
4. An update with the current ETag succeeds.

The probe deletes the canary on success. It returns `StoreError::Storage` if
conditional writes are not enforced. The error text names B2 and Hetzner as
unsupported. See `src/store/lsm/probe.rs`, `probe_store`.

### 5.2 Conditional writes

`PutMode::Create` maps to `If-None-Match: *`. `PutMode::Update` maps to
`If-Match` with an ETag and optional backend version. See
`src/store/storage.rs`, `enum PutMode`, and `ObjectVersion`. The adapter lives
in `crates/oxkv-s3/src/lib.rs`, `impl Storage for ObjectStoreStorage`.

The engine uses conditional writes for these objects.

| Object | Mode | Source |
| --- | --- | --- |
| WAL | `Create` | `src/store/lsm/store.rs`, `append_wal_locked` |
| SST | `Create` | `src/store/lsm/store.rs`, `flush_mem_to_sst_inner` |
| Blob | `Create` | `src/store/lsm/blob.rs`, `put_blob` |
| Manifest | `Create` when absent, then `Update` with ETag | `src/store/lsm/manifest.rs`, `cas_manifest` |
| Ownership | `Create` when absent, then `Update` with version | `src/store/lsm/ownership.rs`, `acquire_ownership` |

On a WAL or SST `Create` conflict, the writer reads the existing object and
compares the bytes. Identical bytes mean a safe retry of its own write.
Different bytes are an error. The store refuses to overwrite. See
`src/store/lsm/store.rs`, `append_wal_locked` and `flush_mem_to_sst_inner`.

### 5.3 Manifest CAS

The manifest is the single consistent point for the prefix. A writer loads the
manifest and its ETag, appends or removes ids, increments `version`, and writes
with `If-Match`. The WAL path retries a conflicting CAS up to four times. The
backoff is `50ms * 2^n` plus jitter, capped at 1 second. After the fourth
failure the call returns `CasConflict`. See
`src/store/lsm/store.rs`, `append_wal_locked`, and
`src/store/lsm/ownership.rs`, `cas_backoff`.

Readers poll the manifest with `If-None-Match`. A match returns
`StoreError::NotModified`, and the reader reuses its cached manifest. The
default cache TTL is 1 second. See `src/store/lsm/manifest.rs`,
`load_manifest`, and `src/store/lsm/store.rs`, `read_ctx` call sites.

### 5.4 Read-after-write

The code gives this sequence for a successful durable write.

1. The WAL object PUT returns success.
2. Ownership still names the writer.
3. The manifest CAS lists the WAL id and returns success.
4. The writer returns `Ok`.

A reader sees the key only after step 3. `OxKvReader` revalidates the manifest
on each read and replays newly listed WAL files. See
`src/store/lsm/reader.rs`, `ensure_replayed`.

The manifest cache can serve a manifest up to its TTL. A reader with a cached
ETag sends a conditional GET. The writer's own cache skips the poll only when
`assume_single_writer(true)` is set. See `src/store/lsm/manifest.rs`,
`load_manifest`.

The code does not add a separate read-after-write consistency wait. It relies
on the configured `Storage` backend to return a successful `GET` for an object
after a successful `PUT`, and to enforce `If-None-Match` and `If-Match`. The
probe checks the conditional-write part at build time.

### 5.5 Not exposed

This branch does not expose multipart upload settings, object-store request
concurrency, or a client retry policy. Those settings belong to the
`object_store` client that the caller passes to `ObjectStoreStorage`. The
engine's own retry loops
are the four manifest CAS attempts and the one stale-manifest retry on a
`not found` read. See `src/store/lsm/store.rs`, `append_wal_locked`, and
`src/store/lsm/read.rs`, `retry_once_not_found`.

The `Storage` implementation maps backend errors to `StoreError::Storage` or
`StoreError::CasConflict`. It does not add retries. See
`crates/oxkv-s3/src/lib.rs`, `impl Storage for ObjectStoreStorage`.

## 6. Errors

`StoreError` has these variants on this branch. See `src/store/mod.rs`,
`enum StoreError`.

| Variant | Meaning | Retry guidance |
| --- | --- | --- |
| `Storage(StorageError)` | Backend, I/O, parse, or group-commit failure. `StorageError` has `message` and `cause`. | Maybe. A write can have an unknown outcome. Re-read before a retry. |
| `CasConflict(String)` | A conditional-write precondition failed. | Yes. Re-read the current state, then retry. |
| `TxConflict { key, current }` | A transaction precondition failed at commit. No staged write applied. `current` is the committed value at the check, or `None` for an absent key. | Yes. Re-read the key, stage the precondition again, and retry the transaction. |
| `Serialization(String)` | Payload or format error. | No. The same input fails again. |
| `Utf8(FromUtf8Error)` | Owned UTF-8 conversion failed. | No. |
| `Utf8Slice(Utf8Error)` | Borrowed UTF-8 conversion failed. | No. |
| `Json(Arc<serde_json::Error>)` | JSON conversion failed. | No. |
| `Other(String)` | Generic error. This includes read-only write rejection. | Depends on the cause. Not generally retryable. |
| `Rejected { key, reason }` | A validator rejected the write. | No. Change the value or the rule. |
| `Fenced(String)` | Another owner holds the epoch. | No. Terminal. Stop writing and restart through ownership CAS. |
| `NotModified` | A conditional GET matched its ETag. | Not a failure. Reuse the cached value. |

`CasConflict` is retryable. The caller must re-read the state that the
precondition used, then retry. The error docs state this. See
`src/store/mod.rs`, `StoreError::CasConflict`.

`TxConflict` is retryable. The caller must re-read the key in `current`, stage
the write and the precondition again, then retry the transaction. The variant
reports only the first failed precondition. No staged write applied. See
`src/store/tx_cas.rs`, `TxCas`, and `src/store/mod.rs`,
`StoreError::TxConflict`.

`Fenced` is terminal. The error docs state that the process must stop writing
and restart through `ownership.json` CAS. See `src/store/mod.rs`,
`StoreError::Fenced`.

A `Storage` error on a write does not prove that the write failed. The WAL PUT
can succeed and the manifest CAS can fail. The group-commit path can also lose
its leader after the WAL write. The code reports this case with the message
`group commit leader lost; write fate unknown — read back to confirm`. See
`src/store/lsm/store.rs`, `GetSet for OxKvStore::put_bytes`.

Before a retry after a `Storage` error, read the key back. For a transaction,
read every key in the transaction. Treat a retry as at-least-once delivery
unless the read proves that the write did not land.

`StoreError::kind` returns an `ErrorKind` for every variant. `ErrorKind` is the
coarse class that callers match on. `StoreError::is_retryable` returns `true`
only for `Conflict` and `Transient`. See `src/store/mod.rs`, `ErrorKind` and
`StoreError::kind`.

| `StoreError` variant | `ErrorKind` |
| --- | --- |
| `CasConflict` | `Conflict` |
| `TxConflict` | `Conflict` |
| `Storage` | `Transient` |
| `Fenced` | `Fenced` |
| `Rejected` | `Rejected` |
| `Serialization`, `Utf8`, `Utf8Slice`, `Json` | `InvalidInput` |
| `Other`, `NotModified` | `Other` |

The `Storage` class is broad. A `StorageError` has a structured `cause` field.
Read it with `StoreError::storage_cause()`.

| `StorageCause` | Meaning |
| --- | --- |
| `NotFound` | The object is absent. |
| `Network` | A network request failed. |
| `Io` | An I/O operation failed. |
| `Corrupt` | A parse or checksum check failed. |
| `LeaderLost` | The group-commit leader stopped before it reported the outcome. |
| `Other` | The failure has no finer cause. |

`StoreError::storage(message)` creates a storage error with cause `Other`.
`StoreError::storage_with_cause(message, cause)` creates one with a given
cause. The `not found` contract reads `StorageError::message`, so a missing
object still contains `not found`.

The cause is structured now. Retry classification is unchanged. Every storage
cause maps to `ErrorKind::Transient`. A `Storage` error does not prove that a
write failed. Read the key before a retry, as described above.

`StoreError::retry_after` returns `None` for every error on this branch. No
backend reports a server delay hint. Callers choose their own backoff. `None`
does not mean "retry now". See `src/store/mod.rs`, `StoreError::retry_after`.

## 7. Memory

| Item | Value or behavior | Source |
| --- | --- | --- |
| Scan limit | `Option<usize>`. `None` returns all matching rows and can exhaust memory. | `src/store/mod.rs`, `GetSet::gets_bytes` |
| Scan result | Full `Vec<KeyValue>` with cloned keys and values. | `src/store/mod.rs`, `struct KeyValue` |
| B-tree scan lock | Map read lock is held for the full page build. | `src/store/btree.rs`, `GetSet for BTreeStore::gets_bytes` |
| Transaction overlay | Unbounded `BTreeMap`. It holds full values. | `src/store/btree.rs`, `struct BTreeTx`, and `src/store/lsm/tx.rs`, `struct OxKvTx` |
| LSM MemTable flush threshold | 32 MiB estimated bytes. The estimate is `key + value + 8` per entry. | `src/store/lsm/store.rs`, `flush_mem_to_sst_inner` |
| SST cache, writer | 256 MiB default. Weighted by `SstFile::size`. | `src/store/lsm/store.rs`, `OxKvStoreBuilder::build` |
| SST cache, reader | 256 MiB default. | `src/store/lsm/reader.rs`, `default_sst_cache` |
| Blob overflow test | `key.len() + value.len() + 8 > block_size` | `src/store/lsm/blob.rs`, `is_overflow` |
| L0 blob overflow threshold | 32 KiB | `src/store/lsm/sst.rs`, `DEFAULT_BLOCK_SIZE`, and `src/store/lsm/store.rs`, `flush_mem_to_sst_inner` |
| L1 blob overflow threshold | 64 KiB | `src/store/lsm/store.rs`, `compact_inner` |
| Group-commit batch | 256 entries | `src/store/lsm/mod.rs`, `MAX_GROUP_WRITES` |
| WAL maintenance trigger | 200 manifest WAL ids | `src/store/lsm/mod.rs`, `WAL_MAINTENANCE_COUNT` |
| Event history | 1024 events | `src/store/hooks.rs`, `EVENT_HISTORY_CAPACITY` |
| Watcher queue | 256 events | `src/store/hooks.rs`, `WATCH_CAPACITY` |

The blob overflow threshold follows the SST block size, not 32 MiB. A value
spills to `e{epoch}/blob/{sha256}` when its encoded record does not fit one
block. The L0 flush uses a 32 KiB threshold. The L1 compaction uses a 64 KiB
threshold. The 32 MiB number is the `MemTable` flush threshold. See
`src/store/lsm/store.rs`, `flush_mem_to_sst_inner` and `compact_inner`, and
`src/store/lsm/blob.rs`, `is_overflow`.

The SST cache is a caller-replaceable `Cache`. `build_with_cache` accepts any
implementation. The default is the built-in `LruCache` `S3-FIFO`. See
`src/store/lsm/store.rs`, `build_with_cache`.

## 8. Gaps and recommended patterns

### 8.1 Present and absent capabilities

The following table states what the code provides and what it does not
provide. Do not rely on an absent feature.

| Feature | Status |
| --- | --- |
| TTL and key expiry | Absent. There is no expiry field and no background expiry job. `ScanExt::scan_older_than` and `BatchExt::delete_prefix_before` are cleanup helpers that the caller must run on a schedule. |
| Stable snapshot reads | Present. `StoreExt::snapshot` returns a read-only `Snapshot` that copies every key and value one time. A later write or delete does not change the snapshot. The snapshot uses memory equal to the size of the store. |
| Public conditional writes | Present. `Cas` provides `set_if_absent`, `compare_and_set`, and `delete_if` for one key. `CasResult` reports `Applied` or `Conflict { current }`. `TxCas` provides the same three methods for several keys in one transaction and reports `StoreError::TxConflict` at commit. |
| Keys-only scans | Present. `GetSet::gets_keys`, `ScanExt::scan_keys`, and `ScanExt::scan` with `values = false` return keys without output values. |
| Batch helpers | Present. `BatchExt` provides `get_many`, `write_batch`, `delete_prefix_chunked`, and `delete_prefix_before`. `BatchOp` describes one write. |
| Durable change feed | Present. `ChangeFeed` writes an append-only event log under `{prefix}/{seq:020}` and keeps the counter at `{prefix}/next`. It gives cursor reads, CAS sequence allocation, and chunked pruning. Delivery is at-least-once. It is not automatic change capture. |
| Cross-store transactions | Absent. A transaction covers one store. |
| Lease and fencing helpers | Absent. `ownership.json` epoch fencing is internal to the LSM writer. There is no public lease type and no fencing token API. |
| Metrics and scoped handles | Present. `StatsStore` counts client-side API calls, returned bytes, scan rows, transaction completions, and errors. `ScopedStore` maps every key to `{prefix}/{key}`, strips the prefix from returned keys, maps cursors, keeps scans inside the scope, and delegates transactions. The `oxkv-otel` crate provides the optional `OtelStore` decorator for exported telemetry. |
| Filtered export and restore | Absent. The whole-store export and restore are present as `save`/`load` and `save_stream`/`load_stream`. There is no filtered export or restore API, and the dump is not a point-in-time backup. |
| `ErrorKind` and `is_retryable` | Present. `StoreError::kind` maps every variant to an `ErrorKind`. `StoreError::is_retryable` is true only for `Conflict` and `Transient`. |

`events_since` keeps at most 1024 events in memory. A watcher queue holds at
most 256 events. A slow watcher can miss events. History does not survive a
restart. Use `ChangeFeed` for a durable log. See `src/store/hooks.rs`,
`EVENT_HISTORY_CAPACITY`, `WATCH_CAPACITY`, and `notify`.

### 8.2 Retention scan and delete

There is no TTL. The store has no expiry metadata and no background expiry
job. To remove expired keys, encode the expiry time in the key and run a
cleanup job. The job has three steps.

1. Compute the cutoff key.
2. Delete the keys below the cutoff in bounded chunks.
3. Repeat the job on a schedule.

`cutoff` is an exclusive lexicographic key bound. A key that is equal to the
cutoff or above it stays. Use a fixed-width time field so that lexicographic
order matches time order.

```text
// Keys use timer/<zero-padded-deadline-ms>/<id>.
let cutoff = format!("timer/{deadline_ms:020}/");
let deleted = store
    .delete_prefix_before("timer/", &cutoff, 500)
    .await?;
```

`BatchExt::delete_prefix_before` commits one transaction per chunk. Each
chunk is atomic. The whole range is not atomic. A failure between chunks
leaves the earlier chunks deleted. The method returns the number of keys
deleted.

Use `ScanExt::scan_older_than` when the caller must inspect the keys before
it deletes them. The method returns one `Page<String>`. `Page::next` is the
cursor for the next call.

```text
let mut after = None;
loop {
    let page = store
        .scan_older_than(KeyPrefix::new("timer/"), &cutoff, after, 100)
        .await?;
    for key in page.items { handle(key) }
    match page.next {
        Some(next) => after = Some(next),
        None => break,
    }
}
```

Use a finite page size. Advance the cursor after each page. `scan_older_than`
calls the keys-only scan, so the job does not read values. The prefix bound
already removes unrelated keys such as `timer0`. See `src/store/scan.rs`,
`scan_older_than`.

`ScanExt::scan` with a `KeyPrefix` makes the loop shorter. It still reads
values to test `is_expired`. `BatchExt::delete_prefix_chunked` applies only
when every key under the prefix is expired. The delete is not atomic across
chunks. It can stop between chunks and leave some keys deleted.

Both methods read the current committed state. They do not read a snapshot.
A concurrent writer can add a key below the cutoff after the read. Run the
cleanup job from one writer, or accept that the next run deletes the late
key. See `src/store/scan.rs` and `src/store/batch.rs`.

### 8.3 Windowed scans

A long scan does not return a stable snapshot. Page in small windows and keep
your own cursor. To read a fixed view, call `StoreExt::snapshot` first and
scan the `Snapshot`. On the B-tree backend, a windowed scan also releases the
read lock between pages. See section 3.

```text
let mut start = Some("feed/".to_string())
loop {
    let page = reader.gets_bytes(Some(100), Next, (start, Some("feed0".to_string())))
    for row in page {
        if row.key.starts_with("feed/") { handle(row) }
    }
    if page.len() < 100 { break }
    start = Some(page.last().key.clone() + "\0")
}
```

`"feed0"` is above every key that starts with `feed/`, because `/` sorts
before `0`. The upper bound is inclusive, so keep the `starts_with` filter.
For a general prefix, compute the next prefix by incrementing the last byte.
If the last byte is `0xFF`, trim it and increment the previous byte. If no
byte can be incremented, use `None` as the upper bound. Then also filter every
returned key with `starts_with(prefix)`.

`KeyPrefix` and `ScanExt` provide this window directly. `KeyPrefix::new`
builds the prefix, and `KeyPrefix::upper_bound` computes the exclusive end.
`ScanExt::scan_keys` returns one `Page<String>`. `Page::next` carries the
cursor for the next call. See `src/store/scan.rs`.

```text
let mut after = None;
loop {
    let page = reader.scan_keys(KeyPrefix::new("feed/"), after, 100).await?
    for key in page.items { handle(key) }
    match page.next {
        Some(next) => after = Some(next),
        None => break,
    }
}
```

### 8.4 Prefix models

Keys are `String` values. Order is lexicographic byte order. Model a namespace
as a prefix with a separator.

```text
entity/<id>
index/by-email/<email>/<id>
outbox/<zero-padded-sequence>
timer/<deadline-ms>/<id>
```

A prefix range is `(Some(prefix), Some(prefix_upper))`. Keep the upper bound
exclusive in your own code, or make it inclusive and skip the sentinel key.
`ScanExt` wraps `gets_bytes` with a prefix parameter and computes the
exclusive upper bound through `KeyPrefix::upper_bound`.

### 8.5 Durable outbox and change feed

Use a transaction to make the business write and the outbox row visible
together. Both rows go into one WAL object. A dispatcher reads the outbox in
order, delivers the event, then deletes the row. This gives at-least-once
delivery. Make the consumer idempotent.

```text
let tx = store.begin_tx()?
tx.put_bytes("entity/42", entity_bytes).await?
tx.put_bytes("outbox/000000000001", event_bytes).await?
tx.commit().await?          // one WAL object, atomic visibility

loop {
    let rows = reader.gets_bytes(Some(100), Next,
        (Some("outbox/"), Some("outbox0"))).await?
    for row in rows {
        deliver(&row).await?       // consumer must be idempotent
        store.delete(&row.key).await?
    }
    if rows.len() < 100 { break }
}
```

The sequence number comes from the caller. The store has no sequence
generator. A crash after `deliver` and before `delete` redelivers the event.
See `src/store/lsm/tx.rs`, `Transaction for OxKvTx::commit`.

`BatchExt::write_batch` writes the entity row and the outbox row together. It
opens its own transaction, so it cannot include reads in that transaction.
`ScanExt::scan_keys` pages the outbox keys with a `KeyPrefix`.

### 8.6 Durable change feed

`ChangeFeed` provides the durable event log. It writes events under
`{prefix}/{seq:020}` and keeps the next sequence in `{prefix}/next`. The first
sequence is 1.

- `next_seq` updates the counter with `Cas::compare_and_set`. Concurrent
  callers cannot get the same sequence number.
- `append` calls `next_seq` and then writes the event. A crash between the two
  steps leaves a gap. A consumer must skip the gap and continue.
- `append_in_tx` writes an event with a caller-supplied sequence. The caller
  can put a business row and the event in one transaction. `next_seq` is not
  part of that transaction. A rollback after `next_seq` leaves a gap.
- `since` returns the events with `seq > cursor` in ascending order. It
  returns at most `limit` events. A `limit` of 0 returns no events.
- `prune` deletes the events with `seq < before` in chunks. It is not atomic
  across chunks. It returns the number of deleted events.
- Delivery is at-least-once. A consumer must be idempotent and use the
  sequence number as the idempotency key.
- `ChangeFeed` is an event-log helper. It is not automatic change-data
  capture. The store does not write a feed event for every write. The caller
  writes each event.

See `src/store/feed.rs`, `ChangeFeed` and `FeedEntry`.

### 8.7 Batch helper costs

`BatchExt::get_many` reads the keys one at a time. It calls `get_bytes` once per
key. It is not a server-side multi-get and it does not coalesce reads. The cost
grows with the number of keys.

`BatchExt::write_batch` opens its own transaction. It cannot include reads in
that transaction. To read and write in one transaction, call `Store::begin_tx`
directly.

`BatchExt::delete_prefix_chunked` commits one chunk per transaction. A failure
between chunks leaves the earlier chunks deleted. The method is not atomic
across chunks.

### 8.8 One store per consistency domain

A transaction cannot span two stores. If two data sets must commit together,
put them in one store. If they can be eventually consistent, use separate
stores and an outbox. See `src/store/mod.rs`, `trait Store` and
`trait Transaction`.

### 8.9 Streaming export is a logical dump

`StoreExt::save` and `StoreExt::save_stream` page live reads. They do not
freeze a view. A concurrent committed write can appear between pages. A
concurrent delete can be missed. The output is a logical dump. It is not a
point-in-time backup. For a fixed view, call `StoreExt::snapshot` first and
then call `save_stream` on the snapshot. See section 8.3,
`src/store/mod.rs`, `StoreExt::save_stream`, and `struct SaveStream`.

### 8.10 Load size limits

`load_stream` and `StoreExt::load` decode incrementally. Decoding memory stays
bounded by the largest pending record. Every decoded record is staged in one
transaction. The transaction overlay grows with the total payload. The commit
writes one WAL object. A large backup therefore costs memory and one large
object. `load_chunked` and `load_stream_chunked` commit one transaction every
`page_size` records. A failure after a commit leaves the earlier pages
committed. See `src/store/mod.rs`, `load_stream` and `load_stream_chunked`.

Keys and values use 32-bit lengths. A key or value of 4 GiB or more fails.
There is no explicit entry-count cap. See `src/store/mod.rs`, `encode_record`
and `RecordDecoder`.

### 8.11 Multi-key unique claim

`Cas` covers one key. A unique claim that must commit with an entity row,
attribute rows, index rows, and an outbox row cannot use `Cas` alone. Use
`TxCas` to stage the claim and the other writes in one transaction. The commit
checks every precondition under the same lock or gate that applies the overlay.
See `src/store/cas.rs`, module documentation, and section 8.5.

```text
let tx = store.begin_tx()?
tx.set_if_absent("index/by-email/a@b/42", b"").await?
tx.put_bytes("entity/42", entity_bytes).await?
tx.put_bytes("outbox/000000000001", event_bytes).await?
tx.commit().await?                 // checks every precondition, one WAL object
```

The transaction makes every row visible together. `TxCas` fails the commit
with `StoreError::TxConflict` when one precondition does not match. A single
writer lease is still useful to reduce conflict retries across processes.

### 8.12 Client-side metrics and scoped handles

`StatsStore::new` wraps any `GetSet`, `Store`, or `Cas` backend. `stats`
returns a `StoreStats` snapshot. `reset` sets every counter to zero.
`into_inner` returns the wrapped store. Clones share the atomic counters.

The counters count calls that reach the wrapper:

- `gets`, `has`, `sets`, `puts`, and `deletes` count the matching method calls.
- `scans` counts `gets_bytes` and `gets_keys` calls. `scan_items` counts the
  returned rows or keys.
- `bytes_read` counts value bytes returned by reads. `bytes_written` counts
  value bytes accepted by writes.
- `tx_commits` and `tx_rollbacks` count transaction completion calls.
- `errors` counts calls that return a `StoreError`. A failed call also
  increments its method counter.
- `Cas` calls only change `errors` when they fail.

These are client-side API counters. They do not count S3 requests, WAL
objects, or compaction backlog. The LSM cache statistics remain the source for
cache hits and misses. See `Cache::stats` and `OxKvStore::sst_cache_stats`.

`StoreExt::scoped(prefix)` returns a `ScopedStore` that owns a clone of the
store. Every key maps to `{prefix}/{key}`. An empty prefix leaves keys
unchanged. Reads return keys without the prefix. Cursors map in both
directions. A scan stays inside the scope when a cursor is unbounded.
`begin_tx` returns a `ScopedTx` that maps keys and delegates commit and
rollback. For `OxKvStore`, the clone shares the cache, client, and session.

`StoreStats` is a snapshot, not a transaction. Concurrent calls can change
counters between field reads.
