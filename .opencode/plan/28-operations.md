# 28. Operations

Operability is what a business buys. A team can forgive a slower query. It cannot forgive a restore with no defined recovery point, a migration that loses a committed write, or an outage that nobody can explain. C1-C5 and A1-A12 freeze the bytes and the semantics. Operations must fix the answers to four questions: what can I recover, how do I move a shard to a new format, how do I bound its appetite, and how do I know it is sick. The operator is a first-class client.

## What the greater goal demands

Authority owns truth and serializes writes. A replica holds a scope, an index, and an outbox, and never writes. A client declares a query, not a mutation. No operational action may write data except through a receipt.

A data shard contains one user table and its system sub-namespaces, and it is the unit of placement, ownership, transaction, fan-out, sync, and replication. Every action is shard-scoped. A cluster command is a loop with one result per shard. The static map makes a shard addressable without a placement service, so a failure has a bounded blast radius.

C1-C5 and A1-A12 set what operations carry. A backup names a `manifest_digest` and a `snapshot_page_version`. A restore names a `SyncCursor` and a `policy_version`. Health must use the same vocabulary. Local-first also means a restore must not strand an offline replica.

## Required capabilities

**Backup and restore.** Backup is a set of ShardSnapshots with a version vector. A backup records the `manifest_digest`, `snapshot_page_version`, `policy_version`, `map_version`, object list, and a checksum per object. It also includes receipt, idempotency, event outbox, and audit state. It copies only immutable objects reachable at the captured vector and holds a retention pin that blocks compaction and collection until it completes or fails. A global consistency point is allowed only after a global write quiesce. A continuous log archive moves the recovery point toward the last durable write.

Restore writes a new epoch and verifies every object before serving a read. It preserves terminal receipts and event cursors, or marks them expired with a named error. It must not re-execute an effect. It writes a manifest for those objects, claims ownership through the normal epoch path, and never mutates the source epoch. A lost pin fails with `ops.backup_pin_lost`. A checksum mismatch fails with `ops.restore_checksum_mismatch` and leaves the source epoch unchanged.

**Migration window.** A window is a published interval. The binary reads format N and N-1 and writes only N. A shard quiesces writes, drains the scheduler, rewrites under a new epoch, verifies counts and checksums, and flips the pointer. Reads continue from replicas. New writes get a Rejected receipt with C5 reason `shard.frozen_for_maintenance`. Rollback is allowed only before the first new-format write. After the first new-format write, the store must not downgrade. An unknown format fails with `format.unsupported_version`.

**Compaction control.** Compaction is automatic with a budget in bytes per second and files per hour. An operator pauses with an expiry, resumes, or runs a pass. Expiry and a quota threshold end a forgotten pause. Pins block deletion. Compaction reports backlog, oldest input age, and bytes rewritten.

**Metrics and traces.** Each contract needs a signal. Manifest: version, epoch, conflict rate. Snapshot and page: checksum and restore failures. Sync: lag, cursor age, resume rate, full-resync rate. Policy: evaluation time and budget rejects. Receipts: outcomes, oldest Queued age, named error rates. Traces mirror the contracts: one write trace from acceptance to event, one query trace with a span per shard, one sync trace from handshake to applied diff.

**Quotas and rate limits.** Quotas are per shard: stored bytes, log bytes, in-flight requests, query cost, event backlog, replicas. Token buckets apply per shard, client, and operation class. Admission rejects before work starts. A reserve protects sync and events. A denied write is a Rejected receipt with a named reason. A denied read is a named error. Neither drops connections.

**Sick shard and runbook.** A sick shard violates its service level while its neighbors are healthy. Health is a pure function over sampled counters. Two leading indicators over five minutes, or one critical indicator over one minute, mark a shard sick. Indicators are conflict rate, leader loss, write-gate wait, log backlog, compaction backlog, oldest Queued receipt, replica lag, and quota saturation. The verdict names the top cause.

Confirm the blast radius. Read the cause. Protect before restart: rate-limit, pause compaction, or raise the quota. Quiesce writes only when truth is in doubt, then drain or reject Queued receipts. Recover by an epoch claim or restore. Verify terminal receipts, falling lag, and a low error rate. Record the incident.

## Invariants that must hold

