# 20. Cost model

A scope is a purchase, and the buyer must see the price before the purchase. oxkv makes the query the unit of ownership. A client adopts a policy-filtered slice of data and pays for that slice on every read, hydration, and resume. A scope inside one prefix reads one shard. A scope that crosses prefixes becomes a scatter-gather. The cost model states that difference before adoption, in numbers an application author can act on. Cost is a planning surface, not a diagnostic afterthought.

## What the greater goal demands

The authority owns truth and serializes writes. The replica is read-only and holds a scope, an index, and an outbox. Scatter-gather therefore runs on both paths, and neither path can hide an unbounded plan behind faster hardware. Offline-first makes the budget personal: a browser or a laptop materializes the scope and answers from local storage.

Three goal constraints shape the model. The policy language is deterministic and resource-bounded, so cost must derive from the same closed inputs. Shards are the unit of placement and fan-out, so shard count must be a first-class term. C1 (manifest schema), C4 (policy language), and A8 (cost and explain) freeze the declarative surface, so the explain shape and the named cost errors must stay stable while estimates improve.

A scope that crosses prefixes is not an error. It is a priced operation. The design must make that price visible at subscribe time, enforceable at query time, and reproducible across native and wasm.

## Required capabilities

**A cost unit.** The unit must combine machine count and data volume without exposing storage internals. Define one cost unit as one shard answering one query step, or one thousand examined keys. In fixed point, `cu_milli = 1000 × shards + keys_examined`. An author reasons about two integers: `shards`, the machines that must answer, and `keys`, the records the plan may touch. A single-shard point lookup is 1 cu. A three-shard search that examines 400 candidate keys is 3.4 cu. Byte counts, cache lines, and page faults stay inside the engine.

**An explain output.** Every scope and query can produce an explain record. The record states the `manifest_digest`, `policy_version`, `map_version`, and `statistics_version`. It also states the normalized policy, chosen shard set, per-shard plan, merge shape, total shards, total keys, total cost, active budget, and verdict. Text serves humans. Canonical JSON serves tools and tests. Explain reads metadata only, never data pages, and produces no event.

**Bounded fan-out.** Every scope declares a shard ceiling. Every replica declares a local ceiling. Admission compares the planned shard set with both ceilings at subscribe time. It rejects the scope with `scope.over_limit` before any data moves. A single query may request a lower ceiling. No plan exceeds its ceiling at execution.

**A deterministic merge.** Results merge under one total order: `(declared sort key ascending, canonical key ascending)`. Arrival order, retry order, shard speed, and shard identifier cannot change output or page boundaries. A pagination cursor records one frontier per shard and the pinned scope version vector.

**A rule for exact and coverage results.** Exact mode is the default. A result is exact or it is a named failure. Coverage mode is opt-in. Every coverage result carries a coverage descriptor that lists answered shards, missing shards, and the scope version vector each one served. A caller must not infer completeness from a row count. The engine never returns a silent subset.

**Cost feeds the scope admission limit.** A scope declares `max_keys`. Compilation proves that the worst-case key bound fits the limit, or it rejects the scope at subscribe. `max_keys`, `max_bytes`, and `max_shards` are admission limits, not runtime truncation. The replica must not stop admitting keys at runtime. A scope that does not fit the declared device budget fails at subscribe with `scope.device_budget_exceeded`.

**Search cost is term cost.** A term probe counts once per shard. A boolean query with t terms over s shards costs at least s × t probes, and posting lists add keys. Explain shows both terms.

## Invariants that must hold

- Cost is a pure function of manifest, policy, shard map, and statistics versions. Equal inputs yield equal output on every target.
- Rule-based estimates never under-report examined shards or keys.
- Statistics refine a displayed estimate and never raise an enforced ceiling.
- The fan-out ceiling is checked before the first shard request, not after the last response.
- Merge order is total and independent of timing, retries, and shard availability.
- Partial coverage is explicit and versioned. No caller infers completeness from row count.
- Explain has no side effects and needs no write path.
- Every result carries the cost-input versions that produced its estimate, so stale advice is detectable.
- Pagination keeps accounted cost additive and monotonic across pages.

