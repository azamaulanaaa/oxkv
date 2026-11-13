# 14. Wire protocol v1

The wire protocol lets a replica own a query. A client declares a scope once and reads locally while offline. On return, the authority must send only the missed changes. A browser must complete the same exchange as a native process. These demands make the protocol a frozen byte contract in C3 and a pure state machine. The state machine decides what to send, and a driver moves the bytes. C3 freezes the message set, resume semantics, and error names.

## What the greater goal demands

The replica is read-only and the authority is the only writer. Downstream the protocol carries ScopeSnapshot pages and diffs. Upstream it carries write requests with an idempotency key and `base_version`, and returns receipts. It must never give the replica a direct path to mutation.

A query is the unit of ownership, so the subscription, not the connection, is the durable object. One subscription may survive many dropped connections. The resume point must survive the process, network, and transport.

C3 covers the sync wire protocol: a browser that holds a v1 `SyncCursor` must work against every 1.x authority. Every exchange must name its `sync_wire_version`, and an unknown `sync_wire_version` must fail with a named error.

A day-long disconnect is the design case. The authority keeps a bounded change history, so a `SyncCursor` can expire. Expiry must be a named outcome with a recovery path, not a transport failure.

## Required capabilities

**SessionHello negotiation.** One `SessionHello` opens each connection. The client sends its `sync_wire_version`, required and optional capabilities, and limits. The authority answers with the chosen `sync_wire_version`, the capability intersection, its limits, and a session identity. `SessionHello` negotiates the RPC descriptor, `sync_wire_version`, `manifest_digest`, `map_version`, capabilities, and limits. It must not negotiate a `policy_version`. A scope subscription carries its `policy_version` and scope pin. A missing required capability must fail with a named error. No later message may renegotiate.

**Scope subscription.** A client subscribes to a scope: user-table prefixes, a policy payload, a pinned `policy_version`, the required indexes, and a `SyncCursor` or a ScopeSnapshot request. The policy payload may travel as opaque bytes. RPC must not decode or rewrite it. The authority must reject an unbounded scope and return a scope identity, the effective `policy_version`, a starting scope version vector, and a `SyncCursor`.

**Diff stream.** The steady state is an ordered, gap-free stream of diffs. Each diff states the shard, key, operation, value or tombstone, and `commit_version`. A diff must state when a key leaves the scope, so the replica can prune. The stream must replay from a `SyncCursor` and tolerate a second apply. Exact mode is the default. Coverage mode is opt-in and must carry a coverage descriptor. A coverage stream must not claim exactness.

**Opaque resumable `SyncCursor`.** The client stores the `SyncCursor` and returns it unchanged, and must never parse it. The token must be bounded, tamper-evident, and valid after an authority restart. Opacity lets the authority change retention and indexing without breaking 1.x clients.

**Snapshot fallback.** A ScopeSnapshot is the recovery path for a new subscription, an expired `SyncCursor`, and a detected gap. The authority sends a page manifest, checksummed pages, the scope version vector, and a tail `SyncCursor`. The handoff to the tail must be atomic for the replica.

**Backpressure.** Flow control is credit based. The replica grants credit in bytes and messages. The authority must not send beyond the credit, and a slow replica must not cause unbounded buffering. Keepalive must detect a dead peer.

**Named errors.** The error set is closed and frozen in C5. It must include `sync.unsupported_version`, `sync.unknown_capability`, `sync.scope_rejected`, `scope.policy_version_unknown`, `sync.cursor_expired`, `sync.snapshot_unavailable`, `sync.gap_too_large`, `sync.rate_limited`, `sync.unauthorized`, and `sync.shard_unavailable`. Each error must state whether it is retryable, whether the client must resync, and whether the `SyncCursor` stays valid.

## Invariants that must hold

1. **One core.** The state machine has no clock, socket, or filesystem. Time arrives as input. The same input sequence must produce the same output.
2. **No transport semantics.** Every driver must carry the same messages with the same meaning. A driver may change framing and lifecycle, not ordering, retry, or error meaning.
3. **`SyncCursor` monotonicity.** The observed scope version vector must never decrease. A replay must return one diff sequence.
4. **Gap detection.** The replica must detect a missing `commit_version` before it applies a later one.
5. **Idempotent apply.** Two applies of one diff must equal one apply.
6. **Scope confinement.** No byte outside the declared scope may leave the authority. Scope fuzzing must test this.
7. **Atomic handoff.** ScopeSnapshot plus tail must cover the window with no hole. A duplicate is safe only because apply is idempotent.
8. **Bounded work.** Every message and negotiation step must have size and count limits.
9. **Total error mapping.** Every failure must map to a named error. A raw transport error must never stand in for a protocol outcome.
10. **Frozen bytes in C3.** Within a major version, changes are additive only. A golden vector must pin every message type.

