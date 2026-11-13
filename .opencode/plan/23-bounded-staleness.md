# 23. Bounded-staleness reads

oxkv sells a query, not a connection. A client declares a scope, matching data materializes locally, and the client reads that copy offline. The copy trails authority by design. The read contract must state how far a read may lag. Bounded staleness turns that gap into a budget. A version pin makes an answer reproducible. A declared bound makes lag admissible. Per-replica monotonicity keeps answers sane. A measured unavailability budget keeps the promise honest. Linearizable reads are unnecessary here. Demanding them would cost offline reads, read availability during leader loss, and read scaling.

## What the greater goal demands

Five commitments decide the read contract.

- **The query is the unit of ownership.** A materialized scope is a copy, so a contract that requires authority truth contradicts the ownership model.
- **Authority serializes writes, not reads.** A read path that consults authority for every answer makes authority the read bottleneck and the single point of read failure.
- **A replica is read-only, with scope, index, and outbox.** It has no write engine, but it must answer three questions: which version an answer reflects, how stale it is, and whether it may serve.
- **Offline for a day must still read and search.** Offline means no authority contact, so a contract that needs authority contact to be correct fails that milestone.
- **The 1.0 contract covers wire-visible semantics.** C3 (sync wire protocol) and A11 (read pin and staleness) make the pin, the staleness error, and the session token wire-visible and must be designed before 1.0.

The failure mode is a silent stale read: reporting the scope version vector makes staleness a feature, hiding it makes it a bug.

## Required capabilities

1. **Version pin.** Every request names a pin and every response reports the scope version vector it reflects. Without a pin, a read resolves against the replica's current scope version vector.
2. **Two pin modes.** `AtLeast(V)` means "V or newer" and is the default. `Exactly(V)` means "exactly V" and serves pagination and audit replay, because one `SearchCursor` must not mix versions.
3. **Declared bound.** A read declares `max_staleness` before it runs. The scope supplies a default. A read may narrow it, widen it only by explicit override, or declare it unbounded for offline use, and the response then reports data age.
4. **Admission.** The replica compares its freshness to the bound. If the bound holds it serves, otherwise it returns `read.stale` and no data. Over-bound data is served only on explicit permission, and the response says so.
5. **Monotonic session.** A session token carries a version vector minimum, and each replica holds a persisted high-water mark. Served scope version vectors never decrease across replicas, reconnects, or restart.
6. **Read-your-writes.** A `Committed` receipt carries the write's `commit_version`. The client raises the session minimum to it, so later reads observe the write. A replica that cannot apply the session minimum must block until it applies or return `read.behind_commit`.
7. **Leader independence and ceiling.** A read that satisfies its bound completes without leader contact. A replica serves only at or below its local commit index, never applied but uncommitted data.
8. **Measured SLO.** Every read records requested bound, served staleness, and outcome. Availability is a distribution, not a claim.
9. **Exact and coverage results.** Exact mode is the default and requires every targeted shard to serve the pin. Coverage mode is opt-in and carries a coverage descriptor. A caller must not infer completeness from a row count.

## Invariants that must hold

- **Total pin and ceiling.** No response omits its scope version vector, and no served `commit_version` exceeds the local commit index. A response without a pin is a protocol error.
- **Monotonicity.** For a fixed session, served scope version vectors never decrease. A lower pin is raised to the session minimum or rejected, never served below it.
- **Bound honesty.** A success reports staleness at or below the bound. A refusal names the bound and the measured staleness.
- **Clock isolation.** No wall clock enters policy evaluation. The bound is checked in routing and admission, so pinned inputs give identical verdicts at any time.
- **No single version for all shards.** A cross-shard read pins each shard separately and merges deterministically from the scope version vector. It never claims one version for all shards.
- **Leader loss changes only when the bound expires**, never the safety of an earlier answer.
- **Search and read agree** at one pin.
- **Two bounds.** Authorization fact version distance and data staleness are separate budgets. A read must satisfy both.

## Decisions and technical choices

