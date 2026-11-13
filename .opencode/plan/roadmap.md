# oxkv — 1.0 Roadmap

> Normative conflict resolutions and shared registries: see `specs.md` (revision 0.2).
> This roadmap follows the wave table and dependency rules in `specs.md` sections 7.24 and 10.1.

**Goal.** Ship 1.0 with the full shape: one user table per data shard, policy-governed
reads and writes, query-materialized replicas, receipts for writes, tiered
storage, Raft availability, search as first class, native and wasm parity.

**What 1.0 means.** 1.0 is a freeze of C1-C5 and A1-A12, not a feature count.

> 1.0 is the point where the declarative surface and the bytes stop moving.
> The Rust API may keep evolving inside 1.x.

The C1-C5 and A1-A12 register:

| ID | Surface | Version |
| --- | --- | --- |
| C1 | Manifest schema | `oxkv/manifest/v1` |
| C2 | Snapshot and page format | `snapshot-page/v1` |
| C3 | Sync wire protocol | `sync-wire/v1` |
| C4 | Policy language | `policy-ast/v1` |
| C5 | Receipt and error semantics | `receipt-error/v1` |
| A1 | Key encoding | `key-encoding/v1` |
| A2 | Tokenizer | `tokenizer/v1` |
| A3 | Event envelope | `event/v1` |
| A4 | Shard map | `oxkv/map/v1` |
| A5 | Scope semantics | `scope/v1` |
| A6 | Projection staleness | `projection/v1` |
| A7 | Audit envelope | `audit/v1` |
| A8 | Cost and explain | `cost/v1` |
| A9 | Cursor types | `cursor/v1` |
| A10 | Descriptor | `descriptor/v1` |
| A11 | Read pin and staleness | `read-pin/v1` |
| A12 | Migration and backup | `migration/v1` |

Version every row from the first commit, while breaking is still allowed. A
browser client holding a v0 ShardSnapshot does not care that the crate is 0.x.

---

## Work sequence

Use contract waves and implementation waves. Do not use a linear phase list as a
gate. A contract precedes every implementation that depends on it. A wave gate
must not require a later wave.

| Wave | Contract work | Implementation work | Gate |
| --- | --- | --- | --- |
| 0 | Registries, ADRs, and harness | Fault harness and vector skeleton | Registries approved and DAG checked in |
| 1 | 05, 06, 07, 08, 09, 13, 19 pin, 22, 14, 10, 24, 17, 29 auth/audit, and 25 layering | 01, 02, 03, and 04 with one storage backend | Cross-contract review and candidate vectors |
| 2 | 11, 12, and 27 contract | 09, 10 in-process, 11, 12, 17, 24 outbox, and 27 | Write terminal state, atomic event, audit record, and local projection |
| 3 | 15, 16, 18, 19, 20, and 23 | 13, 14, 15, 16, 18, 19, 20, and 23 | Offline scope read/search, resume, fallback, and cost explain |
| 4 | 21 and final 22 | 21, 22, and 23 SLO | Leader kill, stale epoch, and bounded staleness |
| 5 | 26, 27, and 28 contract | 24 consumer, 25, 26, and 27 | Email demo, control RPC, and canary rollback |
| 6 | 28, full 29, 30, and 31 | 28, 29, 30, and 31 | Backup/restore, migration, fuzz, and falsifiers |
| 7 | 32 | Final qualification | C1-C5 and A1-A12 register complete and support policy published |

### Wave 0 — Registries and proof harness

The qualification harness starts in wave 0. The falsifier harness starts in wave
0 and gates at wave 6. The contract register, the error registry, the reserved
keyspace, A9 (cursor types), the budget registry, the retention registry, and the
digest registry land before any implementation that depends on them.

### Wave 1 — Foundation and contract candidates

1. **Ordered KV store** — CRUD, range scan, `KeyCursor` pagination, and btree and
   LSM engines behind one interface. A data shard contains one user table and
   its system sub-namespaces under the `0xFF` system marker. *(exists)*
2. **One core, two targets** — native and wasm32 with identical semantics. No
   cfg-forked behaviour. *(exists)*
3. **Pre-write and post-write hooks** — pre-write is deterministic validation.
   Post-write is ordered notification. Both are pure, replayable, and have no I/O.
   *(exists, needs redesign)*
