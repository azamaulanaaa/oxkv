# 27. Rule lifecycle

A rule decides which rows a client may see and which writes a shard may accept. A bad rule is an outage or a data leak with a version number. A rule change must carry the discipline of a deploy: an immutable version, a dry run over a pinned sample, a named approver, a canary smaller than the blast radius, a receipt, and a rollback that is a write. The lifecycle must make a bad rule small, visible, and reversible before it reaches every shard.

## What the greater goal demands

Policy must be a runtime input, not a build artifact. A client declares a scope. The authority evaluates scope and validation with one bounded language. If a rule lives only in a binary, every rule change is a release. The business owner cannot change the rule. A scope and receipt must name their policy version.

A bad rule fails in two directions. A write rule fails closed: a deny-all validator or a budget overrun rejects every write, so one tenant's mistake stops unrelated tenants. A read rule fails open: a widened scope materializes rows on replicas, silently and permanently. Rollback stops leakage and cannot recall delivered rows.

Blast radius is tenants times shards times rows times operations. A table rule governs one shard's writes and can reach every tenant in it. A scope rule governs every replica that pins the `policy_version`. The lifecycle must bound that product before activation.

## Required capabilities

**Versioned rules.** A policy version must be immutable, content-addressed data: rules and budget. It must have an id and a content hash. The system must never edit a stored version.

**A catalog table.** The policy catalog must be content-addressed data in the `_catalog` table. A rule change must be a write to the catalog shard, with an idempotency key and a `base_version`. A root policy in the authority must govern the catalog. Otherwise a deny-all rule also denies the write that would roll it back. The manifest declares rule identifiers and selectors. It must not embed a mutable rule body.

**A receipt per activation.** Every activation must return one receipt in Queued, Committed, Rejected, or Conflict. The receipt must name the policy id, both `policy_version` values, cohort, role, and dry-run digest. A stale `base_version` must return Conflict. A change that fails compile, budget check, or dry run must return Rejected with a named reason and the rule id.

**Dry run against a sample.** The system must evaluate a candidate against a sample pinned to a snapshot digest, stratified by tenant and operation. The diff must have a structural half (rules, paths, budget) and a semantic half (scope changes and verdict flips). The diff digest must join the approval. An unpinned sample is inadmissible.

**Staged rollout.** The authority must activate a `policy_version` for one cohort: tenant, scope, or shard. A scope must pin its `policy_version` during sync. A write must resolve its `policy_version` at admission. The authority must serve at most two `policy_version` values of one policy id. A canary cohort must use a `projection_epoch` that includes its `policy_version`. A rollout must have a deadline. A canary over budget must abort.

**Rollback.** A rollback must activate a prior `policy_version`. It must not edit content or require a rebuild. The receipt must record it as a new write.

**Audit.** Every verdict must name its version. The audit must answer which writes a version judged, per shard and tenant. The lifecycle must record every transition.

**A blast-radius bound.** The system must classify a change: read-widen, read-narrow, write-deny, write-allow, or cost. Each class must set a cohort limit, an approver count, and an abort condition. Read-widen must never auto-promote.

## Invariants that must hold

1. Active rule content must be immutable. A change must never edit it.
2. One `base_version` must produce at most one committed activation per cohort. The loser returns Conflict.
3. A verdict must be a pure function of the `policy_version`, row, subject, and projected facts.
4. A `policy_version` must not serve a cohort before dry run and approval are durable.
5. A receipt must name the `policy_version` that judged the write, not a later inference.
6. The catalog must stay writable while policy denies every row.
7. A shard must not hold more than two `policy_version` values of one policy id.
8. A change that spans tables must decompose by table and cohort. Cross-shard mutation must remain rejected.
9. A rollout must terminate. A deadline must promote or abort.
10. A rollback must not depend on the bad `policy_version`.

## Decisions and technical choices

**Version model: in-place, immutable, or forward-fix only.** An in-place edit keeps one live copy. It destroys the audit and the pin, so rollback is impossible. Forward-fix only refuses rollback, so a deny-all rule holds the shard down during the fix. Immutable versions plus a pointer cost storage and indirection. They give diff and rollback. Recommendation: immutable versions with a pointer. Forward-fix stays for additive rules.

