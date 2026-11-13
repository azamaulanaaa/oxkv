# 24. Event stream

The business event stream is the single exit from the store to the world for write side effects. Rules must stay pure, and a pure rule cannot send mail, charge a card, or call a webhook. So a rule returns a verdict and, at most, a bounded list of business events. The authority commits those events in the same transaction as the data change, and every downstream effect reads that committed stream. A separate operational feed carries audit export, metrics, traces, budget signals, lifecycle notices, and alerts. The business stream gives per-key order, at-least-once delivery, an `EventCursor`, a retention rule, a per-key acknowledgement rule, and the A3 envelope.

## What the greater goal demands

- Rules are deterministic and bounded. The policy language has no clock, no randomness, and no network, and the business event path must not add an escape hatch.
- The authority owns truth and serializes writes. A business event becomes real only when the write commits, so a rejected or conflicting write produces no business event. Replicas are read-only, so only the authority appends.
- A data shard contains one user table and its system sub-namespaces, so event order is defined inside a shard and only per key.
- The manifest declares rules, operations, and event types. A write carries an idempotency key and a `base_version`, and its receipt names the events it caused.
- A3 (event envelope) freezes at 1.0 with the rest of the declarative surface.

The business stream keeps business logic outside the store. A team that sends mail, updates a search cluster, or invalidates a cache writes a consumer. The consumer subscribes, replays, acknowledges, and owns its retries. The store guarantees delivery and order. The consumer owns meaning and idempotence. Audit export and budget signals are operational signals, not business events.

## Required capabilities

1. **Atomic append.** The authority appends the business event in the commit transaction, so no crash separates them.
2. **Per-key ordering.** Every event carries a key and a monotonic sequence. Events for one key are totally ordered, and events for different keys have no promised order.
3. **At-least-once delivery.** A consumer receives every retained event at least once. Duplicates are permitted. Silent loss is not.
4. **Replay cursor.** An opaque cursor identifies a position, and a consumer resumes from it after a disconnect.
5. **Retention rule.** Each shard enforces a time bound, a byte bound, or both, and truncates only acknowledged or expired events.
6. **Acknowledgement rule.** An acknowledgement advances the `EventCursor`. It is contiguous per key, so one unacknowledged event blocks that key and no other.
7. **A3 envelope.** The envelope is self-describing, canonical, and versioned. An unknown `event_version` is `event.unknown_version`.
8. **No rule network access.** The rule host exposes one effect primitive, emit, and no socket, HTTP client, or host import that reaches the network.
9. **Consumer protocol.** Subscribe, read with credit, acknowledge, negative-acknowledge, seek, and observe lag. A cursor older than retention returns a named gap error with the oldest position, and a poison event has a terminal dead-letter route.
10. **Bounded payloads.** Large data travels by reference.

## Invariants that must hold

- An event is readable if and only if its write committed.
- The business event id is deterministic. It derives from shard, key, `commit_version`, rule identity, and emission index, so replay yields the same id.
- Per-key sequence order equals commit order for that key. A consumer never sees a lower sequence after a higher sequence.
- Rule evaluation cannot observe delivery state, acknowledgement state, or the network.
- No side effect occurs before the append. An outbound call made by the store is a contract violation.
- The envelope carries the pinned policy version and rule identity, so a consumer can interpret old events after a rule change.
- Retention never discards an event without a signal.
- Native and WASM targets produce byte-identical envelopes and ids for the same commit.

## Decisions and technical choices

The main choice is where the durable event lives.

**Tapping the write-ahead log.** It derives events from the storage log, with the lowest write amplification and natural ordering. But it couples the public contract to physical storage: compaction, tiering, and migration leak into the consumer contract, and the business event vanishes when the segment is recycled. Reject it as the C5 and A3 contract, but allow it as an internal read path behind the same interface.

**An outbox table.** An event is a row in a reserved table, written in the same transaction as the data. Ordering comes from the key order the store guarantees. Retention, scans, snapshot, and restore reuse table semantics, and it works on both targets at the cost of a second write. Accept it as the logical contract. The seam needs two in-tree implementations, so a log-structured append and a table-backed append must both satisfy the consumer protocol.

