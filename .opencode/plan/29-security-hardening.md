# 29. Security hardening

Security hardening is the design of bounded trust. Once a rule runs inside the
store, the store is the security boundary for the business. The goal is to make
one hostile client, one malicious rule author, one expensive query, or one
compromised replica a bounded incident with a named outcome and an audit record.
A solo vendor will make mistakes, so the design must contain them and prove what
happened.

## What the greater goal demands

The greater goal puts data on devices the vendor does not control. A replica
holds a scope, an index, and an outbox. A stolen replica is expected. The
authority must treat the replica as hostile and the client as authenticated but
not trusted. Audit evidence must survive device loss.

A query is the unit of ownership, so a query is an attack surface. A scope can
request a broad materialization, a deep `CostCursor`, or shard fan-out. The 1.0 search language has no regular expressions. One
policy language governs scope and validation, so a policy author is privileged.
A wrong or hostile rule can widen a scope, approve a forbidden write, or consume
the budget. Clients never write, but they submit requests. Authentication
establishes who asks. Policy decides whether the request is allowed.

The threat model has four adversaries. A hostile client tries to exceed scope,
replay a request, forge a subject, or enumerate rows. A malicious policy author
tries to widen access, exfiltrate through a fact, or make evaluation expensive.
A resource-exhaustion query is legal in shape and pathological in cost. A
compromised replica holds a copied scope, a tampered index, a replayed outbox,
and a credential. A solo vendor cannot watch a dashboard at 03:00. Controls must
fail closed, alarms must be automatic, and evidence must be exportable.

## Required capabilities

- **A policy sandbox with step, memory, and depth budgets.** Evaluation must be
  total, deterministic, and free of ambient authority. It has no clock,
  randomness, network, storage, or host call. A step budget counts abstract steps, so
  native and wasm32 agree. A fixed arena and stack cap bound memory. A panic
  boundary turns a host defect into a denial.
- **Query budgets.** The planner must cost a query before it reads and charge it
  during execution. Caps cover shards, rows, bytes, `CostCursor` depth, subscriptions,
  and diff rate. The 1.0 search language has no regular expressions.
- **Policy fuzzing.** A grammar-aware generator must produce policies, manifests,
  and adversarial rows. The oracle checks totality, determinism, step
  monotonicity, memory bound, and native/wasm parity. A test-only in-tree evaluator
  is the differential oracle. The corpus runs in CI.
- **Client authentication separate from rules.** A sans-IO seam must verify a
  credential and derive a subject before policy runs. A signed request binds the
  body, idempotency key, `base_version`, nonce, and expiry. Revocation has a
  staleness bound. The device key stays outside the scope database.
- **Audit-log integrity.** The authority must record every write enforcement verdict. Read scope evaluation writes to a separate access log, not the write audit chain. Records form
  a hash chain per shard, with signed checkpoints and an asynchronous external witness. A
  receipt names the audit sequence and hash. Export verifies offline.
- **Secret handling.** The manifest and policy name key ids, never key material.
  Policy has no secret access. Audit, errors, and traces carry fixed tokens.
  Snapshots encrypt at rest. Debug output and core dumps are disabled.

## Invariants that must hold

1. Authentication precedes authorization. No rule runs before a verified subject
   is known.
2. Evaluation is total, deterministic, and free of ambient authority. It reads
   one row, one subject, and one pinned fact projection on both targets.
3. The step budget and memory are hard bounds. Exhaustion denies, never allows.
4. No query reads beyond its declared work budget.
5. A client filter may narrow a scope. It may never widen the policy predicate.
6. A fact change requires a trusted write. A client cannot write a fact.
7. One write verdict, one audit record. A read scope verdict writes to the access log. A verdict without a record must not stand.
8. An unauthenticated request gets no receipt. A denied request gets `Rejected`.
9. Secrets never enter policy, audit, error text, or traces.
10. The authority is the only writer. Cross-shard mutation is rejected with a
    named error.

## Decisions and technical choices

**Interpreter with a step budget (recommended).** A closed grammar makes termination and
cost provable. One evaluator serves native and wasm32, and the compiler knows
which facts a rule reads. The cost is containment: a memory-safety defect can
compromise the authority. Mitigations include no `unsafe`, a panic boundary,
fuzzing, and process hardening.

**WASM sandbox.** WASM provides a linear memory cap, no imports, and an engine step budget.
It is stronger for arbitrary code and weaker for policy. The C4 language
becomes a runtime ABI. Step accounting, float behaviour, and SIMD can differ across engines. Static
fact analysis is lost, and two engines double the trusted base.

