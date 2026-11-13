# 12. Enforcement and audit

A client is untrusted, and its write arrives late. The right to write can disappear while the request waits. Enforcement must therefore happen at commit, against a policy version the authority pins and records. The verdict must be replayable from durable evidence, and an auditor must review it without access to protected data. A verdict no one can replay and no one can review is not enforcement.

## What the greater goal demands

The authority owns truth and serializes writes. A replica is read-only. A write is a request with an idempotency key and a base version, ending as Queued, Committed, Rejected, or Conflict. Every authorization decision lands on the authority, at the only moment it can bind: the commit.

One deterministic policy language governs scope and validation, and both share one evaluator. A second evaluator would let a write pass validation and then fall outside the scope that replicates it. Enforcement must use that evaluator and those rule facts.

The business event stream is the only path to write side effects. Enforcement may write an audit record inside the transaction it governs. It must not call a mail server or remote collector on the commit path. Audit export reads the operational feed.

C4 (policy language), C5 (receipt and error semantics), and C1 (manifest schema) freeze at 1.0. A7 (audit envelope) freezes with them, because an auditor reads it years later without the process that wrote it. A6 (projection staleness) freezes with the manifest.

## Required capabilities

1. **Pinned evaluation version.** Every verdict must name the `policy_version`, `manifest_digest`, `projection_epoch`, and fact versions that produced it, recorded with the verdict. The request must not choose any of them.
2. **Declared authorization staleness bound.** The manifest must declare, per table or rule class, the maximum fact version distance per fact source that a write may rely on. Enforcement must compare measured version distance at commit. Time is monitoring data only and must not enter a verdict.
3. **Strict mode for revocation-critical writes.** A rule or table must be able to declare strict authorization. Strict mode must fail closed when facts exceed the bound, the pinned `policy_version` cannot resolve, or the projection is incomplete. The receipt must be `Rejected` with `authorization.stale_facts`. A non-strict write over the bound must stay `Queued` with `projection.lagging` until the authority queue deadline, then become `Rejected` with `write.queue_timeout`.
4. **One audit record per write request.** Each request must produce exactly one durable record with a bounded decision list. Each entry names the row key, rule identity, verdict, and reason code. The record carries request identity, idempotency key, principal, shard, base version, `policy_version`, `manifest_digest`, map version, `projection_epoch`, fact versions, input digest, previous hash, and record hash. The record must let an auditor re-evaluate offline.
5. **A retention rule.** Every rule class must declare a retention class. The floor must cover the longest time a justified write can affect state, plus one incident window. Expiry must be explicit and audited, and a legal hold must override it.
6. **Audit visibility that follows policy.** Audit queries must pass through the same evaluator as any other read. A record must not reveal a field value, a rule body, or its own existence when policy forbids it. Reason codes must be coarse and stable, and export must be filtered.
7. **Principal authentication distinct from policy.** Enforcement must know who asked. Authentication must not live in the rule language, and a rule must not impersonate another principal.
8. **Receipt linkage.** A terminal receipt must identify the audit record that justified it, so a client can cite it in a dispute.
9. **A separate read access log.** Read scope evaluation must write to the access log, not the write audit chain. The access log has its own retention class. An offline replica can evaluate reads without the authority.

## Invariants that must hold

- No committed state change may lack an audit record for the verdict that permitted it.
- A verdict must be a pure function of the request, `base_version`, pinned `policy_version`, rule facts, and fact version distance. Replay must produce the same verdict.
- A `policy_version` must be immutable and must stay resolvable for at least the audit retention floor.
- The fact version distance bound must never be exceeded silently. Exceeding it must produce a named outcome and an audit record.
- A client must not influence the pinned `policy_version`, the principal identity, or the verdict.
- Audit data must not grant read access to the data it describes, nor confirm existence beyond what policy allows.
- Enforcement must produce no side effect other than the audit record and, after commit, the business event, and no network I/O on the commit path.
- A cross-shard mutation must fail with its named error before policy evaluation. Enforcement must not partly evaluate a write that cannot commit atomically.

## Decisions and technical choices