4. **Tierable storage with an authority lease** — RAM to disk to object storage per shard,
   single-writer authority lease with `lease_term` fencing. One storage backend
   ships in wave 1. *(partial)*
5. **Key encoding v1** — table name, not an arbitrary prefix. The data shard
   contains one user table plus its system sub-namespaces. Deterministic byte
   ordering and canonical bounds. A1 freezes key format and ordering.
6. **Snapshot and page format v1** — a self-describing header, `page_id`,
   `page_sequence`, provenance, checksums, and a named error for an unknown
   `snapshot_page_version`. C2 freezes the bytes. A ShardSnapshot is the
   authority and backup unit. A ScopeSnapshot is the replication unit.
7. **Manifest v1** — declares user table names, key schemas, fields, indexes,
   rule metadata, facts, operations, events, receipts, audit, projection, codecs,
   and control. Compiles to a neutral descriptor, storage layout, index config,
   scope, validators, and generated RPC. C1 freezes the manifest schema. Mutable
   rule bodies live in a separate content-addressed policy catalog.
8. **Policy language v1** — deterministic expression AST: paths, literals,
   comparison, boolean, membership, and existence. It uses a step budget. It has
   no clock, no randomness, and no I/O. C4 freezes the AST and evaluation
   semantics, not the source grammar.
9. **Versioned writes** — every write carries a `base_version`. A conflict is a
   named terminal outcome. C5 freezes the error taxonomy.
10. **Receipts and outbox** — durable client queue, idempotency key, states
    `Queued`, `Committed`, `Rejected`, and `Conflict`. C5 freezes receipt
    semantics. A client cancel is a local `CancelRequested` or `Abandoned`
    disposition, not a receipt.
11. **Write scheduler** — per-key FIFO, per-shard independence, explicit
    dependency edges, and a pure function with a step budget. It uses `queued`,
    `in_flight`, and `terminal` states.
12. **Enforcement and audit** — validate at commit against a pinned
    `policy_version`. The authorization bound is a fact version distance. One
    audit record per write request carries a bounded decision list. Read scope
    evaluation writes to a separate access log.
13. **Scope** — a policy-filtered query over user-table prefixes, and the unit of
    replication. A5 freezes scope semantics. Admission limits are `max_members`,
    `max_bytes`, and `max_shards`.
14. **Sync wire protocol v1** — custom sans-IO frames. `SessionHello` negotiates
    the descriptor, `sync_wire_version`, `manifest_digest`, `map_version`,
    capabilities, and limits. Subscribe to a scope, stream diffs with a
    `SyncCursor`, resume after disconnect, and fall back to a ScopeSnapshot.
    C3 freezes the frame format and resume semantics. Connect is the RPC envelope
    and carries sync frames as opaque bytes.
15. **Client replica** — scope-projected pages plus a diff tail, monotonic reads,
    exposed lag and scope version vector, offline reads, a replica-built index,
    and no write engine.
16. **Hydration and recovery** — stream ScopeSnapshot pages, verify checksums,
    hand off to the diff tail atomically, and include the hydration peak in the
    device budget.
17. **Policy projection** — replicate the rule facts a shard needs into that
    shard, versioned, so rule-checked writes stay single-shard. A6 freezes the
    fact version distance semantics. Invalidation follows fact-set, schema,
    ownership, or revocation changes, not every policy predicate change.
18. **Inverted index** — per shard, maintained inside the write transaction, with
    a versioned tokenizer. A2 freezes tokenizer behaviour. A replica builds its
    own index from scope members.
19. **Snapshot-consistent search** — search and get answer at one pin.
    Pagination is stable across pages. Scope-aware filtering runs before top-k.
    Exact mode is the default, and coverage mode is opt-in. The search comparator
    is `(score descending, canonical key ascending)`.
20. **Cost model** — explain output, bounded cross-shard fan-out, and a
    deterministic scatter-gather merge. The general query comparator is
    `(declared sort key ascending, canonical key ascending)`. Shard identity is
    not a semantic tie-break. A8 freezes the explain schema and cost error names.
21. **Replication seam** — fencing or Raft behind one driver interface, with a
    forced leader-kill test in CI. A sealed ScopeSnapshot remains valid across a
    `lease_term` change.
