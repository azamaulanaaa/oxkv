# 2. One core, two targets

oxkv must behave as one engine on a server and in a browser. Native and wasm must accept the same inputs, produce the same bytes, and return the same verdicts. This essay derives that requirement from the greater goal. It states the portable core, the platform seams, and the conformance evidence that make it true. Two targets are two adapters around one core, not two implementations of one idea.

## What the greater goal demands

The core contracts C1-C5 and the annexes A1-A12 are byte contracts. A ShardSnapshot, a page, and a wire frame are bytes. A receipt is a state machine with named outcomes. A policy verdict is a deterministic function of facts and a pinned `policy_version`. None of these contracts names a CPU, an operating system, or a browser. A contract that holds on one target and fails on another is not frozen. It is a platform habit.

**Byte identity.** A shard writes a ShardSnapshot. A browser replica must verify and load it. A disagreement about integer width, byte order, key framing, or checksum coverage makes the ShardSnapshot non-portable. Portability is why C2 and A1 freeze.

**Snapshot portability.** The replica owns a scope. Hydration streams ScopeSnapshot pages and hands off to a diff tail. The handoff must be atomic, and the handoff point must be identical on both targets. A difference lets a browser replica miss a range or apply it twice. The same `SyncCursor` must name the same position.

**Client-side search.** Search is first class. A search result and a get must agree about the scope version vector they reflect, and pagination must be stable. That needs one `tokenizer_version`, one comparison order, one tie-break rule, and one `SearchCursor` encoding. A browser that sorts differently returns a different page two, and the client cannot tell.

**Policy and writes.** One evaluator serves read scope and write validation. A semantic difference can make a replica materialize rows the authority would deny, or hide rows it would allow. Receipts must mean the same thing on both targets, including the error name and the base version.

Identical semantics and identical bytes are not a quality goal. They are preconditions for snapshot portability, offline search, and honest receipts.

## Required capabilities

The system must provide:

1. **One portable core.** All semantics must live in one library. It must compile for native and wasm32 without a semantic fork.
2. **Platform adapters.** An adapter must supply only storage, time, entropy, scheduling, and transport. It must not decide a verdict or shape a byte.
3. **Injected clock and randomness.** The core must read time and entropy through interfaces. Production adapters supply real values. Test adapters supply scripted values.
4. **A storage abstraction.** The core must see one ordered key-value interface with range scan, cursor pagination, atomic batch commit, and declared durability. Native and browser backends must both implement it.
5. **An async boundary that preserves semantics.** The core must expose explicit step functions over a sans-IO state machine. An executor may drive those steps. Scheduling must not change outputs.
6. **Shared conformance vectors.** Golden vectors must cover keys, snapshot and page bytes, wire frames, policy, and receipts. Both targets must run the same vectors.
7. **Versioned behaviour as data.** `snapshot_page_version`, `tokenizer_version`, and `policy_ast_version` must travel with the data. A build flag must not select behaviour. The axes follow section 8.2: contract, data, identity, fence, and session.

## Invariants that must hold

I1. **Semantic identity.** For one ordered input sequence, native and wasm must produce identical observable outputs.

I2. **Byte identity.** An artifact serialized on one target must decode on the other and re-encode to the same bytes.

I3. **No ambient input.** Clock, randomness, locale, environment variables, hostname, thread count, and filesystem metadata must not affect a decision or a byte.

I4. **Ordering identity.** The same logical keyspace must iterate in the same order on both targets, independent of backend and data volume.

I5. **Failure identity.** The same fault must produce the same named error and the same terminal state.

I6. **Offline equality.** An offline replica must answer reads and searches from its scope as a connected replica would at the same version.

I7. **Parallelism is optional.** Concurrency may change latency. It must not change results.

## Decisions and technical choices

**One core, thin adapters.** The core must not contain platform conditionals on a semantic path. A build check must reject them. Only the adapter crate may select a target.

