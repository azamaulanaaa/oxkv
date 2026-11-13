# 11. Write scheduler

The write scheduler invites cleverness more than any other component. That is why it must stay the least clever. A scheduler that owns threads, watches timers, or guesses dependencies becomes a second authority with a private clock. This design fixes it as one pure decision function. It reads a snapshot of pending work, returns the next batch to offer for commit, and writes nothing. It fixes one ordering promise: per key, first in, first out. Shard progress, bounded work, starvation freedom, and replay follow from that promise plus a declared edge. The hard part is refusing features that make it unreadable.

## What the greater goal demands

Authority owns truth and serializes writes. The scheduler may choose an order, but it never commits, rejects, or resolves a conflict. It proposes which queued request the commit path attempts next. The authority alone sets a terminal receipt.

A shard is the unit of placement, ownership, transaction, fan-out, and scope. Progress is per shard. One unavailable shard must not stop another. One slow key must not stop unrelated keys. Rejecting cross-shard mutation enables this. A cross-shard write would need a global order, a second consensus problem.

The receipt contract requires a client to observe a write to a terminal state and replay it. Equal state and budget must produce an equal decision. A decision that depends on clock time, randomness, or hash order cannot be replayed.

The 1.0 ordering contract is narrow: the ordering guarantee is per key only. A stronger claim would become a contract the system defends forever. The pure core is the sans-IO seam, identical on native and wasm, testable without a runtime.

## Required capabilities

1. **Per-key FIFO.** On each key, requests follow arrival sequence. A request may not be selected while an earlier non-terminal request waits. A request has one of three states: `queued`, `in_flight`, or `terminal`.
2. **Independent progress per shard.** Selection for one shard depends only on its queue and edges.
3. **Head-of-line bypass on disjoint keys.** A blocked key must not delay a ready request on a different key in the same shard.
4. **Explicit dependency edges.** A request may depend on an earlier request. Only the application declares the edge. The scheduler validates it at admission and never infers one.
5. **A step budget.** Each call inspects at most `B` positions and `D` edges per request. The budget is a count, not a deadline. Exhaustion returns a partial decision with `more_work`.
6. **Starvation freedom.** A request that stays ready is selected within a stated, tested bound.
7. **Deterministic replay.** The decision is pure over state and budget. Tie-breaks use stable identifiers. The core has no clock, randomness, or I/O.
8. **Explain output.** Every unselected request returns one reason: blocked on a key, on a dependency, outside the budget, or not submitted.

## Invariants that must hold

- **I1.** At most one request per key is `in_flight` at a time. Multiple requests per key may be `queued`.
- **I2.** On one key, commit order equals arrival order after terminal removals.
- **I3.** An edge adds a wait. It never moves a request ahead of an earlier request on its key.
- **I4.** Shard selection does not depend on another shard's queue.
- **I5.** One call examines at most `B` positions and `D` edges.
- **I6.** Equal state and budget produce an equal batch and equal reasons.
- **I7.** Every ready request is selected within the starvation bound.
- **I8.** The scheduler changes no receipt state. The commit path owns terminal transitions.
- **I9.** The scheduler evaluates no policy. The commit path validates against a pinned version.

## Decisions and technical choices

**Per-key FIFO with a ready set.** Each shard holds an arrival-ordered queue and a derived ready set. A request enters it when dependencies resolve and its key is free. A blocked key costs one queue entry and no scan work. This handles slow name changes without a graph.

**Shard partitioning.** It walks shards in stable order and reserves one slot per non-empty shard. Shards with more ready work receive the rest by a deterministic rule. An unavailable shard consumes no slot. Its neighbours continue.

**Explicit edges only.** An edge names an earlier request in the same shard. Admission rejects cycles, forward references, cross-shard edges, and fan-in overflow with named errors. The edge travels in the outbox and receipt.

**No inferred dependencies.** Inference requires reading values, comparing versions, or observing reads. That makes a pure function stateful. A wrong inference is worse than none. It moves a write before its prerequisite, a silent correctness bug, or serializes unrelated writes, an invisible throughput bug. Only the application knows intent. An explicit edge is reviewable and testable. A guess is not.

