# OxKV contracts

This document states the behavior that the code on this branch provides. It
covers the in-memory B-tree backend and the durable LSM backend. Each claim
names the source file and function that implements it.

The crate version is 0.10.0. The contracts below describe this branch only.
They do not describe requested features that are absent from this branch. The
Lucene-style query engine lives in the `oxkv-query` crate at
`crates/oxkv-query`.

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
| Rollback | `BTreeTx::rollback` does nothing. The overlay is dropped with the handle. |
| Conflict detection | None. `commit` does not compare versions or re-check keys. |
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
| Commit, empty | An empty overlay returns `Ok(())` before the write gate. |
| Commit, non-empty | The transaction writes one WAL object that holds all staged records. One manifest CAS lists that object. Then the `MemTable` changes. |
| Rollback | `OxKvTx::rollback` does nothing. The overlay is dropped with the handle. |
| Conflict detection | None. `commit` does not compare versions or re-check keys. |
| Same-key race | Commits serialize on the write gate. The later commit gets the higher WAL sequence. Replay applies WAL files in manifest order. The later value wins. No error is returned. |

`OxKvTx::commit` runs these steps in order. See `src/store/lsm/tx.rs`,
`Transaction for OxKvTx::commit`.

1. Re-validate every staged `Set` event.
2. Take the `write_gate`.
3. Drain the transaction overlay.
4. Encode all overlay updates into one WAL payload.
5. Drain writes staged on the parent store with `stage_set` or `stage_delete`.
6. Call `append_wal_locked`. This writes one WAL object with
   `PutMode::Create`, checks ownership, and appends the WAL id to the manifest
   with a CAS.
7. Apply the updates to the `MemTable` only after the WAL and manifest steps
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
backend. There is no optimistic conflict detection and no transaction abort on
a write-write race. Both backends provide read-your-own-writes and atomic
visibility at commit. Reads outside the overlay read current committed state.
Two calls in one transaction can therefore observe different committed states.
The same-key result is last-commit-wins.

There is no stable snapshot API on this branch.

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
gets_bytes(limit: Option<u32>, direction: Direction, cursor: (Option<String>, Option<String>))
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
streaming save path uses this pattern. See `src/store/mod.rs`, `SaveStream`
and `crates/oxkv-query/src/lib.rs`, `QueryExt::gets`.

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
paging. The limit type is `Option<u32>`.

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

## 5. S3 and object storage

The engine speaks only to the `Storage` trait. The trait has five methods:
`get`, `get_opts`, `put_opts`, `delete`, and `list`. See
`src/store/storage.rs`, `trait Storage`. Browser OPFS is a `Storage`
implementation in `crates/oxkv-opfs`, `OpfsStorage`.

The trait contract requires two error forms. A missing object is
`StoreError::Storage` with `not found` in the message. A failed conditional
write is `StoreError::CasConflict`. See `src/store/storage.rs`, module
documentation.

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
| `Storage(String)` | Backend or I/O failure. | Maybe. A write can have an unknown outcome. Re-read before a retry. |
| `CasConflict(String)` | A conditional-write precondition failed. | Yes. Re-read the current state, then retry. |
| `Serialization(String)` | Payload or format error. | No. The same input fails again. |
| `Utf8(FromUtf8Error)` | Owned UTF-8 conversion failed. | No. |
| `Utf8Slice(Utf8Error)` | Borrowed UTF-8 conversion failed. | No. |
| `Json(Arc<serde_json::Error>)` | JSON conversion failed. | No. |
| `Other(String)` | Generic error. This includes read-only write rejection and query parse errors. | Depends on the cause. Not generally retryable. |
| `Rejected { key, reason }` | A validator rejected the write. | No. Change the value or the rule. |
| `Fenced(String)` | Another owner holds the epoch. | No. Terminal. Stop writing and restart through ownership CAS. |
| `NotModified` | A conditional GET matched its ETag. | Not a failure. Reuse the cached value. |