## Decisions and technical choices

**Two numbers and a scalar.** Shards drive latency, connection count, and cross-machine spend. Keys drive CPU and I/O. The scalar ranks plans and feeds one budget. The pair stays visible because one scalar cannot separate a cheap wide query from an expensive narrow one.

**Rule-based versus statistics-based.**

| Property | Rule-based | Statistics-based |
|---|---|---|
| Inputs | manifest bounds, shard map, policy shape | per-shard sketches, versioned |
| Shard set | exact | exact |
| Key bound | worst case | estimate with declared error factor |
| Determinism | total | tied to statistics version |
| Availability | always | needs fresh sketches |
| Role | authorization and safety | display and tuning |

The chosen model is hybrid with a strict order: rules authorize, statistics refine. Admission compares the rule-based worst case with the ceiling. The displayed estimate uses statistics only when the statistics version matches the manifest version. A missing or stale sketch falls back to the rule bound. A fully statistics-authorized model loses because one stale sketch could open fan-out, and two replicas could disagree about legality.

**Ask cost at subscribe, not after the first page.** A scope is a standing purchase. The author sees the explain record, the cardinality headroom, and the hydration cost before the replica starts.

**Statistics live with the manifest and share its lifecycle.** A `statistics_version` is an input to explain and to the result envelope. A rule change or index change invalidates statistics. Staleness is a declared bound, not an accident. Statistics refine cost estimates only. They must not affect ranking, because 1.0 ranking uses document-local features only.

**Freeze the shape in A8, not the numbers.** A8 (cost and explain) freezes the explain schema and the cost error names at 1.0. The weights and estimates remain observations that may improve inside 1.x. A client can parse cost without pinning a calibration.

## Dependencies

- A1 (key encoding) and prefix bounds (roadmap 5) define the shard set.
- C1 (manifest schema) (7) declares tables, indexes, limits, and statistics references.
- C4 (policy language) (8) supplies the deterministic predicate shape.
- A5 (scope semantics) (13) supplies the unit that carries a budget.
- C3 (sync wire protocol) (14) carries coverage descriptors and cost versions.
- Snapshot-consistent search (19) supplies the scope version vector that cost pins.
- A4 (shard map) (22) makes the shard set computable before execution.
- The business event stream (24) carries budget and cardinality signals. Metrics use the operational feed.
- The qualification suite (30) and falsifier benchmarks (31) measure the model.

## Acceptance criteria

1. Explain output is byte-identical on native and wasm for one input tuple.
2. A scope above the shard ceiling fails at subscribe with `scope.over_limit` and transfers no data.
3. A query above its runtime budget fails with `query.budget_exceeded` and a coverage descriptor.
4. Shuffled shard responses produce identical pages and `CostCursor` values under property test.
5. Rule-mode estimates are greater than or equal to observed keys for every case in the qualification matrix.
6. Statistics-mode estimates stay inside the declared error factor for the benchmark corpus.
7. No accepted scope exceeds its `max_keys` limit at any scope version vector.
8. Pagination cost is additive across pages and a `CostCursor` stays bounded in size.

## What would falsify this design

- Legitimate scopes hit the rule-based worst case so often that admission rejects ordinary work. Rule mode then fails as a gate, and the ordering of rules and statistics must change.
- Deterministic merge forces full materialization of every shard page. The bounded-memory claim then fails and merge needs a different frontier.
- Most clients request coverage results, so the explicit rule becomes friction and the default must invert.
- Observed work departs from the estimate beyond the declared factor across the benchmark matrix. The model then describes intent, not cost.
- Colocated shards make shard count a weak latency term. The 1000-to-1 weight then needs a per-shard weight class.

## Open questions

1. Does fan-out run at the authority query path, the replica coordinator, or both, and which side owns the hard ceiling?
2. Where do statistics live, and does their staleness bound equal the policy projection bound?
3. Should write validation share the cost unit with scope queries, or keep a separate write budget?
4. Does tiering add a per-shard weight for cold reads from object storage?
5. Is `max_keys` a hard admission limit, or a soft limit with an explicit overflow receipt?
