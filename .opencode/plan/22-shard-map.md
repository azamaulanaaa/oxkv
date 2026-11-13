# 22. Shard map, static

The shard map is the line between a library and a distributed database. A library answers questions about bytes. A distributed database must also answer, before any read or write, which authority owns the bytes and which authorities take part. For 1.0 the map is a static, versioned artifact. It routes every key to exactly one shard. It validates disjointness and coverage at load time. It compiles a cross-prefix scope into explicit scatter-gather. It rejects a cross-shard mutation with `shard.cross_mutation_rejected`.

## What the greater goal demands

A data shard contains one user table and its system sub-namespaces, and it is the unit of placement, ownership, transaction, fan-out, scope, and replication. Placement follows key encoding. Ownership follows placement. The write path never crosses a transaction boundary. Authority owns truth and serializes writes. A replica holds a scope, an index, and an outbox, and it never writes.

A scope is a policy-filtered prefix query, so one scope can cross shards. Reads must cross. Writes must not, because the transaction unit is the shard and 1.0 has no distributed commit.

The policy language is deterministic and bounded, and the cost model requires bounded fan-out. The map must therefore make the fan-out set a static fact computed before execution. An operator must rebuild placement from the manifest and the map alone, with no coordinator, gossip, or discovery.

## Required capabilities

1. **Routing.** A pure, total function from key bytes to one shard identifier. It returns the same answer in every process and on both targets for the life of a map version. It reads no clock, seed, environment variable, or local data.
2. **Configuration format.** A versioned document declaring `shard_id` values, table assignment, prefix bounds, and placement. It parses, validates, canonicalizes, and exposes a digest. An unknown `map_schema_version` fails with `map.unknown_version`. A readable text form and a canonical byte form both exist.
3. **Placement rule.** A declared user table name maps to one data shard. The rule states table granularity and the catalog shard assignment. Placement is a declaration, never an inference from data. A table must not split across shards.
4. **Validation.** No key belongs to two shards. Every key belongs to one shard or a declared reserved region. Overlap, gap, duplicate identifier, empty range, split table, and unknown version each fail at load time with a distinct named error.
5. **Range resolution.** For a key, table name, or scope, return the shard set in deterministic order and report whether it is single-shard.
6. **Explicit scatter-gather.** A cross-prefix scope compiles into a plan with one sub-query per shard, deterministic merge order, a declared shard bound, and defined exact and coverage behavior. The plan is inspectable and explainable. Implicit fan-out is forbidden. Exceeding the bound is `search.fanout_exceeded`.
7. **Named cross-shard mutation error.** A write resolving to two or more shards fails with `shard.cross_mutation_rejected` carrying the key, shards, and operation. The receipt is terminal `Rejected`. Failure precedes scheduling, so no shard sees a partial write and no outbox entry reaches `Committed`.
8. **Version agreement.** The `map_version` and `map_digest` travel with the manifest, ShardSnapshot, scope, and `SessionHello`. A mismatch is `sync.incompatible_manifest` or `map.unknown_version`. Silent placement divergence is forbidden. The map must not validate the manifest.

## Invariants that must hold

- **Determinism.** One `map_version` and one key produce one shard, on every target, across restarts.
- **Totality and disjointness.** Coverage is complete and non-overlapping for the declared keyspace.
- **Version immutability.** A placement change creates a `map_version`, and an old `map_version` stays interpretable for old ShardSnapshots.
- **Single-shard write.** No committed transaction spans two shards.
- **Bounded fan-out.** Every scope resolves to a finite shard set before execution.
- **Deterministic sync order.** Scope sync order is `(shard_id ascending, key ascending)`. It is a concatenation order, not a query order. The query order is `(declared sort key ascending, canonical key ascending)`. Shard identity is not a semantic tie-break.
- **Data independence.** Routing never reads stored data.
- **Single owner.** One shard `lease_term` has one lease holder.
- **Rebuildability.** Hand-authored and generated maps route identically after canonicalization.

## Decisions and technical choices

**Range, not hash.** A table range is a scan unit and a scope unit. Range routing preserves those units. A scan and its index stay local, and a prefix scope stays cheap. Hash routing scatters a table across all shards, turns every prefix scan into a fan-out, and destroys index locality. It buys even key distribution, which is not the primary goal. Predictability and locality are. A table stays whole. The benchmarks measure skew.

