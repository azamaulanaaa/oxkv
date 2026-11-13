# 8. Policy language v1

A query is the unit of ownership, and a policy expression makes ownership
checkable. One language must answer two questions that must never drift apart:
*may this replica hold this row?* and *may this write become this row?* The same
expression, interpreter, budget, and versioned facts must answer both. Separate
evaluators would let a client hold data it cannot update, or let an authority
accept data outside the admitting scope. Both are silent corruption, invisible in
a receipt.

## What the greater goal demands

A replica holds only its scope, so scope must be a portable predicate: serialized,
versioned, replayable. The authority evaluates it, the replica proves what it
holds, and sync diffs scopes. An unreplayable predicate is not a scope.

Writes are requests, and the authority validates each one against rules. A second
mechanism diverges at the first edge case, and the divergence is a correctness
hole. Projection also requires that the facts a rule reads travel to the shard
that evaluates the rule, so a checked write stays single-shard. A language with
ambient authority cannot declare its inputs, because it has none.

C4 (policy language) freezes the AST, evaluation semantics, and cost model at 1.0. Meaning must be pinned across native and wasm32,
across nodes, and across years. A contract that only one binary defines is not a
contract. It is merely stable.

## Required capabilities

The language must express tenancy, ownership, field-level constraints, region,
role, and version preconditions. It must not express computation.

- The grammar must provide paths, literals, comparison, boolean connectives,
  membership, and existence checks. It must provide nothing else in 1.0.
- The language must be total. Every expression must return a verdict for any input
  in its declared domain, or a named budget failure.
- The evaluator must be pure. It must not read a clock, draw randomness, perform
  I/O, inspect the host, or keep state.
- The evaluator must be deterministic. Equal expression version, fact set, and
  input must produce equal verdicts.
- Every evaluation must carry a step budget and a depth budget. The budget belongs
  to the policy version, not a runtime setting.
- The parser must be closed. A document outside the published grammar must be
  rejected with a named error.
- Policy must be stored as versioned data. Every verdict must name the policy
  version and the fact version that produced it.
- One evaluator must serve read scope and write validation. A test-only differential oracle may exist outside the production build. No second production evaluator may exist in any layer.
- Evaluation must be per row. A batch verdict must be the conjunction of the row verdicts. If one row rejects, the batch rejects and changes no row.
- The evaluator must read one pinned fact projection. It may read a bounded set of facts from that projection.
- The interpreter must be sans-IO and must produce identical results on native and wasm32.
- Evaluation must produce a structured explanation, not only a boolean.

## Invariants that must hold

**Single evaluator.** Any authorization path must call one entry point. A shortcut
is a second language. A test-only differential oracle may run in tests, fuzzing, and qualification. It must never enter a production authorization path.

**Verdict closure.** A verdict is a pure function of the `policy_version`, the
fact versions, and the input value. Nothing else may enter.

**Budget totality.** No expression may run without a budget. Exhaustion must
produce a named terminal failure, never a default allow or deny, because a silent
default hides the attack.

**No ambient authority.** An expression may name only what the context binds.
There are no globals, no imports, and no environment.

**Scope–validator coherence.** Any committed value must pass the scope predicate
that admitted the replica, under the same policy version. This must hold
structurally, not by convention.

**Audit sufficiency.** One audit record must let a reader replay the verdict
without reading the store.

**Additive stability.** Additions must be additive. An expression keeps its meaning
for 1.x. A meaning change requires a new language version and a migration.

## Decisions and technical choices

### Decision 1 — language family

**General scripting language.** Disqualifying, and not for performance. It has
unbounded execution and allocation, ambient authority, mutable state between
calls, and implementation-defined number and string semantics. A runtime release
defines its meaning, so an upgrade can change a stored verdict and break C4 and the audit trail.

**CEL-like.** The semantics are published, the language is non-Turing-complete,
and it ships a cost model. But the project does not own the specification. CEL's
type system, macros, protobuf bindings, and time types are larger than this goal
needs, and each adds an ambient concept C4 must tolerate.

**Bespoke, CEL-shaped.** Own a small typed grammar, borrow the structural
discipline, and publish the evaluation rules as the contract. The cost: write the
specification, parser, conformance suite, and fuzzer. Under-design is worse, because
a missing operator forces a second evaluator.

