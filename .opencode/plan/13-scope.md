# 13. Scope

A scope is the materialized result of a policy-filtered query over one or more prefixes. It is the answer to a declared question, not a keyspace copy and not a cache. It carries an identity, a version, and a diff stream, and those three make ownership checkable. A replica holding a scope can name its question, its answer version, and what entered or left. One holding "some data" can name none.

## What the greater goal demands

The thesis is that a query is the unit of ownership. Ownership needs a name, a boundary, and a version, or it cannot be enforced or audited.

Scope is also the read half of the write contract. A client never writes. A write carries an idempotency key and a base version taken from the client's scope. Without scope identity the authority can call a base version stale but cannot name the question it is stale against, so `Conflict` is an opinion.

One policy language governs read scope and write validation. C4 (policy language) freezes the AST, while scope semantics must also be pinned, or the words freeze and the sentence stays open.

The read milestone promises that after a day offline the client reads and searches exactly its query. Search makes that harder: a wide index leaks excluded records, a narrow one returns a false negative that looks like absence.

## Required capabilities

1. **Declaration.** A scope must be declarable as data: prefixes, predicate in the one policy language, projection, ordering, in one canonical byte form.
2. **Identity.** A deterministic digest of the canonical declaration. Two clients asking the same question must compute one identity on any machine at any time.
3. **Scope version.** The scope version vector names the exact answer held. A single-prefix scope may use a vector with one entry. A cross-prefix scope must use a per-shard vector, since independent shards cannot share a scalar order.
4. **Add, remove, and churn.** Both directions must be first class. A remove carries the key, its `key_version`, and a reason: the record changed, the policy changed, or the scope narrowed. A record that stops matching must leave, even with unchanged key and bytes. The system must report per-version churn.
5. **Cardinality limits.** A scope must declare `max_members`, `max_bytes`, and `max_shards`. An over-limit declaration must fail at subscribe with `scope.over_limit`, reporting the limit and the observed value. Truncation is not a fallback. A scope that does not fit the declared device budget must fail at subscribe with `scope.device_budget_exceeded`.
6. **Explicit scatter-gather.** A cross-prefix scope must enumerate its shards, keep a `SyncCursor` per shard, and merge deterministically. An unreachable shard must yield a coverage result with a coverage descriptor and the scope version vector reached. Exact mode must return a named error and no data when one targeted shard cannot serve the pin.
7. **Policy binding.** A scope must record the rule set and the policy version behind its materialization. A policy change must open a new diff interval and must not reinterpret emitted rows.
8. **Offline answerability.** A replica must answer its query from local state and expose its vector and lag. It must not need the authority to distinguish "no such record" from "not yet replicated".
9. **Read-only enforcement.** No path may create a member except an authority diff.
10. **ScopeSnapshot hydration.** A replica hydrates a ScopeSnapshot built from ShardSnapshots. A ScopeSnapshot contains only scope user rows and needed projection facts. It must not contain receipt, event, audit, or lease system sub-namespaces.

## Invariants that must hold

- **Exactness.** At a complete coverage pin, membership equals the records in the declared user-table prefixes that satisfy the predicate under the pinned `policy_version`, within the limits. Equality, not approximation. A coverage result is explicit and never claims exactness.
- **Identity determinism.** Same declaration, same identity. Distinct predicates must not collide, and identity must not depend on order, host, or time.
- **Monotone version, non-monotone membership.** The scope version vector must strictly increase. Membership may shrink, and every shrink must be an explicit remove.
- **Batch atomicity.** A shard batch applies at one `commit_version`. A partial batch is never visible.
- **Vector consistency.** One read, one scope version vector. No mixing of shard versions.
- **Removal completeness.** No member leaves without a remove, and none re-enters without an add, even with identical bytes.
- **Limit honesty.** A scope never exceeds a declared admission limit. A breach stops it at subscribe with `scope.over_limit`.
- **Residency.** Eviction may remove out-of-scope pages and superseded pages. It must not remove an in-scope logical member. Two replicas at the same scope version hold the same logical membership, but they may have different residency.
- **Policy isolation.** A policy change must not change how an emitted diff is read.
- **Authority exclusivity.** Only authority diffs produce membership.

## Decisions and technical choices

