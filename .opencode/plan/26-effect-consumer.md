# 26. Reference effect consumer

A rule decides that an invoice is approved. The business event stream carries that decision. The last service left turns it into an email: a small consumer that performs one effect and records that it did. oxkv does not remove the network. It confines the network to one component with a durable `EventCursor`, an idempotency record, and a dead-letter rule.

## What the greater goal demands

The greater goal fixes the consumer's shape. A query is the unit of ownership. The authority owns truth and serializes writes. A client never writes. It submits a request with an idempotency key and `base_version`, then observes a receipt. The business event stream is the only path to write side effects. Rules decide, services act.

The consumer must not validate policy, write an authority shard, or repair data. It must consume a committed event and perform an external effect. The rule engine must stay deterministic and free of I/O, so the network call and its failures live here. An email outage must not delay an invoice write.

The consumer is the surviving remnant of a service, with its own secrets, scaling, and failure domain. It must stay auditable.

## Required capabilities

1. Subscription and `EventCursor`. It must attach to shards, read per-key order in bounded batches, keep a durable `EventCursor`, and resume without replay.
2. Handler boundary. It must expose one handler per type. The handler receives the business event, an idempotency key, and a deadline, but no authority write handle.
3. At-least-once and idempotency. It must treat every event as deliverable more than once, check a completion record before a call, and write it after success. It must not offer exactly-once.
4. Ack and nack. An ack follows the effect and commits the record with the `EventCursor`. A nack schedules a retry and does not advance the `EventCursor`.
5. Retry. It must use jittered exponential backoff, bounded by attempts and time, and honor a provider delay hint.
6. Dead-letter rule. It must move an event to a dead-letter queue (DLQ) on terminal failure or exhausted budget. The DLQ write and `EventCursor` advance commit together.
7. Poison handling. Treat unknown type, unsupported schema, decode failure, permanent client error, and panic as terminal. Never retry a deterministic failure.
8. Redrive. It must let an operator retry one DLQ record with the original key and an audit record.
9. Visibility. It must expose lag per key, attempt outcomes, and DLQ depth.
10. Self-contained payload. A required effect field must come from the payload. The handler must not read the authority.

## Invariants that must hold

1. No effect before commit. An event appears only for a Committed write. The consumer never acts on Queued, Rejected, or Conflict.
2. No silent loss. Every event ends as an ack or a DLQ record. A crash may duplicate work, not skip it. The ack follows the effect, and the record or provider key absorbs a crash between them.
3. Order is per key. First delivery on one key follows commit order. The consumer may assume per-key order only. Cross-key, cross-shard, and retry order are not guaranteed.
4. Per-key processing has a cost. One `EventCursor` per key means a poison event delays only that key until the DLQ. The retry budget bounds it.
5. The handler cannot write the authority. A data change found during an effect becomes a new request with its own key and `base_version`.
6. State is scoped. `EventCursor`, journal, and DLQ are isolated per consumer and tenant. Payloads and secrets stay out of logs and metrics.
7. The core is sans-IO and deterministic. It takes a clock and a transport so crash, retry, and duplicate paths are testable without a provider.

## Decisions and technical choices

**Delivery: library, runtime, or pattern.** A library owns the loop, journal, retry, and DLQ behind a handler trait. The deployment supplies secrets, alerts, and the provider client. It is small and testable, but each deployment owns its operations. A runtime standardizes the DLQ and metrics, but recreates the service this design retires and freezes a plugin ABI outside C1-C5 and A1-A12. A documented pattern leaves ack ordering to each reader. Recommendation: ship the library and a reference binary. No plugin runtime in 1.0.

**Transport: store-native or broker.** A broker offers mature retry and DLQ tools. It also creates a second durable log with a second retention policy and breaks local-first resume. The business event stream must remain the source of truth. Recommendation: store-native, with transport behind a trait.

