# 3. Pre-write and post-write hooks

A write path needs two entries for declared logic: one before the decision, one after. Pre-write is where a rule decides. Post-write is where a rule notifies. Both must be pure, explicit-input, and bounded, because the policy engine and event stream grow from here. If either reaches the network, observes the clock, or re-enters the store, the outcome stops being a function of its inputs, and no receipt or replay can be trusted.

## What the greater goal demands

The query is the unit of ownership. The shard is the unit of transaction. A write is a request with an idempotency key and a base version. Authority serializes it into one of four receipts. Four demands follow:

1. A rejection must be decidable before state changes, so a Rejected receipt changes no data. The rejection still writes a receipt and an audit record, and it consumes the idempotency key for the same body.
2. A verdict must be reproducible from bytes, because C5 (receipt and error semantics) and A7 (audit envelope) are frozen contracts.
3. Side effects must not live in the write path. The business event stream is their only path, so the write path emits facts only.
4. One rule must give one verdict on native and wasm, online and offline, first run and replay.

So pre-write returns a verdict, post-write returns events, and neither performs I/O.

## Required capabilities

- **Phase-typed entry points.** Pre-write maps each proposed row to Accept or Reject. The batch verdict is the conjunction of the row verdicts. Post-write maps a committed batch to an ordered event list.
- **An explicit input envelope.** The batch in declaration order, with key, operation, value, base version, and prior value per touched key. The pinned `policy_version`, `projection_epoch`, and fact versions. The commit position. No ambient state.
- **A verdict type.** A named code, a rule id, a value path, and a bounded message. Read scope and write validation share it and one evaluator.
- **A typed event envelope.** A stable id, the commit position, the key, a manifest-declared kind, and a bounded payload from the committed row.
- **Deterministic order.** The manifest defines event order. Map iteration must not.
- **A resource meter.** Bounds on steps, events, bytes, and nesting.
- **A replay harness.** It runs recorded inputs and compares output bytes.
- **Two in-tree runners behind one contract.** A declarative runner and a reference runner with null and audit hooks. The seam is real only when both pass the same vectors.

## Invariants that must hold

1. **No state change before acceptance.** No mutation, sequence increment, index update, or outbox write. A rejection is total.
2. **No receipt before event derivation.** State and events commit as one unit.
3. **Purity.** A hook reads only its envelope. No I/O, clock, randomness, environment, or global mutation.
4. **Determinism.** Equal inputs produce byte-equal outputs on every host, target, and run.
5. **Bounded cost.** Exhaustion is a named error, not a timeout.
6. **One evaluator.** Read scope and write validation never split.
7. **Total order.** Batch order first, then declaration order within a mutation.
8. **No reentrancy.** No write path, scheduler, or nested hook. One run per phase per batch.
9. **Fail closed.** A malformed rule, a trap, or a missing fact rejects. No hook failure commits.
10. **Replay identity.** Verdict, events, and ids reproduce exactly.

## Decisions and technical choices

**Reject-only pre-write.** Pre-write accepts or rejects. It does not rewrite the batch. A rewrite would make the committed value differ from the requested value, and the client would need a second contract to learn what happened. With reject-only, the client reads the new version through its scope.

**Post-write inside the commit step.** Post-write runs after the state transition is computed and before durability and the receipt. Events and data commit together or not at all. The alternative commits first and derives events later from a watermark. That alternative leaves a window where a write is visible but its event is missing. The window silently loses a side effect and needs a reconciliation loop to close. For 1.0 the atomic choice is smaller. The cost is that derivation counts against commit latency, so the business event budget is a commit budget.

**Events are declared facts.** The manifest declares the kinds a table may emit and the fields each kind carries. A hook selects a kind and binds fields. The envelope freezes as A3 (event envelope), and a rule cannot place an arbitrary payload in the stream.

**Policy is an input, never a lookup.** The pinned snapshot arrives with its version and fact hash. A hook never reads the latest rules. A canary compares two snapshots on one batch without committing twice.