**Change data capture.** CDC reads committed changes from a replication feed and derives events afterwards. It fits an external source, but here the source is the authority. It cannot express an intent that is not a direct diff of user data, such as "send a reminder", and it exposes physical schema. Reject it.

The chosen shape is a persisted outbox record per event, keyed by shard and sequence. A relay owned by the authority serves the consumer protocol from that record. The relay is stateless beyond cursor bookkeeping.

Retention defaults to a time bound with a byte cap. A consumer-held cursor keeps server state small. A cursor behind retention returns the gap error and the oldest position, and the consumer chooses a snapshot backfill or a dead-letter review.

Acknowledgement is contiguous per key. This keeps the `EventCursor` small and the order statement honest. The cost is head-of-line blocking on one poison event, which the dead-letter path resolves. A consumer that needs cross-key order must reduce its keys to one.

The envelope has a deterministic binary wire encoding and a canonical text form. Its fields are `envelope_version`, `event_id`, `shard_id`, `table`, `key`, `key_sequence`, `commit_version`, `committed_at`, `rule_id`, `policy_version`, `emission_index`, `payload`, `payload_schema`, and `origin_request_id`. `committed_at` is an injected commit parameter. The core must not read a clock. The envelope must not carry the raw idempotency key.

The rule host enforces purity by construction. The grammar has no I/O node. The sandbox imports no host function except a bounded allocator and emit. A rule that tries to reach the network fails to load or fails deterministically. Scope and validation read the same evaluator.

## Dependencies

- C1 (manifest schema) declares rules, operations, and event types before A3 freezes.
- Versioned writes and C5 (receipt and error semantics) define commit, rejection, and conflict, because the business event contract refers to them.
- The write scheduler guarantees the per-key order the business stream publishes.
- A1 (key encoding), A4 (shard map), and C2 (snapshot and page format) define key bytes, shard identity, and outbox restore.
- Policy projection carries the pinned `policy_version` the envelope records.
- The reference effect consumer and generated RPC depend on the consumer protocol.
- A3 (event envelope) freezes at 1.0 as an annex to C5 (receipt and error semantics).
- The operational feed is separate and has its own envelope and retention.

## Acceptance criteria

- A committed write that matches a rule emits exactly one event with a stable id. A rejected or conflict write emits none.
- Kill the authority after commit and before relay. Restart. The business event is readable.
- Disconnect a consumer mid-stream. Resume from its `EventCursor`. No event is skipped, duplicates are allowed, and per-key order holds.
- Stop acknowledging. The consumer receives `event.retention_gap` with the oldest position, not silence.
- Attempt a rule with a network import. The load fails. A rule that performs I/O fails deterministically, and replay matches.
- Replay a commit twice and run it on native and WASM. The ids and envelope bytes match.
- Interleave events from two shards. The test asserts that no cross-shard order is promised.

## What would falsify this design

- A storage backend cannot commit the outbox row and the data change in one transaction. Atomicity fails, and the design needs a two-phase or at-most-once concession.
- A required effect needs a synchronous answer inside the write transaction, such as payment authorization. The write must become a saga, and the stream alone is not sufficient.
- Consumers must replay from the beginning forever. The log is then an archive, a different product with different retention economics.
- Per-key order proves insufficient and users demand global order. Satisfying that demand costs a single writer and breaks shard independence.
- Event volume dominates data volume, so the second write overwhelms the simplicity gain.

## Open questions

1. Does the outbox live in the `0x02` event system sub-namespace or a hidden namespace?
2. Should the sequence be per key, per shard, or both, and does a contiguous `EventCursor` or a gap set govern acknowledgement?
3. Do `EventCursor` values live on the consumer or as server-side group offsets, and how do multiple instances coordinate?
4. What default retention pair of time and bytes, and what gap error taxonomy, does A3 freeze at 1.0?
5. Does a rule emit one event or a bounded list, and how does that interact with deterministic ids?