**Sandboxed WASM.** Strongest isolation and a mature runtime, but it fails the
other requirements. A step budget per instruction is neither human
meaningful nor cost-stable across runtimes. Floating point and memory growth vary
by host. The artifact is opaque binary, so an auditor cannot diff a rule change.
Worst, policy stops being data and becomes a program with compiled dependencies,
which makes projection and rollout intractable.

**Recommendation:** bespoke and CEL-shaped, with no loops, no user functions, and
no arithmetic beyond bounded integer comparison. The surface is roughly eight node
types: small enough to fuzz to a fixed budget and to reimplement independently.
Later, every feature must pass one test: does this need a new node type? A yes is
a version bump.

### Decision 2 — one expression or two

**Recommendation:** one expression, one context shape with `before` and `after`.
Read scope evaluates with both bound to the same row. Write validation evaluates
with `before` as the stored value and `after` as the proposed value. Coherence
holds by construction: the read case is the write case under a degenerate
transition. Separate scope and validator expressions are more expressive, but
coherence becomes a proof obligation the compiler often cannot discharge. Authors
must then write a transition whenever read and write shapes differ.

### Decision 3 — budget accounting

**Recommendation:** a step count per node, weighted by input size, plus a fixed
depth cap. Wall-clock deadlines are non-deterministic and disqualified. A runtime-specific budget alone
is deterministic but opaque. Steps let an error name the node and budget. Cost
then tracks input size, so scope queries must be bounded by page size.

### Decision 4 — budget failure semantics

**Recommendation:** fail closed with the named terminal error
`policy.budget_exhausted`, mapped to `Rejected`, never to `Conflict`. Fail-open
would convert a resource attack into an authorization bypass. Partial evaluation
returning "unknown" is unusable: it has no receipt state, and the four states are
closed. The budget must therefore be generous enough that legitimate policy never
exhausts it, so tuning is measured.

## Dependencies

- Manifest v1 must declare policies, field types, and operations, because the
  manifest types the grammar's paths.
- Key encoding v1 must define prefix bounds, because a scope over a range is a
  filter plus a range.
- Receipt and error semantics must reserve the mapping from policy failures to
  terminal states.
- Scope must be defined as a policy expression over this grammar, not as a
  separate construct.
- Policy projection must consume the declared fact set of each expression.
- Rule lifecycle must version, diff, and roll back expressions.
- The qualification suite must hold golden vectors for grammar and evaluation.

## Acceptance criteria

1. The evaluator must return identical verdict bytes on native and wasm32 for every
   vector in the conformance corpus.
2. Every expression must terminate within its declared budget or return
   `policy.budget_exhausted`. No third outcome is permitted.
3. The parser must reject every document outside the grammar and report the byte
   offset of the first error.
4. A fuzzer must run for 24 hours with no panic, no non-total result, and no
   nondeterministic verdict.
5. The manifest compiler must reject a policy that names an undeclared field with
   `manifest.rule_unknown_path`.
6. Every commit must write one audit record with the `policy_version`, fact versions,
   verdict, and explanation.
7. A replica read and an authority write must agree on admission when they use the
   same `policy_version` and equal values.
8. A batch with one rejected row returns `Rejected` and changes no row.
9. The published specification must be sufficient for an independent
   implementation to pass the conformance corpus.

## What would falsify this design

- An isolation requirement the subset cannot express, where the workaround needs a
  second evaluator, falsifies the single-evaluator thesis.
- Budget exhaustion at a rate that makes fail-closed operationally unacceptable
  falsifies the budget model.
- Verdict drift between native and wasm32, or against an independent
  implementation, falsifies the determinism claim.
- A coherence violation between an admitting scope and an accepting validator
  falsifies the one-expression decision.
- A projection that needs more facts than a shard can hold falsifies single-shard
  rule-checked writes.

## Open questions

- Is membership over a literal list sufficient, or does scope need index-backed set
  membership on large tenants?
- Does the language need arithmetic, or is ordered comparison of encoded versions
  enough?
- Are facts flat scalars or nested documents? Nesting complicates projection and
  the audit record.
- How does a rule observe absence for a tombstone without a separate existence
  node?
- What is the migration path for a semantic bug fix when replicas pin an older
  policy version?
