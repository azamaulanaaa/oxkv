# 15. Client replica

A client replica is a read-only store of scope-projected pages plus a diff tail, not a second database engine. Because clients never write, the replica stores ScopeSnapshot pages unchanged, so page identity becomes structural. The replica builds its own search index from scope members. It owns a query, not a data set. It answers reads and searches for one policy-filtered scope at a nameable scope version vector, online or offline.

## What the greater goal demands

A query is the unit of ownership. A client declares a policy-filtered query, a scope. Matching data materializes locally, stays searchable offline, and syncs incrementally. Authority owns truth and serializes writes. The replica is read-only and holds a scope, a replica-built index, and an outbox.

Clients never write. A write is a request with an idempotency key and a `base_version`. A receipt is `Queued`, `Committed`, `Rejected`, or `Conflict`. A data shard contains one user table and its system sub-namespaces, and it is the unit of placement, ownership, transaction, fan-out, scope, and replication. One deterministic, bounded policy language governs scope and validation through one evaluator. The business event stream is the only path to write side effects. The seam rule requires two in-tree implementations, sans-IO protocol cores, and readable behavior before fast behavior.

These rules forbid the tempting design: local mutation, independent compaction, or a fifth receipt state violates the goal even if every read passes.

## Required capabilities

1. **ScopeSnapshot storage.** Store the authority ScopeSnapshot header and pages without re-encoding, re-blocking, or re-ordering. A page keeps its authority `page_id` and `page_sequence`.
2. **Diff tail.** Keep an append-only ordered diff log. Each entry names shard, `commit_version` range, `SyncCursor`, and page changes. The tail is the only mutable structure.
3. **Monotonic reads.** Every read observes a scope version vector that never moves backward for one replica identity. A read pins one scope version vector.
4. **Exposed version and lag.** Report the applied version vector, authority watermark, lag in versions and time, and hydration state.
5. **Offline read and search.** Answer reads and searches for the scope with no network, using a replica-built index under the authority `tokenizer_version` and the manifest scorer.
6. **Eviction bounded by the scope.** Drop pages the scope no longer admits or the ceiling cannot hold. Eviction changes residency, not scope meaning. Eviction must not remove an in-scope logical member.
7. **Multi-tab coordination.** Use one replica identity per origin, with one cursor advancer. Other tabs read the shared store through a local tab lease. A replica must not hold an authority lease.
8. **Memory ceiling.** Hold pages, tail entries, index data, and query working memory under a ceiling. Include the hydration peak in the budget. Breach fails at subscribe with `scope.hydration_budget_exceeded` or degrades service by eviction and reporting.
9. **Integrity checking.** Verify checksums on ingest. Never serve a page that fails verification. Refetch instead.
10. **Outbox without a write engine.** Store write requests, idempotency keys, `base_version` values, and last receipts. Never apply a local mutation to the ScopeSnapshot store.

## Invariants that must hold

- **I1 — Page identity.** Every stored page equals the authority page for the same `shard_id`, `page_id`, `page_sequence`, and `commit_version`, byte for byte.
- **I2 — Version monotonicity.** The applied scope version vector never decreases for one replica identity. A format change forces full rehydration, not a lower version.
- **I3 — Single advancer.** At most one component advances the `SyncCursor` per replica identity at a time.
- **I4 — Read atomicity.** A read pins one scope version vector. A get and a search at that pin agree.
- **I5 — Scope closure.** The store holds only scope-projected pages, or pages needed to evaluate the scope. Unfiltered data does not rest on the client.
- **I6 — No local mutation.** Stored pages change only from authority data. The client changes only its outbox and `SyncCursor`.
- **I7 — Bounded memory.** Resident bytes stay at or below the ceiling at all times, including the hydration peak.
- **I8 — Deterministic replay.** The same diff sequence from the same `SyncCursor` yields the same bytes and read results.
- **I9 — Terminal receipts.** Every submitted write reaches `Queued`, `Committed`, `Rejected`, or `Conflict`. The replica invents no fifth state and never resubmits a committed request under a new key.
- **I10 — Fail closed on policy.** If the pinned `policy_version` is unknown, scope evaluation refuses. It does not guess.

## Decisions and technical choices

**Choose a scope store, not a full engine and not a pure cache.** A full engine gives the replica its own store, compaction, index, and format. Two implementations must then agree on ordering, compaction, and tokenization. Page identity becomes a failing test, not a property. A full engine also invites a local write path. A pure cache is simpler but cannot hold a scope offline for a day, keep an index across restart, or bound memory while retaining the scope. The scope store gives page identity by construction and keeps one mutable tail. Seam rule: two in-tree backends, memory and durable.