22. **Shard map, static** — deterministic key-to-shard routing. A table maps to
    one data shard and does not split. Cross-prefix scope is explicit
    scatter-gather. A cross-shard mutation returns
    `shard.cross_mutation_rejected`. A4 freezes the map configuration format.
    The map does not validate the manifest.
23. **Bounded-staleness reads** — follower and replica reads with a scope version
    vector pin, `AtLeast(V)` and `Exactly(V)` modes, SLO instrumentation, and a
    measured unavailability budget. A11 freezes the pin modes and staleness
    errors.
24. **Business event stream** — ordered per key, at-least-once, replayable from an
    `EventCursor`. The business event stream is the only path to write side
    effects. A separate operational feed carries audit export, metrics, traces,
    budget signals, lifecycle notices, and alerts. A3 freezes the envelope fields.
    Acknowledgement is per key.
25. **Generated RPC** — Connect for native, browser, and in-process callers, all
    from one protocol core. Connect, not gRPC. Connect is the RPC envelope and
    carries sync frames as opaque bytes. A10 freezes the descriptor. Control
    operations use the `control` namespace.
26. **Reference effect consumer** — per-key ack and nack, dead-letter, idempotent
    handling, and a self-contained payload. The handler must not read the
    authority.
27. **Rule lifecycle** — dry-run, staged rollout, diff, and rollback. A rule
    change is a write with a receipt and an audit record. The policy catalog is
    content-addressed. A canary cohort uses a `projection_epoch` that includes its
    `policy_version`.
28. **Operations** — backup and restore, format migration, compaction control,
    metrics and traces, per-shard quotas, and rate limits. Backup is a set of
    ShardSnapshots with a version vector. Restore preserves terminal receipts and
    event cursors, or marks them expired. One migration framework covers artifact,
    shard format, and key encoding migration. A12 freezes the backup manifest and
    migration steps.
29. **Security hardening** — step budgets, policy fuzzing, client authentication
    distinct from rules, and audit export. Every write verdict enters the
    per-shard audit chain. Read scope evaluation uses a separate access log. The
    external witness runs asynchronously. The 1.0 search language has no regular
    expressions.
30. **Qualification suite** — scenario matrix native to browser to native:
    disconnect, conflict, rule change, ScopeSnapshot restore, and format upgrade.
    Golden vectors for every core contract C1-C5 and every annex A1-A12. The suite
    starts in wave 0 and grows with every wave.
31. **Falsifier benchmarks** — sync growth against scope size, policy cost per
    write, conflict rate under realistic concurrency, cold start, and read p99.
    The scope byte measurement compares live bytes and hydration peak against the
    declared device budget.
32. **1.0** — freeze C1-C5 and A1-A12, publish the deprecation and migration
    policy, and tag.

---

## Dependency rules

- A contract node must be declared before any implementation that depends on it.
- An implementation may use a fake behind a declared contract.
- A wave gate must not require a test from a later wave.
- A wave must not close on a test that needs a later wave.
- A contract candidate may break before wave 7.
- A C1-C5 or A1-A12 contract must not break inside 1.x.
- The error registry is a candidate until wave 7.
- The qualification harness starts in wave 0.
- The falsifier harness starts in wave 0 and gates at wave 6.
- The manifest compiler stages its emitters. Layout and indexes start in wave 1.
  Scope starts in wave 3. RPC starts in wave 5.
- Item 18 starts after the item 19 read-pin contract is declared.
- The Phase D milestone says "reads exactly its query." Search joins in Phase E.
  This is the only retained phase label, and it is descriptive, not a gate.

## Historical note

The earlier Phase A-I list is retired. It had cycles: 07 needed 22, 22 needed 07,
28 needed 29 and 32, 32 needed 28, 27 needed 30, and 30 needed 29. The wave table
above replaces it. A reader who finds an old Phase A-I reference must treat it as
historical and non-normative.

## Gates

Each wave closes only when its acceptance test runs in the qualification suite,
not when the code merges. The suite is the regression net, the ABI proof, and the
customer demo at the same time. A wave gate must not require a later wave.

## Open decisions

1. **Cross-shard mutation** — reject with `shard.cross_mutation_rejected`
   (recommended) or 2PC over Raft groups (a second consensus problem).
2. **Shard map** — static configuration (recommended for 1.0) or a placement
   control plane.
3. **RPC surface** — generated from the manifest (recommended) or hand-written.
