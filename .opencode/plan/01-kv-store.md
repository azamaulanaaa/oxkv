# 1. Ordered KV store

The ordered key-value store is the substrate on which every other oxkv contract rests. It must present each shard as one totally ordered map from immutable byte keys to versioned byte values, and it must do so through a single interface that both a B-tree engine and an LSM engine satisfy. Ordering is not a storage detail. It is the property that turns a prefix into a contiguous range, a range into a resumable stream, a stream into a stable page, and a page into a snapshot-consistent answer. If keys have no total order, the goal has no shard map, no `KeyCursor`, and no honest receipt.

## What the greater goal demands

The greater goal says a query is the unit of ownership. A client declares a scope: a policy-filtered query over user-table prefixes, whose matching data materializes locally. A prefix is contiguous only under a total order of the key space. A data shard contains one user table and its system sub-namespaces, so the shard map is a partition of one ordered space. Placement, ownership, transaction scope, query fan-out, sync scope, and replication all inherit that partition. If two nodes can disagree about the order, they can disagree about which shard owns a key. If native and wasm32 can disagree about the order, a ShardSnapshot from one target cannot resume on the other.

The 1.0 freeze of C2 (snapshot and page format), A1 (key encoding), and C3 (sync wire protocol) makes ordering a protocol obligation, not an optimization. C2 must encode a slice of an ordered stream. C3 must stream diffs with a `SyncCursor` and resume after a disconnect. Resume is sound only when the `SyncCursor` names a position in a total order at a pinned `commit_version`, because the data after that position must not shift while the client is away. The receipt contract needs batches that commit fully or not at all, since a receipt that reports `Committed` for a half-applied request is a lie. Replicas must answer a search and a get at one version, so the store must expose reads at a pinned `commit_version`. Each of these contracts terminates in this layer.

## Required capabilities

- The store must support point reads and point writes on byte keys within a shard.
- The store must support range scans over inclusive lower and upper bounds, in ascending and descending byte order.
- The store must support bidirectional `KeyCursor` values that resume at an exact position and remain stable across pages at a pinned `commit_version`.
- The store must support atomic batches. A batch must commit all operations or none.
- The store must expose one interface with replaceable engines. A B-tree engine and an LSM engine must both satisfy it in-tree.
- The store must support versioned reads at a caller-supplied `commit_version`.
- The store must apply operations in a deterministic order inside a batch, so that post-write hooks and the business event stream can replay them.
- The store must preserve committed state across restart and must recover to the last committed batch.
- The store must return named errors for a missing key, an invalid range, an unknown `KeyCursor` version, and a cross-shard batch.
- The store must enforce key, value, batch, and page size limits. Each breach must return a named error.
- The store must place every system key under the `0xFF` system marker and a registered system identifier.
- The store must expose `page_id` and `page_sequence` for each framed page.
- The store must return an exact result by default and a coverage result only on explicit request.

## Invariants that must hold

The comparator must be lexicographic over unsigned bytes. It must not depend on locale, platform, pointer width, or the selected engine.

Every key must belong to exactly one shard. Shard ranges must be disjoint, and together they must cover the declared key space. A user key must never use `0xFF` as a component tag, so the user key space stays disjoint from the system key space.

A read at `commit_version` V must observe exactly the writes committed at or before V. It must not observe a partial batch or a later version.

A `KeyCursor` must carry a `commit_version` and a resume position. A continuation at the same `commit_version` must produce the same remaining sequence, in either direction.

A range scan must return every live key inside the bounds exactly once, in the declared order.

A batch must become visible at one `commit_version` or not at all. `commit_version` values must increase monotonically per shard.

A page must carry a deterministic `page_id` and a strictly increasing `page_sequence` inside one ShardSnapshot.

Two engines must produce the same logical results for the same operation log. Native and wasm32 must produce the same bytes for the same operation log.

## Decisions and technical choices

**Comparator: typed per table or bytewise on encoded keys.** A typed comparator can order fields naturally, for example numbers numerically and strings by a chosen collation. The cost is that every engine must reproduce the same type rules, and a divergence becomes a silent correctness bug. A bytewise comparator is smaller and easier to test, but it pushes semantic order into the key encoding. Recommendation: bytewise order on canonical encoded keys. Encoding is the only place that knows field semantics. This choice constrains later work. A1 must freeze at 1.0, and any change to sort order requires a new `key_encoding_version`.