**The replica is a projection, not a compaction authority.** The authority decides page layout and compaction. The replica may drop pages by eviction. It must not rewrite, merge, or re-block pages. A merged page has no authority `page_id` or `commit_version`, cannot be validated, and cannot be repaired by refetch. Independent compaction would let two clients disagree about the same query. A second writer would need its own conflict resolution.

**Monotonic reads with an explicit stale window.** Reads default to the highest applied scope version vector. A caller may pin an older scope version vector only inside the advertised retention window. A pin outside is a named error, not a silent fresher read. Lag is an output, not an error: the replica never blocks a read to chase freshness beyond a declared bound. A replica local pin is the applied scope version vector. It remains readable offline and does not expire with authority retention.

**One tail per replica identity, not per tab.** Tabs share one scope store and `SyncCursor` through a local tab lease. A follower reads through shared storage or the leader. Independent `SyncCursor` values would duplicate tail entries and diverge versions.

**The ceiling is admission control.** Pages and tail entries are admitted under a budget that includes the hydration peak. When full, the replica evicts out-of-scope or superseded pages.

**The outbox is separate from the scope store.** Outbox records hold idempotency key, `base_version`, scope, payload hash, and last receipt per key. This separation makes clients non-writers in storage.

## Dependencies

- A5 (scope semantics) (13) and C4 (policy language) (8), through one evaluator.
- C3 (sync wire protocol), sans-IO (14): subscribe, `SyncCursor`, resume, ScopeSnapshot fallback.
- C2 (snapshot and page format) (6) and A1 (key encoding) (5): read-only inputs that fix bytes and ordering.
- Write request, receipt, and outbox semantics (9, 10, 11).
- Hydration and recovery (16), for the atomic ScopeSnapshot-to-tail handoff.
- Index format and snapshot-consistent search (18, 19), plus the shard map (22) for scope-to-shard fan-out.

## Acceptance criteria

1. After 24 hours offline, reads and searches return the scoped result at the last applied scope version vector.
2. After a disconnect, sync resumes from the `SyncCursor` with no full resync and equals a fresh ScopeSnapshot at the same scope version vector.
3. A get and a search at one pinned scope version vector agree, and pagination stays stable.
4. A scope larger than RAM must use a separate degraded contract and must not claim exact offline. A scope that fits the device budget serves every member offline. A scope that cannot fit the device budget fails at subscribe with `scope.device_budget_exceeded`.
5. Two tabs produce one `SyncCursor` advance and no duplicate tail entries. A mid-diff restart serves no torn page and never regresses the `SyncCursor`.
6. An injected corrupt page fails checksum verification, is refetched, and is never served.
7. An offline write stays `Queued` under one idempotency key, and reconnect yields one terminal receipt.
8. A fault injection that mutates stored pages returns a named error. Scope shrink evicts out-of-scope pages and keeps in-scope reads correct.
9. A search at a pin returns no hit outside the scope, and a replica index build is byte-identical on native and wasm32.

## What would falsify this design

- If a required read needs re-encoding, the scope store choice is wrong.
- If the scope is much smaller than an authority page, page eviction cannot bound memory to the scope, and the design needs a finer unit.
- If compaction rewrites pages under a stable version, byte identity is not structural, and the replica needs version-addressed pages.
- If multi-tab coordination costs more than one shared worker, the design is wrong.
- If lag needs a round trip to measure, exposed lag is not honest.
- If search needs a local tokenizer that differs from the authority's, snapshot-consistent search breaks.
- If the product needs a local optimistic writer, the premise fails.

## Open questions

1. What is the eviction unit: page, shard, or scope slice? The page is authority-native, but a scope can span pages.
2. How is scope closure proved when a page mixes in-scope and out-of-scope rows: read-time filtering, or filtered pages from the authority?
3. Is the ceiling hard or a target with degradation? A hard limit needs deterministic admission.
4. What retention window does stale read pinning need, and who sets it?
5. Do followers read shared storage directly or proxy through the leader? Direct reads can expose bytes ahead of the `SyncCursor`.
6. Is a partially hydrated scope readable, or does the replica refuse reads until hydration completes?
7. Is lag in versions, wall time, or both? Policy forbids clocks but not observability.
8. How is the tail truncated without independent compaction: checkpoint against pages, or wait for a new snapshot?
9. Where does the pinned policy version come from when authority policy advances while the replica is offline?