- Truth has one owner per shard. No control action writes data outside a receipt.
- No operation spans shards. A multi-shard action returns one result per shard.
- A backup has one version vector and completes or fails loudly.
- No receipt stays Queued past the authority queue deadline. Maintenance ends every request in a terminal state or leaves it `Queued` with a deadline.
- A migration never loses a committed write.
- Format, policy, and map versions are explicit in every backup and restore.
- Quotas reject at admission, not after exhaustion.
- Telemetry comes from protocol events. The core stays sans-IO.
- Every control action is audited and cannot impersonate a data write.

## Decisions and technical choices

**Backup mechanism.** A logical dump is portable and format independent, but it is not point-in-time and costs a full pass. An in-memory snapshot is exact, but its cost equals shard size. A physical copy of immutable objects with a retention pin is fast, byte-exact, and cheap to restore, but it couples to the storage layout. **Recommendation:** physical copy plus log archiving, logical dump for export, snapshot for tests.

**Migration.** An online dual-format rewrite avoids downtime, but it doubles the read paths and is hard to prove. Offline per-shard migration with a new epoch bounds the blast radius and allows rollback only before the first new-format write. Copy to a new shard keeps the old intact, but it doubles storage and changes the map. **Recommendation:** offline per-shard, with copy-and-switch for a shard too large for the window.

**Operational surface.** CLI-first works when the authority is degraded, but it has no shared history, weak authorization, and no safe concurrency. API-first gives one audited contract to the CLI, the UI, and automation, at the cost of authentication and availability. Dashboard-first is glanceable and useless for automation. **Recommendation:** API-first control plane, CLI as the reference client, dashboard as a read-only projection. Control actions never bypass the data path.

## Dependencies

C2 (snapshot and page format): versioned ShardSnapshot and page format. C1 (manifest schema): the manifest a backup pins. C5 (receipt and error semantics): the receipt and error taxonomy. A6 (projection staleness): the policy projection and staleness bound. The replication driver a restore must fence. A4 (shard map): the static shard map. A11 (read pin and staleness): the service levels health measures. The operational feed for audit. Rule lifecycle and audit path. Authentication and audit export. Qualification vectors. The deprecation policy and window length.

## Acceptance criteria

1. A 10 GB backup completes under write load and records a version vector. A restore reproduces every key, value, index entry, receipt, and event cursor at the captured vector.
2. A lost pin fails with `ops.backup_pin_lost`. A checksum mismatch fails with `ops.restore_checksum_mismatch` and leaves the source epoch unchanged.
3. A migration drains every authority-known receipt to `Committed`, `Rejected`, or `Conflict`. No receipt stays `Queued` past the queue deadline.
4. A write during maintenance gets a Rejected receipt with C5 reason `shard.frozen_for_maintenance`. Reads continue.
5. A migration from N-1 to N and its rollback complete inside one window when rollback is allowed. A rollback after the first new-format write returns `ops.downgrade_not_allowed`.
6. A paused compaction holds the input file count steady for ten minutes. Resume or expiry reduces it.
7. A quota breach rejects at admission with a named error, inside the memory budget, without dropping connections.
8. A conflict storm marks a shard sick within five minutes and names the cause.
9. Every control action is audited with actor, shard, `policy_version`, and result.
10. Two backup targets and two telemetry exporters pass the same contract tests.
11. A restore does not re-execute a committed effect. A client with an old outbox entry receives `ops.client_outbox_stale` and a rebase path.

## What would falsify this design

- A restore cannot reproduce a consistent index or search result at the chosen version.
- A migration needs a cross-shard transaction or can lose a committed write.
- Backup or restore misses its recovery point or time under load.
- An operator cannot separate a sick shard from a healthy one without raw logs.
- Quota enforcement needs a post-hoc kill.
- A control action needs a receipt bypass, or the error taxonomy needs a fifth receipt state.
- Pins let storage grow without a bound.

## Open questions

- What recovery point and time does 1.0 promise, and at what cadence?
- Who revokes a pin when the holder dies?
- Should a cross-shard query during one-shard maintenance return a coverage result or fail?
- Does 1.0 add owner expiry, or does the operator remain the dead-writer detector?
- How does a backup read a node-local object? The hold protocol is undesigned.
- How are limits shared across replicas and event delivery without starving sync?
- Is control authorization separate from policy?
- How does a replica rehydrate after an epoch change without a full resync?
