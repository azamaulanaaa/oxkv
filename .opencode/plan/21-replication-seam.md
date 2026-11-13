# 21. Replication seam

A shard has one owner at a time. Availability is a product requirement, so ownership can change. One driver interface governs that change for a time-bounded authority lease and a replicated consensus group. A `lease_term` is the only fence that crosses it. A shard with two live ownership mechanisms is a defect, not a feature.

## What the greater goal demands

Authority owns truth and serializes writes. A replica never writes. The receipt is the client's proof of an outcome, and C5 (receipt and error semantics) freezes at 1.0. A leader change therefore decides whether an in-flight write becomes `Committed` or `Rejected`. It must never produce both.

A seam needs two in-tree implementations, and its protocol core must be sans-IO. One state machine then runs under a test harness, a native server, and a simulated network. Readable form comes before fast form.

Availability must be measurable. Wave 4 closes on an unavailability budget that the seam wins or loses.

## Required capabilities

1. **One driver interface.** It exposes acquire, renew, and release of leadership, plus append, commit index, read at index, member set, and transfer. Callers see no other leader concept.
2. **One fence.** A `lease_term` is the only value that fences a write. It increases monotonically, it never repeats, and it travels with every log record. The storage boundary compares the write `lease_term` against the owner `lease_term`. A higher `lease_term` wins. A lower `lease_term` gets `replication.stale_epoch`.
3. **A log on low-latency durable storage.** A record counts as appended only after its bytes are durable. The latency budget for that step is explicit and measured. The log does not live on an object store.
4. **A defined read path.** The design names one default read mode and states its bound. A leader serves a read after it confirms its authority lease or commit index. A follower serves a bounded-staleness read only with an explicit scope version vector pin.
5. **Membership change rules.** Change one member at a time. A configuration change is a log entry. Two changes never run at once. A new member catches up before it counts toward a quorum.
6. **A forced leader-kill test.** CI kills the leader mid-write, under load. The test asserts one terminal receipt per write. It is not a manual drill.
7. **A driver conformance suite.** Both implementations pass the same suite with a simulated clock and network.
8. **Sealed ScopeSnapshots across failover.** A `content_seal` covers the `content_digest` and the `lease_term`. The `lease_term` must not enter the `content_digest`. A sealed ScopeSnapshot remains valid across a leader change, and a new leader may serve the same sealed bytes.

## Invariants that must hold

1. **Single owner.** At most one leader holds a valid `lease_term` for a shard at any moment.
2. **Monotonic `lease_term`.** A `lease_term` increases and never returns to an earlier value.
3. **Durable commit.** A `Committed` write survives the loss of any minority.
4. **One terminal receipt.** A receipt reaches exactly one terminal state. A committed write never becomes rejected, and a rejected write never becomes committed.
5. **Bounded staleness.** A read never returns state older than the bound for its mode.
6. **No driver bytes on the wire.** C3 (sync wire protocol), C1 (manifest schema), and C2 (snapshot and page format) carry no leader field, so a client cannot tell which driver serves a shard.
7. **Ordered events across failover.** The business event stream keeps per-key order through a leader change.
8. **One mechanism per shard.** A shard never runs an authority lease and a consensus group as independent authorities.

## Decisions and technical choices

**One seam, two implementations.** The first is primary-backup with an authority lease and `lease_term` fencing. The second is Raft. Both implement the same interface, and the pair proves it hides no assumption from one model.

**Compare the three models.**

- *Primary-backup with an authority lease.* One node holds a time-bounded right to write. A backup accepts a new primary only after the authority lease expires. Few round trips give low latency, but the model needs a bounded clock assumption and a clock source. During a partition it trades availability for safety: the old primary must stop before the new one starts.
- *Raft.* A leader wins an election with a majority, appends to a log, and commits at a majority. The model defines election, log matching, and membership change. It defines read paths, such as an authority lease read or a read after a confirmed commit index. It costs one quorum round trip.
- *Paxos.* The family has the strongest published safety argument and no required distinguished leader. Multi-round agreement is harder to implement and to test than Raft. The seam admits a Paxos driver. The design does not require one for 1.0.