**Cursor: per shard, per key, or global.** A global cursor needs an order the system does not provide. A per-key `EventCursor` allows independent progress and bounds a poison event to one key. A per-shard `EventCursor` is one pointer but blocks every key on a poison event. Recommendation: per key in 1.0, with parallel consumers across keys.

**Idempotency: provider key, journal, or both.** The provider key is the only mechanism that prevents a second effect across a crash. The journal records completion and supports audit, but cannot close the gap between the effect and its own write. Recommendation: both. The key is the guarantee, and the journal is the record. A provider with neither cannot support an exactly-once effect.

### Example: an approved invoice

An approval rule commits `invoice/1042` as approved and appends `invoice.approved` in one atomic unit. The receipt is `Committed`.

1. `invoice-mail` reads sequence 91 on shard `invoice/`.
2. The handler builds the message from the payload. It does not read the authority.
3. It calls the provider with `Idempotency-Key: oxkv/invoice.approved/<event_id>`.
4. It writes the completion record and `EventCursor` 91 in one transaction.
5. Attempt one times out. Attempt two sends the same key and gets the first result.
6. Eight attempts fail. The business event moves to the DLQ. The mail is late, not lost.

Mail is the roadmap example, not the easy case. Many providers do not enforce a key, so the consumer can only narrow the duplicate window.

## Dependencies

- **24, Event stream.** Supplies A3 (event envelope), `EventCursor`, per-key order, and at-least-once guarantee. It ships first.
- **25, Generated RPC.** One protocol core and three transports.
- **22, Static shard map.** The shard set for `EventCursor` and fan-out.
- **10, Receipts and outbox.** A business event only for a Committed write.
- **3, Hooks.** The business event is atomic with the data, with no I/O in the rule.
- **7, Manifest.** Declares event types. Reject an unknown subscription at startup.
- **28, Operations.** Alerts, redrive, and backup.
- **30, Qualification suite.** The crash, duplicate, and poison matrix.

## Acceptance criteria

1. Ten deliveries of one event must produce one effect, counted by key.
2. A kill after provider accept and before ack must redeliver. A kill after ack must not. The count must stay one.
3. A retryable failure must wait for backoff. A virtual clock must show jittered growth inside the cap.
4. A terminal failure must produce one DLQ record on attempt one.
5. An exhausted budget must produce one DLQ record and an `EventCursor` advance in one transaction.
6. A DLQ record must hold event, attempts, timestamps, error class, and `commit_version`, with no secrets.
7. Two keys may interleave. One key must stay in order when no event is dead-lettered.
8. A poison event on key A must not stop key B. Key A must resume within budget.
9. A handler must have no authority handle. A write must fail at the boundary.
10. A malformed payload must never reach the handler. Reason: `event.poison`.
11. A restart must resume from the last ack. Lag, attempts, and depth are visible per key.

## What would falsify this design

- A provider has no key or lookup API, but the product requires one effect. It can only bound the duplicate window.
- A business rule needs global order. A per-key `EventCursor` cannot provide it. Use one ordering key or a coordinator.
- One slow key must never delay another on a shard. A per-key `EventCursor` provides that isolation.
- The business event is not atomic with the commit. A phantom or lost effect follows. The business event requirement fails first.
- The DLQ must live in the authority while clients never write. One rule must change.
- The consumer must run offline longer than retention. A cursor cannot resume. Add a snapshot and backfill.
- The business event lacks the fields the effect needs. The handler reads newer state. Business event capture must enter the envelope.

## Open questions

- All events or a policy-filtered scope? If a scope, how does a revocation affect an open cursor?
- Where does the DLQ live? A consumer store is durable only if it is. An authority DLQ gives the consumer write access.
- How long must completion records live? The window must exceed the redelivery horizon and the restore window.
- Per-key cursors in 1.x, or per-shard cursors with a documented head-of-line cost?
- Does redrive call the handler directly or append at the tail? The first can break per-key order.
- Is a duplicate window acceptable for mail? It is not for payments or shipment.