**Sans-IO cores.** Protocol and policy cores must exchange values, not sockets. A caller supplies input bytes and receives output bytes plus effects. A test can replay a trace without a network.

**Two implementations per seam.** A seam is real only when two in-tree implementations exercise it. The storage seam needs a native backend and a browser backend. The clock and entropy seams each need a real adapter and a scripted adapter.

**Canonical encoding.** Formats must use fixed-width integers, explicit byte order, length-prefixed fields, and checksums over a stated range. Formats must not use floats. A fraction must use fixed-point with a stated scale.

**Pinned text behaviour.** Normalization, case folding, and tokenization must be versioned data. Both targets must emit the same tokens for the same input.

**Threads.** Native may use threads. Browser may lack them. Threads are a scheduling resource, not a semantic one. Shard work must merge in canonical key order, which makes thread count invisible.

**Atomics.** The core must not require 64-bit atomics that wasm32 cannot provide. Shared state must use a portable primitive, or the core must run single-threaded per shard.

**Filesystem.** Paths, directory order, and metadata must not enter a decision. The adapter maps the interface onto files or browser storage. The adapter, not the core, declares durability.

**Async.** The core must expose a step function with an explicit budget. The adapter drives it. Different step orders must reach the same final state. Cancellation must be a named outcome, not a dropped future.

**Human-readable before fast.** Headers must be self-describing. Errors must carry a stable name and a version. A reader must decode a frame without the writer's source.

## Dependencies

This design depends on the ordered key-value contract and four frozen formats: C2 (snapshot and page format), A1 (key encoding), C4 (policy language), and C3 (sync wire protocol). It depends on C5 (receipt and error semantics) and on the qualification suite, where the vectors run. It does not depend on Raft, tiering beyond one backend, or generated RPC.

The wire format must not ship before A1 and C2. The policy evaluator must be single across targets. A test-only differential oracle may exist outside the production build. A second production evaluator would break this design.

## Acceptance criteria

AC1. Given one ordered input log, native and wasm must emit byte-identical keys, snapshots, pages, wire frames, receipts, and errors.

AC2. A snapshot written on one target must hydrate the other target. Checksums must verify. Re-encoding must produce identical bytes.

AC3. The qualification suite must run the golden vectors for every core contract C1-C5 and every annex A1-A12 on both targets. A missing target run must fail the gate.

AC4. A static check must fail when core modules read the clock, entropy, environment, or locale directly.

AC5. A static check must fail when a platform conditional appears on a semantic path in the core.

AC6. A native to wasm to native round trip must preserve bytes and query results.

AC7. For one scope, one query, and one version, both targets must return the same ordered results and the same page cursors.

AC8. Under injected faults, both targets must reach the same terminal state and named error. Faults include a truncated write, an interrupted snapshot handoff, and a restart during outbox drain.

AC9. The same input sequence under two different executors must produce the same outputs.

AC10. With threads enabled on native and disabled on wasm, results must remain identical.

## What would falsify this design

The design fails if one input pair produces different bytes or verdicts. It fails if a cross-target snapshot does not load. It fails if search returns a different order or cursor for the same version. It fails if a semantic path needs a platform branch. It fails if a golden vector cannot run in a browser. It fails if clock resolution or thread count changes a result. It fails if performance needs a semantic fork.

A target-specific fast path that skips a check would pass benchmarks and break portability. The conformance vectors must run against the fast path, not only the reference path.

## Open questions

1. Which browser storage primitive is primary, and what durability does each provide?
2. Will wasm threads become a requirement, and can parallelism stay a pure optimization?
3. Should the async boundary expose only step functions, or also an async facade?
4. How does a tokenizer or Unicode upgrade preserve old snapshot readability?
5. Does the cost model need fractions, and what fixed-point scale is canonical?
6. Should a crate boundary, build script, or lint enforce the no-platform-conditional rule?
7. Which wasm target is normative, and does WASI count as native or as a third target?
8. What browser memory ceiling applies, and does it bound page size or scope size first?