A scope is a view, not a cache. A cache may be wrong, partial, and evictable. A view is total and exact at a version. Lose that and four things break. A missing row looks like an unreplicated row, so a false negative passes as an answer. Eviction hides, so the offline promise lasts until the device needs space. Removals vanish, since a cache need not remember what left. Search and get disagree, because the index no longer provably covers the scope. A client that treats its scope as a cache repairs through the authority, putting the network back on the correctness path.

**Identity: bind the `policy_version`, or not.** Hashing the query with the `policy_version` makes an identity name one immutable answer, but every rule edit then creates a new scope, forces full re-hydration, and discards `SyncCursor` values. Recommended: hash the canonical query, the rule-set selector, and the projection. The selector names a rule set and the scope version vector carries the `policy_version`, so one identity may denote different member sets over time. A consumer that needs immutability pins (identity, `policy_version`). The wire and ScopeSnapshot headers must carry both.

**Membership: authority-computed members, mirrored prefixes, or client filtering.** Recommended: store members only, with an index over them. Storage stays proportional to the scope, and the policy boundary holds because a replica never receives an undeclared record. The price is that the authority must emit removals, since a client cannot recompute them, which makes the tombstone horizon a contract. Mirroring prefixes and filtering at read time makes removals free, but the replica then holds records outside its policy and the filter runs in two places. Client-side evaluation adds an evaluator-identity dependency across native and WASM. Both are rejected.

**Version shape: one scope version vector or a per-shard vector.** A scalar is easy to compare, but across independent shards it must either serialize every batch through one order, dragging consensus into replication, or assert a consistency it cannot prove. Recommended: a vector with a single-entry form for one-prefix scopes. This constrains the diff envelope, the ScopeSnapshot claim, and the `base_version` rule.

## Dependencies

- Manifest v1 (7) must express prefixes, fields, rules, and projections before a scope can be declared.
- C4 (policy language) (8) must freeze first: one evaluator serves both scope filtering and write validation.
- A1 (key encoding) (5) and A4 (shard map) (22) must fix prefix bounds, because fan-out is exactly the shards the prefixes cover.
- C2 (snapshot and page format) (6) and C3 (sync wire protocol) (14) must carry identity, `policy_version`, scope version vector, diffs, and a `SyncCursor`.
- Receipts (10) must bind a base version to an observed scope version, or `Conflict` has no referent.
- Search (19) and the cost model (20) must apply the scope filter and bound fan-out by declared shard count.

## Acceptance criteria

1. Two clients declare the same query on different machines. Their identities are byte-identical.
2. A record stops matching. The replica receives one remove. An offline query then returns absent at a known version, distinguishable from "not replicated".
3. A scope crosses three prefixes. The client names three shards, a read returns one scope version vector, and an unreachable shard yields a coverage result with a coverage descriptor.
4. A declaration exceeds the member limit. Subscribe fails with `scope.over_limit` reporting the limit and count. No member is truncated.
5. A policy version advances. The replica receives a new interval, and older diffs still parse under the old `policy_version`.
6. A replica disconnects for a day and resumes without a full resync. The diff count scales with churn, not scope size.
7. A read at scope version vector `V` returns only records that were members at `V`.
8. Search returns no record outside the scope, and a search page and a get at one scope version vector agree.
9. Eviction of an out-of-scope page does not change membership.
10. The qualification suite (30) runs these cases on native and WASM with golden vectors.

## What would falsify this design

- If sync cost grows with scope size at low churn, the stream is not a diff and the scope is a cache with extra steps.
- If two honest declarations of one query yield different member sets at one version, identity is false.
- If a client can see a record outside its scope, scope is not a policy boundary.
- If compaction makes a removal unrepresentable, exactness fails, and a short tombstone horizon forces a resync.
- If a cross-prefix version needs a global serialization point, scope has smuggled consensus into replication.
- If an empty scope looks like an unreplicated one, "no answer" and "no data yet" are the same.

## Open questions

- Does identity include the projection? Same rows with different fields may be one question or two.
- Is the tombstone horizon a 1.0 contract or an operational setting?
- May scopes compose in 1.x? Composition is the obvious pressure, and 1.0 must not preclude it.
- Must a write's base version be the per-shard entry of the observed vector?