`CasConflict` is retryable. The caller must re-read the state that the
precondition used, then retry. The error docs state this. See
`src/store/mod.rs`, `StoreError::CasConflict`.

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
| `Storage` | `Transient` |
| `Fenced` | `Fenced` |
| `Rejected` | `Rejected` |
| `Serialization`, `Utf8`, `Utf8Slice`, `Json` | `InvalidInput` |
| `Other`, `NotModified` | `Other` |

The `Storage` class is broad. It covers a backend failure, an I/O failure, and
a lost group-commit leader. A `Storage` error does not prove that a write
failed. Read the key before a retry, as described above.

`StoreError::retry_after` returns `None` today. No backend reports a server
delay hint. Callers choose their own backoff. Do not read `None` as "retry
now". See `src/store/mod.rs`, `StoreError::retry_after`.

## 7. Memory

| Item | Value or behavior | Source |
| --- | --- | --- |
| Scan limit | `Option<u32>`. `None` returns all matching rows. | `src/store/mod.rs`, `GetSet::gets_bytes` |
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
| TTL and key expiry | Absent. There is no expiry field or background expiry job. |
| Stable snapshot reads | Absent. `gets_bytes` returns a materialized `Vec`, not a snapshot. |
| Public conditional writes | Present. `Cas` provides `set_if_absent`, `compare_and_set`, and `delete_if`. `CasResult` reports `Applied` or `Conflict { current }`. |
| Keys-only scans | Present. `GetSet::gets_keys`, `ScanExt::scan_keys`, and `ScanExt::scan` with `values = false` return keys without output values. |
| Batch helpers | Present. `BatchExt` provides `get_many`, `write_batch`, and `delete_prefix`. `BatchOp` describes one write. |
| Durable change feed | Absent. `events_since` uses an in-memory history. |
| Cross-store transactions | Absent. A transaction covers one store. |
| Lease and fencing helpers | Absent. `ownership.json` epoch fencing is internal to the LSM writer. There is no public lease type and no fencing token API. |
| Metrics and scoped handles | Absent. The core store has no metrics or scoped-handle API. The `oxkv-otel` crate provides an optional `OtelStore` decorator. |
| Export and restore | Absent. `save`/`load` and `save_stream`/`load_stream` move a whole snapshot. There is no filtered export or restore API. |
| `ErrorKind` and `is_retryable` | Present. `StoreError::kind` maps every variant to an `ErrorKind`. `StoreError::is_retryable` is true only for `Conflict` and `Transient`. |

`events_since` keeps at most 1024 events in memory. A watcher queue holds at
most 256 events. A slow watcher can miss events. History does not survive a
restart. See `src/store/hooks.rs`, `EVENT_HISTORY_CAPACITY`,
`WATCH_CAPACITY`, and `notify`.

### 8.2 Retention scan and delete

There is no TTL. A caller can store an expiry field in the value or in the key
and run a bounded scan and delete job.

```text
let mut start = Some("timer/".to_string())
loop {
    let page = store.gets_bytes(Some(500), Next, (start, Some("timer0")))
    for kv in page {
        if kv.key.starts_with("timer/") && is_expired(kv) {
            store.delete(&kv.key).await?
        }
    }
    if page.len() < 500 { break }
    start = Some(page.last().key.clone() + "\0")
}
```

Use a finite page size. Advance the cursor after each page. The store has no
server-side filter, so the job reads values. The `starts_with` check removes
the `timer0` sentinel and any unrelated key. See `src/store/mod.rs`,
`GetSet::gets_bytes`.

`ScanExt::scan` with a `KeyPrefix` makes the loop shorter. It still reads
values to test `is_expired`. `BatchExt::delete_prefix` applies only when every
key under the prefix is expired.

### 8.3 Windowed scans

A long scan does not return a stable snapshot. Page in small windows and keep
your own cursor. On the B-tree backend, a windowed scan also releases the read
lock between pages. See section 3.

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

### 8.5 Durable outbox

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

`BatchExt::write_batch` writes the entity row and the outbox row in one
transaction. `ScanExt::scan_keys` pages the outbox keys with a `KeyPrefix`.

### 8.6 One store per consistency domain

A transaction cannot span two stores. If two data sets must commit together,
put them in one store. If they can be eventually consistent, use separate
stores and an outbox. See `src/store/mod.rs`, `trait Store` and
`trait Transaction`.