**Step budget.** The budget stops a backlog from turning one decision into a full scan. A partial batch is safe because per-key order holds. The caller repeats while `more_work` holds.

**Starvation freedom.** With reserved slots and a persisted epoch, a ready request waits at most `ceil(total_ready / B)` calls. Blocked requests are absent from the ready set, so they cannot skip a ready one.

**Deterministic replay.** State uses ordered structures, never hash maps with unspecified iteration. The log records state digest, budget, batch, and reasons. Replay compares all four.

**The slow-name-change case.** A rename waits on an approval write to the same record. Both touch one key, so per-key FIFO gives the required order. Another record in the same table touches a different key, stays ready, and commits. A graph would add nothing and would let one slow path delay unrelated work. If a slow commit causes the delay, a graph cannot remove it. Partitioning confines it to one key.

**Comparison.** Three designs were considered.

| Design | Strength | Cost |
|---|---|---|
| Per-key FIFO with ready set | Predictable, replayable, local | No cross-key reordering without an edge |
| Dependency DAG | Parallelism across keys | Cycle checks, closure, a second edge language |
| Priorities | Expresses urgency | Starvation, aging, mutable state |

FIFO wins on the properties the goal needs: a fixed per-key guarantee, bounded work, and replay. The DAG wins only on parallelism that rejected cross-shard mutation forbids. Priorities are an admission concern: a request that matters more should arrive earlier or carry an edge. Urgency belongs in outbox policy, not the ordering core.

## Dependencies

- **Key encoding v1** must define key identity. "Same key" is undefined without canonical keys.
- **Versioned writes** must define base version and conflict outcome. The scheduler orders, the commit path decides.
- **Receipts and outbox** must define `Queued` and terminal states. The scheduler consumes queued work and emits proposals.
- **Manifest and static shard map** must map a key to one shard. The scheduler may not guess a shard.
- **Enforcement and audit** must validate at commit. The scheduler must not call the policy evaluator.
- **Event stream** inherits per-key order. It must not add another ordering layer.

## Acceptance criteria

1. A randomized property test proves per-key FIFO across all interleavings.
2. A blocked key does not delay a disjoint ready key in the same shard.
3. A blocked shard does not reduce another shard's selections over `N` calls.
4. Golden vectors prove equal state and budget yield equal batches on native and wasm.
5. Instrumentation proves the `B` and `D` bounds on every call.
6. A test proves the starvation bound for every ready request.
7. Admission rejects cycles, forward edges, cross-shard edges, and fan-in overflow with named errors.
8. Replay rebuilds the batch sequence from the log with no divergence.
9. Every unselected request carries one reason.
10. A build gate rejects clock, randomness, and I/O in the core.

## What would falsify this design

- A real workload needs cross-key ordering the application cannot express as an edge.
- Per-shard reservation causes unacceptable latency on a skewed shard map.
- Ready-set upkeep costs more than the scan it avoids.
- Determinism fails on wasm because ordered state is not actually ordered.
- Applications cannot name dependencies without reading store state, so explicit edges are too weak.
- A single key carries most traffic, so per-key FIFO serializes the workload. That is a data-model signal, but it falsifies the throughput claim.
- Priorities prove necessary for correctness rather than urgency.

## Open questions

1. Do edges stay intra-shard, or may a request declare a cross-shard read dependency? Recommendation: intra-shard only.
2. Is an edge named by idempotency key or request id? A key survives retry and is the better reference.
3. Should budget allocation use fixed reservation or deterministic water-fill?
4. Is a multi-request batch per shard needed at 1.0, or is one request enough?
5. Does the ready set persist, or rebuild from the queue? Rebuild is simpler but must produce the same set.
6. How does the system detect an in-flight request that never resolves?
7. Where does priority live if not in the scheduler, and how does admission enforce it fairly?
8. When two shards compete for the last slot, is the tie-break the lowest shard id or the epoch?