**Catalog placement: control plane or shard.** A control plane survives a data-plane fault. It is a second writer that can commit an unused version. The catalog shard reuses one write path and audit. Recommendation: catalog shard, root policy in the authority, no second write path.

**Rollout unit: global, shard, tenant, or scope.** A global flip maximizes blast radius. A shard cohort is coarse for read rules. A tenant cohort matches business ownership and needs version selection. A scope cohort must be declared explicitly. Recommendation: tenant cohort for read-visible and write-deny changes. Scope cohort for query-shaped rules outside a tenant. Shard cohort for projection and cost. No global flip for read-widen or write-allow.

**Dry run: static sample or shadow.** A static sample is reproducible, cheap, and biased. Shadow sees real distribution and doubles cost. Recommendation: static sample at admission, shadow on the canary before promotion. Never promote on shadow alone. A shadow verdict is not a receipt.

**Approval in a business setting.** Four roles: author, table owner, platform operator, security. The author must not self-approve. The table owner approves intent. The platform operator approves cohort, budget, and rollback. Security approves read-widen. The authority must verify a signed claim, not business authority. The receipt records role and subject. An emergency allows one canary approver. Promotion waits for a second review.

**Controller shape.** The lifecycle controller must be a sans-IO state machine. It must take a clock and events as inputs, and return actions: activate, abort, notify. An adapter must perform I/O. The clock input must be recorded, because policy has no clock and rollout windows do.

## Dependencies

- C1 (manifest schema): `manifest_revision`, `manifest_digest`, rule references.
- C4 (policy language): immutable `policy_version` values and budget.
- Versioned writes: `base_version` and Conflict.
- C5 (receipt and error semantics): four states and idempotency.
- A7 (audit envelope): one verdict per write, with the `policy_version`.
- A5 (scope semantics): read effect and sync pin.
- A6 (projection staleness): fact backfill before the dry run.
- A4 (shard map): cohort and catalog placement.
- The business event stream: lifecycle signals to consumers. Metrics use the operational feed.
- Qualification: rule-change parity vectors.

## Acceptance criteria

1. Each activation returns one receipt in one of four states, naming both `policy_version` values, cohort, role, and dry-run digest.
2. A stale `base_version` returns Conflict and activates nothing.
3. A failed compile, budget check, or dry run returns Rejected with a named reason and the rule id.
4. One pinned sample and one candidate produce byte-identical diff output.
5. A canary and a control cohort serve different `policy_version` values, and each receipt names the `policy_version` that judged its write.
6. A rollback is one activation write. It returns Committed and rebuilds nothing.
7. A deny-all validator on one tenant leaves other tenants' writes Committed.
8. A read-widen change with one approver returns Rejected, reason `rule.approval_required`.
9. A third live `policy_version` of one policy id returns Rejected, reason `rule.rollout_limit`.
10. A canary over budget aborts and restores the prior pointer.
11. The audit returns write counts per shard and tenant for any retained `policy_version`.
12. The catalog accepts a rollback while the data-plane policy denies every row.
13. A multi-table change produces one receipt per table.

## What would falsify this design

- A sample misses a defect the canary does not catch. Sample admission is insufficient.
- Approval latency exceeds the outage cost, so operators bypass the gate.
- The two-version bound fails under long canaries. The rollout unit is wrong.
- Rollback needs the bad version. The catalog and root split is wrong.
- The audit cannot reconstruct which writes a version judged.
- A customer spans tenants, so a tenant cohort is no boundary.

## Open questions

- Read-widen is irreversible. Does the lifecycle need a quarantine stage?
- What sample size is enough, and who owns it?
- Does a signed role prove business authority, or does read-widen need a second channel?
- At the deadline, does the system promote, roll back, or strand the tenant?
- Must the controller record its clock input, or is the receipt enough?
- Who orders fact backfill and activation?
- Does approval wait count as Queued, and does expiry become Rejected?
- How is a scope cohort named without changing scope semantics?
- Does a multi-table release need a linking record?
