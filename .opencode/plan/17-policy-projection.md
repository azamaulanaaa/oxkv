# 17. Policy projection

A rule that reads foreign state must not turn every write into a distributed transaction. oxkv keeps the transaction boundary at one shard. When a rule needs a fact that another shard owns, the fact must arrive before the write, not during it. Policy projection is that arrangement: a versioned, bounded-staleness copy of the facts a shard's rules read, held inside the shard that evaluates them. The shard then decides alone, inside one local transaction, with no remote call on the commit path.

## What the greater goal demands

Authority owns truth and serializes writes. A data shard contains one user table and its system sub-namespaces, and it is the unit of placement, ownership, transaction, and fan-out. Cross-shard mutation is rejected by default. A validation step that reads another shard while a write is in flight breaks all three rules. It makes the authorization shard a gate for every write.

The policy language is deterministic and bounded: no clock, no randomness, no I/O. Scope filters and write validation share one evaluator. A rule cannot fetch a foreign fact at evaluation time. The fact must already be present, carry a version, and sit within a stated bound.

A6 (projection staleness) makes that bound part of the contract. C1 (manifest schema), C4 (policy language), C3 (sync wire protocol), and C5 (receipt and error semantics) must express projection state. A later change is breaking.

## Required capabilities

1. **A declared fact set.** The manifest must name every fact a rule may read. Each declaration must give a fact identity, a canonical type, an owning shard, and a cap on facts per evaluation. An undeclared fact is a compile error.

2. **A fact authority and a fact log.** Exactly one shard must own each fact. A fact must change only through a write request with an idempotency key, a base version, and a receipt. The projection is derived state, never truth, and no client may write it.

3. **A versioned projection in every consuming shard.** A shard that evaluates a rule over foreign facts must hold a projection table. The table must map fact identity to canonical value and carry a `projection_epoch` plus a per-source watermark. It must live in the `0x04` projection system sub-namespace, hidden from scope and closed to client writes.

4. **A projection protocol.** The feed must support a snapshot, an ordered diff tail, a cursor, resume after disconnect, and fallback to a fresh snapshot. Apply must be idempotent and must reject version regression. The core must be sans-IO, with two in-tree transports behind one seam.

5. **A declared staleness bound.** Every rule must declare a maximum fact version distance per fact source. The bound must be a fact version distance, not a duration, because policy cannot read a clock. Operators may monitor time, but the shard enforces the fact version distance.

6. **An invalidation rule.** Invalidation must cover a schema change, a fact-set change, an ownership change, a revocation, a watermark regression, and a checksum failure. It must not follow every policy predicate change. It must bump the `projection_epoch` and fail closed until rebuild.

7. **Defined behavior for a lagging projection.** Validation must return `projection.lagging` when the bound is exceeded. The receipt must stay `Queued`, not `Rejected`, because staleness is not a policy verdict. The authority must enforce a queue deadline. At the deadline, the receipt must become `Rejected` with `write.queue_timeout`. A strict rule or table must instead return `authorization.stale_facts` and `Rejected`. Reads must expose lag and `projection_epoch`, and no verdict may rest on untrusted entries.

8. **An audit record bound to a projection version.** Every verdict must record the `policy_version`, `projection_epoch`, per-source watermarks, fact versions read, rule identity, and decision. The record must replay the verdict offline.

## Invariants that must hold

1. Rule evaluation must perform no I/O and no cross-shard call.
2. Every allow or deny verdict must name one `policy_version` and one `projection_epoch`.
3. A shard must never authorize a write from a projection older than the declared version distance.
4. Applying a projection must be idempotent, monotone, and a pure function of the fact log at a stated version. A `projection_epoch` must never move backward.
5. A projection must never accept a client write and must never become an authority.
6. Cross-shard mutation must remain a named error. Projection must not create a hidden write path.
7. A fact-set, schema, ownership, or revocation change must invalidate dependent projection entries. A `policy_version` change must not invalidate a projection that reads the same facts with the same schema.
8. An audit record must be append-only within its shard and must name the fact set digest.
9. Native and wasm must produce identical projection bytes and identical verdicts.

## Decisions and technical choices