**Hooks are data first.** The manifest declares rules per phase and table in declaration order. A native code hook signs the same contract and passes the vectors.

**Failure semantics.** A pre-write Reject produces a Rejected receipt and one audit record. A version mismatch produces Conflict before hooks run. A budget breach, missing fact, or trap produces Rejected with a named internal error, and the rule version is quarantined. A post-write failure aborts the batch, because events are part of the commit. A Rejected verdict consumes the idempotency key for the same body. A corrected body uses a new key.

**Reentrancy limits.** Depth is zero. A hook is a leaf. It may call the evaluator, but the evaluator never calls a hook. A post-write hook may not submit a write, directly or through the scheduler. Consumption runs outside the shard, in the reference consumer, with ack, nack, and idempotent handling.

**Why the network is fatal.** A network call breaks five things. The verdict depends on a remote service, so the same batch commits or rejects by timing. Replay re-sends effects or fails on a dead endpoint. Native and wasm diverge, so golden vectors cannot exist. A slow endpoint applies backpressure and holds the authority lease. Rule text becomes an egress surface and an SSRF path. Any one defeats a core contract. External data belongs in a versioned projection made before the batch.

## Dependencies

- **Ordered KV and one core for native and wasm.** Without identical semantics, the contract is not testable.
- **Manifest v1.** It declares phases, rule ids, event kinds, and ordering.
- **Policy language v1.** It supplies the evaluator that pre-write calls.
- **Receipt and error taxonomy.** It supplies Rejected, Conflict, and named codes.
- **Policy projection.** It places shard facts in the shard, so validation stays single-shard.
- **Business event stream envelope.** It consumes the post-write output.

The contract lands in wave 1. Policy input is null until the language lands, and event output is captured but not delivered until the stream lands. Define the seam early and fill the ends later.

## Acceptance criteria

- A golden-vector test fixes a batch, a snapshot, and a commit position, then compares verdict and event bytes.
- A rejection test proves no key changes, no sequence advances, and no outbox entry appears. The receipt is Rejected.
- An ordering test touches three keys in order and observes ordered events with stable ids.
- A purity test runs one hook twice at different times and hosts, then compares bytes.
- A static check rejects I/O, clock, randomness, or environment access in a hook module.
- A budget test submits an oversized batch and receives a named budget error within the bound.
- A reentrancy test calls the write path from a hook, then observes a named error and no commit.
- A replay test replays ten thousand commits and compares every event byte and id.
- A crash test stops the process between state write and durability, then observes both data and events or neither.
- A parity test runs the vectors on native and wasm and compares bytes.
- A backlog test blocks the consumer and proves that commits continue and the backlog grows.
- Both runners pass every vector.

## What would falsify this design

- A real rule needs a value outside the envelope, such as a lookup or a rate. The model then needs a versioned snapshot, not a network call.
- Event derivation dominates commit latency at scale. The atomic choice then fails its budget.
- Replay cannot reproduce verdicts because the pinned snapshot omitted a fact. The envelope is then incomplete and the audit claim is false.
- Two runners disagree on event order under one manifest. Ordering is then underspecified.
- A real workflow needs a rule that rewrites the batch. This falsifies reject-only.
- The crash test observes a committed write with a missing event. This falsifies atomic derivation.

## Open questions

1. Does pre-write get prior values for every touched key, or only a declared read set?
2. Are event ids from commit position plus index, or a content hash? Position replays more simply, a hash deduplicates across shards.
3. Does a Rejected receipt consume the idempotency key? Yes, for the same body. A corrected body uses a new key.
4. How does quarantine end? A trapped rule stays rejected until rollback, or disables itself after a threshold.
5. Are hooks declared per table, per shard, or per batch, and how do order rules compose?
6. In a canary, do both rule versions run for every batch or a sample? Duplicate events must be impossible.