**Evaluate at enqueue, at commit, or at both.** Enqueue evaluation gives fast feedback and protects the queue from work that can never succeed. Its verdict is advisory, because authorization can change while the request waits, so it cannot answer a security review. Commit evaluation is authoritative, sees the final base version, and runs under shard serialization. Its cost lands only on writes that reach commit, but a hostile client can fill the queue with requests that will fail. Evaluate at both. Enqueue is an admission filter with an advisory verdict. Commit re-evaluates unconditionally and records the binding verdict. Both share one evaluator and one fact projection, and only the commit verdict is authoritative on the receipt.

**Pin at commit, filter at enqueue.** Enqueue may use the current `policy_version` to reject obvious violations. Commit must pin the `policy_version` it evaluated and record it. Reusing an enqueue pin would let a revoked grant survive the queue delay.

**Measure version distance outside the evaluator.** The policy language must have no clock, randomness, or I/O. The authority must read the fact version distance and pass it in explicitly, which keeps the evaluator deterministic and clock policy in one visible place.

**One record per write request, in the same transaction.** The audit record must commit atomically with the state change it authorizes. Where a shard cannot pay that cost, a write-ahead audit outbox with gap detection is the alternative, and the invariant becomes "no visibility without a record." That option must be a declared, measured choice.

**Append-only and hash-chained per shard.** Records must be ordered per shard and chained by hash, with periodic checkpoints. This gives tamper evidence without a second consensus problem. Expiry may break the chain only at a recorded checkpoint.

**Non-leakage by construction.** The envelope must carry reason codes, not messages built from data values. A denied read of a record must be indistinguishable from an absent record unless policy says otherwise. Aggregate views must bucket counts, so a count cannot confirm a hidden record.

## Dependencies

The manifest must declare fact version distance bounds, strict mode, retention classes, and audit visibility. The policy language must define the deterministic evaluation enforcement calls. Versioned writes supply the `base_version`. Receipts carry terminal states and the audit reference. The scheduler sets queue latency and request staleness. Policy projection must expose fact versions and completeness. The shard map and single-shard transaction boundary decide when enforcement may run. The operational feed carries audit export. Rule lifecycle must treat a rule change as an audited write. Security hardening supplies principal authentication and export control.

## Acceptance criteria

- Revocation between enqueue and commit must yield no state change, a `Rejected` receipt naming the pinned `policy_version`, and exactly one audit record.
- A strict-mode write past the bound must be rejected with `authorization.stale_facts`, and the record must show the measured fact version distance.
- A non-strict write over the bound must stay `Queued` with `projection.lagging` until the queue deadline, then become `Rejected` with `write.queue_timeout`.
- Replaying an audit record must reproduce the verdict on golden vectors.
- A principal without a grant must receive no audit records, and the response must not reveal whether matching records exist.
- A fault after commit and before receipt delivery must leave the record durable and exactly one record per verdict.
- Two in-tree verdict implementations must agree on the golden vectors, including staleness and strict-mode cases.
- Retention expiry must require an audit record and must not remove a record under legal hold.
- A cross-shard mutation must fail before evaluation and leave no verdict record.

## What would falsify this design

The design fails if a committed write cites a policy version other than the one evaluated. It fails if a strict-mode revocation takes effect only after a commit. It fails if an audit query returns data the normal query path denies. It fails if replay yields a different verdict, or two evaluator implementations disagree. It fails if the staleness bound is exceeded without a named outcome. It also fails if cost per verdict grows with audit history, because it makes retention a performance decision.

## Open questions

- Does the pinned version resolve from a global catalog or the per-shard projection, and how does an auditor resolve it after compaction?
- Is the audit record in the shard transaction or a write-ahead outbox with gap detection?
- Must a denial hide a record's existence, and what can a rejected client be told that still lets it repair its request?
- How does the authority measure fact version distance without a trusted clock, and what is the declared behavior under clock skew?
- Is strict mode per rule, per table, or per field, and who may change it?
- Can audit records batch without breaking one-record-per-verdict, and what is the measured write amplification?