Three designs can give a rule access to foreign state. They differ in where the cost lands.

| Property | Remote read on commit | Two-phase commit | Projection |
|---|---|---|---|
| Calls on commit | One per write | Two per write | None |
| Failure coupling | Another shard's leader | Coordinator and participants | Local only |
| Atomicity | None, the fact may change | Strong, at a cost | Bounded staleness |
| Availability | Lowest | Low | Highest |

A remote read makes the authorization shard gate every write. Each write waits on another shard's leader and availability, so a partition stops writes everywhere. The read is not a lock, so the fact can change before commit. The design pays for a distributed transaction and still lacks its guarantee.

Two-phase commit buys the guarantee. It adds a coordinator, durable prepare state, blocking on coordinator failure, and a second consensus problem. It contradicts the default that rejects cross-shard mutation. Reserve it for facts that need a linearizable read.

Projection is the default. The shard reads local bytes inside the write transaction. The price is a declared version distance, storage, and the invalidation machinery. That price is visible, bounded, and auditable. A hidden remote read is none of those.

The projection must stay small. The manifest must cap a rule's read set and a projection's fan-in. Use a human-readable encoding first and canonical binary second. Eviction must force the affected rule to fail closed, never read a gap.

Scope uses the same projection. A scope filter that reads foreign facts must evaluate against the replica's `projection_epoch`, which the replica advertises with its `SyncCursor`. A replica that cannot meet the bound must widen its scope request or report lag, never silently narrow its result. A canary cohort must use a `projection_epoch` that includes its `policy_version`.

## Dependencies

- C1 (manifest schema) must express fact declarations, read-set caps, and version-distance bounds.
- C4 (policy language) must expose projected facts and no other foreign access.
- Versioned writes and C5 (receipt and error semantics) must give every fact a version and a terminal outcome.
- Enforcement and audit must pin a `policy_version` per verdict and emit one record.
- A5 (scope semantics) must share fact identity with validation.
- C3 (sync wire protocol) must carry `projection_epoch` and watermark with the `SyncCursor`.
- Hydration and recovery must rebuild a projection from ScopeSnapshot plus tail atomically.
- A4 (shard map) must define fact ownership, and ownership change must be explicit.
- The business event stream must carry fact changes, ordered per key and replayable. Audit export and metrics use the operational feed.

## Acceptance criteria

1. A rule in shard A that reads a fact owned by shard B must produce a verdict with zero network calls. The test must fail on any outbound call during commit.
2. With lag one version beyond the declared bound, the write must return `projection.lagging` and the receipt must stay `Queued` until the queue deadline.
3. The same fact log and `policy_version` must produce byte-identical projection content on replay, on native and on wasm.
4. A stored audit record must reproduce its verdict against the fact log alone.
5. A schema, fact-set, ownership, or revocation change must invalidate dependent entries and force fail-closed behavior. A `policy_version` change that reads the same facts must not invalidate them.
6. A rebuild must reach the same watermark as an uninterrupted feed.
7. A client write addressed to the `0x04` projection sub-namespace must fail with `projection.client_write_rejected`.
8. Golden vectors must pin projection encoding, watermark comparison, and version-distance semantics.

## What would falsify this design

- If most writes depend on foreign facts, projection storage and fan-out may cost more than the coordination it removes.
- If real workloads need a zero staleness bound, projection cannot satisfy them.
- If fact churn outpaces dependent writes, fail-closed behavior becomes permanent unavailability.
- If a verdict cannot be reproduced from its audit record, the audit trail is decorative.
- If the only way to build a projection is a cross-shard read on commit, the design has not solved the problem.
- If operators cannot state a bound, a default bound hides a correctness hole.

## Open questions

1. Should the manifest express the bound in versions only, or also accept a time bound that the system translates?
2. Should a lagging scope read fail closed, or serve a coverage result with a lag marker?
3. May a projection depend on another projection? If yes, what depth limit bounds fan-in?
4. How should revocation interact with a large staleness bound?
5. Is a per-source watermark granular enough, or does each fact need its own version in the audit record?
6. Does 1.0 allow an opt-in two-phase commit exception, or defer it entirely?