**KeyCursor: raw last key or opaque token.** A raw last key is easy to inspect and debug. It cannot pin a `commit_version`, so concurrent writes can make a later page repeat or skip a key. An opaque token can carry the `commit_version`, the resume key, the direction, and a scope fingerprint. The cost is a new validation surface and a versioned token format. Recommendation: an opaque, versioned `KeyCursor` token. The token becomes frozen bytes in A1 and C3, so it also needs a named error for an unknown version and a defined expiry rule.

**Engine seam: one engine with a reserved interface or two in-tree engines.** A single engine is cheaper to finish and easier to optimize. A seam with one implementation is a guess about an interface that no second implementation has tested. Recommendation: ship the interface with both a B-tree engine and an LSM engine. This choice constrains every later capability, because ShardSnapshots, `KeyCursor` values, and batches must be expressible in both. The interface may not leak engine-specific behavior, and differential tests must compare the engines on the same operation log.

**Batch scope: cross-shard atomicity or single-shard atomicity.** A cross-shard batch would need a second consensus problem, and 2PC is out of scope for 1.0. A single-shard batch matches the rule that a shard is the transaction scope. Recommendation: atomic batches are single-shard, and a cross-shard batch returns `shard.cross_mutation_rejected` with no state change. Any future multi-shard write must arrive as a new protocol with its own named semantics, not as a quiet extension of this interface.

## Dependencies

The store depends on a canonical key encoding for byte order and prefix bounds. It provides the ordered primitive that C2 (snapshot and page format), C3 (sync wire protocol), and C5 (receipt and error semantics) consume. The manifest must declare table names and indexes on it. The write scheduler must name a per-key order inside a shard. Pre-write hooks must run before a batch commits, and post-write hooks must run after commit in the batch order. Tiered storage must preserve logical order across RAM, disk, and object storage. Search must build its index inside the same commit boundary, so a search and a get agree at one `commit_version`.

## Acceptance criteria

- A point read at `commit_version` V returns the committed value, or it returns `key.not_found`.
- A range scan returns every live key inside the inclusive bounds in ascending byte order, and each key appears one time.
- A forward `KeyCursor` and a reverse `KeyCursor` over the same pinned `commit_version` return the same key set.
- A `KeyCursor` resumes after page N with no duplicate key and no skipped key while writes continue.
- A batch commits all operations or commits none. A rejected batch leaves no visible state.
- A cross-shard batch returns `shard.cross_mutation_rejected` and changes no state.
- An unknown `KeyCursor` version returns `sync.cursor_invalid` and does not advance the `KeyCursor`.
- A user key with `0xFF` fails with `key.reserved_system_marker`.
- A system key with an unknown identifier fails with `key.unknown_system_id`.
- A page with a missing `page_id` fails with `format.corrupt_page`.
- An exact read with one unavailable shard returns a named error and no data. A coverage read returns a coverage descriptor.
- The B-tree engine and the LSM engine return the same ordered results for the same operation log.
- A restart returns every committed batch and no uncommitted batch.

## What would falsify this design

The bytewise decision fails if a higher contract needs an order that canonical byte encoding cannot express without a new comparator. The opaque `KeyCursor` fails if a token cannot be bounded in size and validated cheaply, or if ShardSnapshot retention cannot be bounded. The two-engine seam fails if ShardSnapshots or `KeyCursor` values cannot share one semantic contract without leaking engine behavior. The single-shard batch fails if a real write request cannot be expressed without cross-shard mutation, because then 2PC becomes mandatory. The whole design fails if range pagination costs grow with the number of skipped keys, or if `KeyCursor` stability breaks under a realistic write rate.

## Open questions

- Must `KeyCursor` tokens be signed, or is a version plus checksum sufficient?
- Should the store expose `commit_version` values directly, or hide them behind a ShardSnapshot handle owned by the authority?
- How long may a ShardSnapshot live before compaction invalidates it, and which named error reports that expiry?
- What memory budget per shard applies to retained ShardSnapshots and open `KeyCursor` values?