## Decisions and technical choices

**A custom sans-IO sync protocol over Connect.** The sync protocol keeps its own versioning, frame types, streaming, diff semantics, cursor, and error codes. Connect is the RPC envelope for generated RPC. A Connect server stream may carry sync frames as opaque bytes. RPC must not re-encode a sync diff entry into a second schema. A custom RPC framework would make the project own framing, flow control, multiplexing, and every binding. gRPC in a browser needs a proxy and creates a second protocol. Both are rejected.

The sync frames keep control. The envelope is readable and the payloads are typed and generated from the manifest. It states the message type, `sync_wire_version`, scope identity, `SyncCursor`, credit, and correlation identity. Page bodies stay binary. Human-readable before fast: a hexdump must identify the message without a schema.

The cost is real: code generation, tooling, and interoperability tests. Connect gives generated RPC and browser support without a proxy. The decision holds only because the sync frames must stay stable for years.

**Transports are drivers.** The core emits actions: send, close, set a timer, grant credit. Drivers translate actions for in-process calls, a length-prefixed stream, WebSocket messages, and fetch-based unary calls. The in-process driver is the reference. The qualification suite must compare every other driver against it.

**Diffs first, ScopeSnapshot as recovery.** A normal resume must not send a ScopeSnapshot. The authority must send a ScopeSnapshot only when the gap exceeds a threshold or the `SyncCursor` is invalid.

**`SyncCursor` form.** The `SyncCursor` holds the scope generation, a per-shard scope version vector, and a retention floor identity. It must be authenticated, so a client cannot forge progress. Every diff response must state the oldest `commit_version` the authority can serve, so the client learns expiry before a wasted round trip.

## Dependencies

- A1 (key encoding) defines the keys inside every diff.
- C2 (snapshot and page format) defines the ScopeSnapshot bytes.
- C1 (manifest schema) defines the message set and subscription surface.
- C4 (policy language) defines the scope predicate and its bounded evaluation.
- A5 (scope semantics) defines the subscription unit and the leave-scope diff.
- C5 (receipt and error semantics) defines the upstream write outcome and error vocabulary.
- A9 (cursor types) defines `SyncCursor`.
- The business event stream is separate. A diff replicates state, and a business event triggers a side effect. They must not share an envelope. Audit export and metrics use the operational feed.
- The client replica and hydration consume this protocol. Generated RPC wraps it in Connect.

## Acceptance criteria

- A replica disconnects for a simulated day and resumes from a `SyncCursor`. It receives only missed diffs, and the byte count stays under a stated fraction of scope size.
- A browser completes `SessionHello`, subscription, ScopeSnapshot, tail, and resume over Connect and over fetch-based transport. The state matches the native result.
- The in-process and network drivers produce the same message sequence for one input transcript.
- A property test applies each diff stream twice and compares replica state.
- An injected gap forces a ScopeSnapshot fallback with no lost `commit_version`, and an expired `SyncCursor` returns `sync.cursor_expired` with a ScopeSnapshot offer.
- An unsupported `sync_wire_version` returns `sync.unsupported_version` with the supported range.
- A slow consumer causes bounded authority memory and a retryable close with the `SyncCursor` intact.
- A scope fuzz run produces zero out-of-scope bytes.
- Every message type has a golden vector, and a one-byte change fails the suite. A recorded transcript replays to the same output with no clock or I/O.

## What would falsify this design

- A browser cannot run the core without a proxy or a second protocol.
- A bounded `SyncCursor` cannot hold per-shard positions for a large scope.
- The diff volume for a one-day disconnect exceeds the ScopeSnapshot size.
- `SessionHello` needs more than one round trip for common clients.
- Scope evaluation at subscribe time cannot be bounded, or ScopeSnapshot and tail cannot hand off within the write latency budget.

## Open questions

1. Cursor: a signed token or a server-side registry? A registry costs state, a token costs size and rotation.
2. One connection with many scopes, or one per scope? Multiplexing needs stream identities and fair credit.
3. What retention window does the authority advertise, and how does it state the floor without leaking internals?
4. How does authentication bind to a cursor, so one principal cannot replay another cursor?