**Why not linearizable reads.** Linearizable reads need leader contact per batch or a valid lease. They tie read latency to the leader round trip, throughput to the leader, and availability to leader liveness. The client accepts sync delay, works offline, and reads a materialized scope. A stronger tier forces it to know which tier it uses, which leaks topology. The cost is offline reads, leader-loss availability, and read scaling.

**Default: follower read with a declared bound.** The replica answers from its local commit index with no leader contact. Leader loss degrades freshness until the bound expires, not availability. Only this meets the offline milestone.

**Optional tightening: read index.** A read index asks the leader for the commit index, waits until the replica applies to it, then serves locally. It is stronger but costs one round trip per batch and depends on leader reachability. A `max_lag = 0` read is exactly this, offered outside the 1.0 promise.

**Rejected for reads: authority lease.** An authority lease lets the leader serve reads locally while it holds. It helps only the leader, re-centralizes reads, needs bounded clock skew, and fails on leader loss until renewal. The authority lease stays the fencing primitive for placement, not the read path.

| Strategy | Leader contact | Survives leader loss | Which node serves | Cost |
|---|---|---|---|---|
| Authority lease | None while held | No, until renewal | Leader only | Clock skew, centralization |
| Read index | One round trip per batch | No, needs a reachable leader | Any caught-up replica | Latency, leader dependency |
| Follower read | None | Yes, until the bound expires | Any replica with a valid pin | Staleness must be bounded and measured |

**How the bound is measured.** The contract bound is the fact version distance and the data version distance. Time lag in seconds is monitoring data only. Lag captures missed updates. Silence captures a replica that hears nothing from authority. Zero lag after a long partition is not fresh. Admission fails when either the fact version distance or the declared time budget exceeds its limit.

**What A11 must pin.** The pin modes, the scope version vector, the named staleness error with its value, the session token, and the cross-shard scope version vector.

## Dependencies

- C2 (snapshot and page format), because a pinned read reconstructs a page at a `commit_version`.
- A5 (scope semantics) and C3 (sync wire protocol), because the scope supplies the bound and filter, and the pin, session token, and scope version vector travel on the wire.
- Client replica monotonic reads, because the session minimum is enforced there first.
- Policy projection, because A6 defines authorization staleness, the second bound.
- Snapshot-consistent search, because search and get must share a pin.
- Replication seam, because the commit index is the ceiling and each driver must expose a lower bound on it.
- Shard map, because cross-shard reads pin per shard and fan out deterministically.
- Receipts, because read-your-writes needs a terminal receipt's commit version.

## Acceptance criteria

- A read at `AtLeast(V)` returns a scope version vector at or above V, or `read.stale`.
- A read that cannot meet its bound returns `read.stale` with the measured staleness and no data.
- A replica restart never regresses its high-water mark. A read below it is raised or refused.
- Killing the leader leaves bounded reads available for the full bound, and the suite reports an outage window at or below it.
- A 24-hour offline session reads and searches its scope. Every response reports data age.
- Search and get at one pin return one scope version vector.
- Policy evaluation is time-invariant: identical pinned inputs give identical verdicts under a shifted clock.
- The SLO report gives availability, p99 staleness, over-bound rate, and leader-kill recovery time.

## What would falsify this design

- Bounded follower reads show no latency or availability gain over read-index reads, so the machinery is unjustified.
- The leader-kill drill shows an outage longer than the declared bound, so the bound is false.
- A real application needs linearizable reads for correctness, so the thesis is wrong for that application.
- The over-bound rate exceeds its budget in steady state, so replication lag is the defect.
- A cross-shard merge depends on arrival order rather than the scope version vector, so determinism is broken.
- No client requests `Exactly(V)`, so the pin modes are overbuilt.

## Open questions

1. Does the bound live per read, per scope, or both, with the read narrowing the scope default?
2. Is the session token carried by the client, by the transport, or by the receipt outbox identity?
3. What is the unavailability budget, and over what window?
4. Should a zero-lag tightening ship at 1.0, or stay outside the contract?
5. For a cross-shard read, does the response report the minimum shard version, or the full version vector?
6. How does a replica prove commit-index liveness under the fencing driver with no quorum acknowledgement?