Raft is the reference driver and the authority lease is the minimal driver. The authority lease serves small deployments where a quorum adds latency without adding availability. It also gives the suite a cheap second implementation.

**Two ownership mechanisms are a defect factory.** Wave 1 gives tiered storage an authority lease with `lease_term` fencing. Wave 4 adds consensus leadership. If both stay active for one shard, a write must pass two fences. It can pass one and fail the other. A receipt then has two possible truths, and the audit trail records both. The test matrix grows with every interleaving of two clocks, two expiry rules, and two election paths. No suite covers it. The repair is composition, not addition. The authority lease becomes a degenerate case of the same interface. The driver reports one `lease_term`, and the storage fence compares one value.

**An object store cannot be the consensus log.** An object store offers put, get, and list. Put is not a linearizable append to a shared log. A writer cannot always tell a failed put from a lost response, so every retry is ambiguous. Conditional put needs a read and a compare, so each append adds a round trip. Latency is tens of milliseconds with a long tail that sets commit latency. List results can lag, so a recovering node cannot trust a listing as the log. The store has no fsync contract that orders an acknowledgement. Use the object store for snapshots, segments, and cold tiers. Never use it for the log that decides leadership.

**Readable before fast.** A log record has a plain header with an explicit `lease_term`, index, and length. An operator can read it without a decoder. A later version can add a compact encoding.

## Dependencies

- **Tiering and the authority lease (4).** The `lease_term` fence and the durable path come from here.
- **Versioned writes, receipts, and scheduler (9–11).** The seam preserves their terminal states, and the scheduler must tolerate a retry after failover.
- **Scope and sync wire protocol (13–14).** Resume after disconnect must work across a leader change.
- **Business event stream (24).** Per-key order must survive a failover. Audit export and metrics use the operational feed.
- **Shard map and manifest (22, 7).** Placement names the shard, the driver names its owner, and generated RPC stays driver-agnostic.
- **Bounded-staleness reads (23).** This essay defines the read modes that item 23 bounds.

The driver interface and write semantics come before any consensus work.

## Acceptance criteria

1. The leader-kill test runs in CI. Every write reaches exactly one terminal receipt.
2. Repeated kills lose no committed write and duplicate no commit for one idempotency key.
3. Both drivers pass one conformance suite with a simulated clock and network.
4. A stale leader presents an old `lease_term` and receives `replication.stale_epoch`. No write reaches storage.
5. A read never returns a scope version vector older than its declared bound.
6. A membership change completes under load, one member at a time, with no split brain.
7. Log append latency meets the stated budget at p99.
8. Wire bytes are identical under both drivers, and the business event stream keeps per-key order across the kill test.
9. A sealed ScopeSnapshot remains readable after a leader change.

## What would falsify this design

- The seam is wrong if one interface cannot express both models without leaking leader logic to callers.
- The authority lease model is insufficient if the kill test cannot reach a deterministic outcome without a wall-clock sleep.
- The log placement is wrong if fsync p99 exceeds the availability budget.
- The interface is too thin if a driver swap changes any receipt or error.
- The defect claim is weakened if a test shows two ownership mechanisms that never conflict under any partition. Such a test would need to enumerate all partitions.
- The read path is wrong if a follower read returns a superseded version.
- The premise fails if an object store can meet the append latency and linearizability requirements. Measure before you accept it.

## Open questions

1. What authority lease duration and clock-drift bound apply, and which component supplies time?
2. Does the default leader read use an authority lease or a confirmed commit index?
3. Should the authority lease driver be a single-voter consensus group, so both drivers share one code path?
4. Does the log share bytes with the write-ahead record?
5. Who serves a ScopeSnapshot during a leader change: the leader, or any caught-up follower?
6. Does the `lease_term` appear in the client-visible receipt, or only in internal records and audit?
7. Is the membership-change protocol part of C3 (sync wire protocol), or only the single-step rule?
8. Do two-node deployments get a witness member or the authority lease driver?
