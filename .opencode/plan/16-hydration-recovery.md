# 16. Hydration and recovery

A replica that returns after days offline must resume where it stopped, verify every byte before visibility, and cross from ScopeSnapshot to diff tail at one version without a gap. Hydration is a resumable, verifiable, interruptible state machine. Recovery is the proof that every interruption leaves a state the replica can name and continue from.

## What the greater goal demands

A query is the unit of ownership. A client declares a policy-filtered scope, matching data materializes locally, and the replica reads and searches offline. Authority owns truth, serializes writes, and the replica never writes.

Hydration must hold that contract under three stresses: a large scope, an offline period longer than any buffer, and power loss at any byte. A full resync after each interruption turns offline use into a demo. A silent gap makes local answers wrong in a way the caller cannot detect.

C2 (snapshot and page format), A12 (migration and backup), and C3 (sync wire protocol) raise the cost of a late repair at 1.0. Snapshot, page, and wire formats must carry resumption and integrity from the first version. The bytes must say what they are, which generation they belong to, and how to check them.

## Required capabilities

**Scope descriptor.** Hydration names the `manifest_digest`, `policy_version`, `map_version`, ScopeSnapshot identity, base scope version vector, and page index. A client rejects a ScopeSnapshot whose descriptor differs from its scope.

**Resumable transfer.** The authority serves the ScopeSnapshot as ordered pages, each with `page_id`, `page_sequence`, byte range, and digest. A client resumes at the first page it has not verified.

**Verification before use.** A page passes its digest before decode. The ScopeSnapshot passes a `content_digest` and a coverage check before any part becomes visible. Policy membership is rechecked against the pinned `policy_version`. Repeated failure is a terminal, named error.

**Diff tail with a fixed start.** The authority retains diffs from the ScopeSnapshot base for a declared retention window. The client reads the tail only after it holds the base. Frames carry `commit_version` and sequence numbers.

**Atomic handoff.** Hydration builds a new generation beside the served one. Publication swaps one pointer at one version. A reader sees old or new, never a mixture. Prefetched frames wait for a complete, verified ScopeSnapshot.

**Gap and duplicate prevention.** The published scope version vector advances only across a contiguous prefix of applied frames. A frame at or below the watermark is a duplicate and is discarded. A missing sequence number forces a re-request from the last applied sequence. The client never skips a frame.

**Crash recovery at each step.** A durable journal records phase, ScopeSnapshot identity, verified pages, applied watermark, and the swap commit. Every transition is idempotent. Restart resumes at the last durable boundary.

**Declared cold-start budget.** The budget covers time to first readable version, time to full convergence, and bytes transferred. It scales with scope size and offline delta, not database size. A full ScopeSnapshot is the worst case and must be named. The scope class must declare `max_hydration_overhead`. A scope that cannot fit the peak must fail at subscribe with `scope.hydration_budget_exceeded`.

**Visible progress.** The replica exposes phase, verified bytes, published version, lag, and target version.

## Invariants that must hold

- **No regression.** A served read scope version vector never decreases.
- **No gap.** At scope version vector V, the local key set must equal the authority's policy-filtered state at V for the declared scope, given verified base S and a contiguous tail from S to V.
- **No resurrection.** A key deleted in the tail after the ScopeSnapshot stays absent. Apply is version-idempotent, so replay yields the same state.
- **Atomic transition.** A crash leaves either the old generation or the new one. No frame applies beyond the durable watermark.
- **One writer.** Hydration produces no mutation and no receipt.
- **Determinism.** The same ScopeSnapshot and tail produce byte-identical state, including index bytes, on native and wasm.

## Decisions and technical choices

**ScopeSnapshot-then-tail is the chosen order.** The replica transfers the ScopeSnapshot at base S, verifies it, then applies the tail (S, V]. The authority pins retention at S for the transfer. The client may prefetch frames, but application stays ScopeSnapshot first, tail second.

**Tail-then-ScopeSnapshot is rejected.** That order streams live diffs from now, then fetches a ScopeSnapshot from an earlier base. It must buffer every diff until the base arrives, so the buffer grows with ScopeSnapshot duration, not offline duration. No consistent state is publishable until the ScopeSnapshot lands, and the no-gap proof rests on an unbounded buffer. When transfer dominates, the answer is a faster ScopeSnapshot.