**Static, not dynamic.** A static map is reviewed, versioned, validated in continuous integration, and deployed atomically with the manifest. A dynamic map needs a coordinator, placement consensus, lease migration, and online rollout. That is a second control plane and a second consensus problem, and 1.0 rejects it. Static placement also lets a replica prove its shard coverage without asking anyone.

**Canonical bounds.** Bounds are byte-oriented, inclusive lower and exclusive upper. Adjacent ranges compose without gap or overlap. Empty ranges are illegal. Identifiers are opaque and stable. Merge order is separate from identity.

**Placement granularity.** A user table maps to one data shard. It must not span shards. A map with a split table fails with `map.table_split_unsupported`.

**Plan as a value.** The plan is a pure function of scope and map. It lists shards, sub-queries, merge order, and bound, and explain output prints it.

**Cross-shard mutation is rejected.** The error belongs to C5 (receipt and error semantics). Two-phase commit over Raft groups adds a second consensus problem and breaks the single-shard invariant.

**Generated RPC.** RPC generated from the manifest carries the map digest in metadata. Hand-written surfaces must not invent placement metadata.

**Placement control plane.** A control plane would add placement from observed load, automated splits, shard movement with copy and lease handoff, failure-domain awareness, and re-replication. It would also add online map rollout and map collection. Each item needs global state and coordination, and together they need consensus on placement. That doubles the failure surface before single-shard semantics are proven. 1.0 changes placement by reviewed redeploy. A later version can add the control plane without changing route semantics if map versions and digests are correct now.

## Dependencies

A1 (key encoding) gives table-name prefixes, canonical bounds, and ordering. C1 (manifest schema) supplies table names. C4 (policy language) and A5 (scope semantics) supply prefix sets. C5 (receipt and error semantics) supplies the terminal `Rejected` state. The write scheduler assumes per-shard independence. The cost model consumes the shard bound and merge order. The replication seam attaches ownership and fencing to map entries. `SessionHello` carries `map_version` and `map_digest`. Order is strict: encoding, manifest, map, scope fan-out, replication, RPC.

## Acceptance criteria

1. A property test over random keys and valid maps returns one shard per key, identically on native and wasm32 and across restarts.
2. The validator rejects overlap, gaps, duplicate identifiers, empty ranges, split tables, and unknown versions with distinct named errors.
3. Golden vectors fix map, keys, shard identifiers, and digest. They change only with a `map_schema_version`.
4. A scope over N prefixes yields at most N sub-queries, identical across targets and runs.
5. A two-shard write returns `shard.cross_mutation_rejected` with `Rejected`, and both shards stay unchanged with no committed outbox entry.
6. A cross-prefix search keeps `(shard_id ascending, key ascending)` sync order under injected latency, including slow and failed shards, while query pages keep `(sort key ascending, canonical key ascending)`.
7. A stale `map_digest` is refused at `SessionHello`.
8. Continuous integration validates total coverage of every declared table.
9. A leader kill mid-write yields `Committed` or `Rejected`, never partial, and the `map_version` stays stable.
10. A rebuild test reproduces hand-authored routing from the manifest.

## What would falsify this design

- Skew stays beyond capacity without a split, so range routing cannot meet the load budget.
- A table must split to meet the load budget, so the no-split rule fails operationally.
- Realistic scopes exceed the shard bound, so the one-table-per-shard premise is wrong.
- A business invariant spans tables, so rejecting cross-shard mutation blocks the workload.
- Map changes are too frequent for atomic deployment with the manifest.
- Routing diverges across targets, breaking one core and two targets.
- Merge order cannot survive partial failure and retry in the fan-out path.

## Open questions

1. Must a table stay whole? Yes. A split is not supported in 1.0.
2. Who sets the maximum shard count per scope?
3. Are identifiers meaningful or opaque with display labels?
4. Does the map carry storage-tier hints?
5. Are reserved system sub-namespaces shards or an excluded region?
6. Must a ShardSnapshot pin a `map_version`?
7. Does a replica receive the whole map or only its covered shards?
8. What is the offline migration story for moving a prefix between versions?