**Separate OS process with seccomp.** Strongest containment, but IPC cost, no
browser parity, and more operational surface. It contains a compromised runtime,
not a wrong rule.

Recommendation: ship the interpreter and harden the authority process with
seccomp, no shell, a read-only filesystem, and an egress allowlist. Reserve WASM
for post-1.0 untrusted extensions. The test-only evaluator is the differential
oracle and satisfies the two-implementation rule.

Authentication must be a sans-IO seam with two in-tree verifiers, HMAC and
Ed25519. The authority verifies the signature, not a proxy header. Tokens must
be short-lived, and revocation must be a signed list with a published bound.

Audit must be a per-shard hash chain with a signed checkpoint. A local chain
alone is tamper-evident but truncatable. An asynchronous external witness, such as a
customer-controlled bucket or RFC 3161 timestamp, must be accepted. The commit path must not wait for the witness.

## Dependencies

- **C1 (manifest schema).** Field types, secret annotations, auth per operation.
- **C4 (policy language).** AST, step budget, profiles, test-only oracle.
- **C5 (receipt and error semantics).** The reason set must include `auth.forbidden`,
  `policy.budget_exhausted`, `query.budget_exceeded`, and `audit.unavailable`.
- **12, enforcement and audit.** Pinned `policy_version`, one record per write verdict,
  fact version distance, and the separate read access log.
- **A5 (scope semantics), C3 (sync wire protocol), and 15, replica.** Subscription auth, device keys,
  and query budgets.
- **A6 (projection staleness).** Trusted fact writes and provenance.
- **19, 20, and 25.** Fan-out cap, `CostCursor` depth, frame limits, replay defense.
- **27, 28.** Canary rollback, quotas, audit export.
- **30.** Fuzz corpus, parity, tamper tests.

## Acceptance criteria

1. A policy that names a clock, randomness, network, storage, or another row
   fails to compile.
2. A write that exhausts its step budget returns `Rejected` reason `policy.budget_exhausted` and is
   not retryable. A read excludes the row and increments
   `policy_budget_exceeded_total`. An arena overrun is denied.
3. A query above the shard, row, byte, or `CostCursor` cap returns `query.budget_exceeded` and
   reads no row beyond the cap.
4. The planner rejects a regular expression at plan time with `policy.regex_not_supported`. The
   authority never backtracks, because the 1.0 search language has no regular expressions.
5. The fuzz target completes its CI budget with no panic, no timeout, and no
   native/wasm difference. Every golden vector agrees on both.
6. An unauthenticated request returns `auth.unauthenticated` before policy runs.
7. The authority refuses a replayed request after its nonce or expiry. A retry
   with the same idempotency key returns the original receipt.
8. A revoked credential cannot sync or write after the revocation staleness bound.
9. Every write verdict has one audit record, and the chain verifies to the checkpoint. A read scope verdict writes to the access log, not the write chain.
   A failed audit append rejects the write with reason `audit.unavailable`. A
   changed byte or missing record fails verification.
10. A canary secret does not appear in the manifest, policy, audit, logs, errors,
    or traces.
11. The authority refuses a policy publish above the step budget on the
    adversarial corpus.

## What would falsify this design

- A rule that is total, cheap, and still leaks rows through an allowed fact. The
  sandbox would be sound and the authorization model wrong.
- A legal query whose worst-case cost cannot be bounded. Plan-time budgets would
  be a broken promise.
- A step model that produces different costs on native and wasm32. Determinism
  would be a claim, not a property.
- A memory-safety defect that escapes the evaluator and the panic boundary.
  Interpreter containment would fail.
- An audit chain that an operator can truncate without detection.
- A stolen replica that syncs after the revocation bound with a long-lived token.
  The authentication model would fail the stated threat.
- A policy that passes fuzzing and review and still grants a cross-tenant write.
  Fuzzing would test implementation, not intent.

## Open questions

- Is the external audit witness mandatory or optional? Who operates it?
- How are rate-limited requests audited without an audit-log denial of service?
- May a failed audit append stop all writes? Is a bounded local spool acceptable?
- Does seccomp belong in 1.0 if it breaks single-binary deployment or wasm
  parity?
- Should read verdicts be constant-time to hide row content from timing?
- Should an offline replica expire its scope after a fixed number of days?
- Who reviews a customer policy when the vendor has no second reviewer? Is a
  linter report enough?
- Can a tenant administrator author rules? If yes, what separation of duties
  prevents self-grant?