**Retention decides resume versus resnapshot.** The handshake publishes the retention window. If the last served scope version vector S is at or above the oldest retained diff, the replica resumes with missing pages plus the tail. If S is older, the authority returns `sync.snapshot_unavailable`. The replica hydrates a fresh ScopeSnapshot into a new generation and keeps the old generation readable until publication. This is the only permitted full resync, and it is explicit.

**One generation swap makes recovery simple.** An append-only log with a visibility watermark must order data, index, and the watermark. A crash can leave the watermark ahead of the index. A root pointer plus a durable commit record reduces the question to whether the swap happened.

**Version arithmetic orders the tail.** Clocks skew, especially after a long sleep. `commit_version` and sequence numbers are the only ordering authority. Deduplication uses the applied watermark plus a small per-key last-`commit_version` map.

**The content seal is separate from the authority lease.** A ScopeSnapshot is valid when its `content_digest` verifies and its `content_seal` verifies. The `lease_term` must not enter the `content_digest`. A sealed ScopeSnapshot remains valid across an authority lease change. The `lease_term` fences new writes and unsealed in-flight pages only. A client renegotiates when the map changes, not when the `lease_term` changes.

**A policy or manifest change restarts hydration.** A scope is defined against one `policy_version` and one `manifest_digest`. A mid-flight change makes old ScopeSnapshot and old tail incomparable. The replica discards the new generation and re-declares the scope. The served generation stays readable.

## What a reader sees during hydration

On a hydrated replica, reads continue on the old generation at its last published scope version vector. Reads are monotonic against that scope version vector. Search answers at the same scope version vector, so get and search never disagree.

On a cold replica, a scope read returns an explicit "not hydrated" status with progress, not an empty result set. A caller cannot mistake "not loaded" for "no rows".

At handoff, a reader sees one version step: old version, then new. The step equals the accumulated tail.

## Dependencies

- A1 (key encoding) and shard identity define the page key space and per-shard routing.
- C2 (snapshot and page format) defines header, `snapshot_page_version`, `content_digest`, provenance, and page boundaries.
- C1 (manifest schema) and C4 (policy language) define the scope, its index layout, and the pinned `policy_version`.
- C3 (sync wire protocol) defines `SessionHello`, resume, and ScopeSnapshot fallback.
- The client replica defines monotonic reads, lag, and the no-write rule.
- Tiering and the authority lease define ScopeSnapshot location and the serving `lease_term`.
- Snapshot-consistent search defines index visibility at publication.
- The qualification suite runs the crash matrix, and falsifier benchmarks measure the cold-start budget.

## Acceptance criteria

1. A crash at any phase boundary and at random byte offsets within a page converges to the same state after restart.
2. A reader loop during hydration observes a non-decreasing version and never sees a key set that violates scope or the no-gap property. A duplicate frame leaves state unchanged, and a missing sequence number produces a re-request.
3. A replica offline within retention transfers only missing pages and the tail, and converges without a full ScopeSnapshot.
4. A replica offline beyond retention receives the named ScopeSnapshot outcome, keeps the old generation readable, and publishes only after verification.
5. A corrupt page is rejected and re-fetched and never becomes visible. Persistent corruption ends in a named terminal error with no partial publication.
6. A crash during the swap leaves exactly one published generation.
7. A policy or manifest change during hydration restarts hydration and never mixes identities.
8. The measured cold start meets the declared budget per scope class, and native and wasm produce the same post-hydration state hash.

## What would falsify this design

- If most offline windows over one day require a full ScopeSnapshot, the no-full-resync claim fails.
- If ScopeSnapshot size grows with database size rather than scope size, the cold-start budget is unreachable.
- If idempotent apply needs unbounded dedup state, the duplicate rule does not scale.
- If atomic handoff requires blocking reads, the availability claim fails.
- If double-generation storage exceeds the device budget for a large scope, that class is unsupported.
- If the crash matrix finds one interleaving where a reader sees a hole, the no-gap invariant is false.

## Open questions

- What retention window does 1.0 promise, and who pays, the authority or the resnapshotting replica?
- Is retention per scope or per shard, and can replicas share retained diffs?
- What are the numeric cold-start budgets, and which scope classes are declared?
- For a cross-shard scope, is the visible version the minimum across shards, or does each shard publish alone with an explicit pin?
- How much dedup state is acceptable per key, and for how long?
