# 32. 1.0

1.0 is a freeze of C1-C5 and A1-A12, not a feature count. It fixes the three things a consumer
cannot rebuild: the declarative surface, the bytes, and the meaning of a write
outcome. The Rust API stays free to change. A scope declared at 1.0 keeps its
meaning, and a snapshot written at 1.0 keeps its data, for the life of the major.

## What the greater goal demands

Oxkv treats a query as the unit of ownership. A client declares a
policy-filtered scope. The authority evaluates it. Matching data materializes
locally. The client reads and searches offline, then syncs incrementally.

Every step crosses a boundary we do not control. A browser bundle
ships once. A snapshot sits in object storage for years. An outbox entry waits
for a receipt. None can be rebuilt on the release schedule.

A write is a request. It carries an idempotency key and a base version. Its
receipt has four lifecycle states, three of them terminal, and retry logic reads
them.

One policy language governs read scope and write validation. A replica and an
authority that disagree diverge silently, so determinism is a contract.

Every frozen surface crosses the boundary: C1 manifest schema, C2 snapshot and page format, C3 sync wire protocol, C4 policy language, C5 receipt and error semantics, and annexes A1 key encoding, A2 tokenizer, A3 event envelope, A4 shard map, A5 scope semantics, A6 projection staleness, A7 audit envelope, A8 cost and explain, A9 cursor types, A10 descriptor, A11 read pin and staleness, and A12 migration and backup. 1.0 freezes those surfaces. The Rust API stays free: it
compiles into the consumer's build, so a change costs one recompile. The frozen surfaces of C1-C5 and A1-A12 travel in artifacts that never recompile.

## Required capabilities

The release MUST provide:

| Contract | Register parts | Stays free |
| --- | --- | --- |
| C1 Manifest schema | Table names, key schema, fields, indexes, rule metadata, facts, operations, events, receipts, audit, projection, codecs, control | Rust builders, generated code, layout |
| C2 Snapshot and page format | Header, provenance, page identity, page sequence, checksums, framing, encoding, key order | Codec named in the header, block size, cache |
| C3 Sync wire protocol | Framing, `SessionHello`, subscribe, diff, cursor, resume, flow control | Transport, authentication, batching |
| C4 Policy language | AST, evaluation order, determinism, step budget, explanation | Source syntax sugar, diagnostics |
| C5 Receipt and error semantics | `Queued`, `Committed`, `Rejected`, `Conflict`, idempotency, retry class, error registry, receipt envelope | Scheduler, outbox storage, logs |
| A1 Key encoding | Version byte, table grammar, component tags, system marker, order, bounds | Table set and schemas |
| A2 Tokenizer | Identifier, version, Unicode data, normalization, case folding, segmentation | Ranking parameters |
| A3 Event envelope | Envelope fields, event identity, ordering, retention | Consumer library |
| A4 Shard map | Routing, table assignment, bounds, version, digest | Placement control plane |
| A5 Scope semantics | Identity, version vector, diffs, removal reasons | Scope composition |
| A6 Projection staleness | Version-distance bound, epoch, watermarks | Numeric limits |
| A7 Audit envelope | Record fields, hash chain, checkpoints | Export tooling |
| A8 Cost and explain | Explain schema, cost error names | Weights and estimates |
| A9 Cursor types | Token formats, fields, expiry, errors | Server-side state |
| A10 Descriptor | Method, message, field, code mapping | Language bindings |
| A11 Read pin and staleness | Pin modes, session vector, staleness errors | Numeric bounds |
| A12 Migration and backup | Backup manifest, restore steps, migration steps | Tooling and scheduling |

1. **A complete versioned register.** C1-C5 and A1-A12 each carry a version, a spec, a
   changelog, golden vectors, and a migration path, from the first commit.
2. **A complete manifest.** It declares user table names, key schemas, fields, indexes,
   rule metadata, facts, operations, events, receipts, audit, projection, codecs,
   and control. It compiles to a neutral descriptor, storage layout, index config,
   scope, validators, and generated RPC.
3. **A self-describing format.** A header, a `snapshot_page_version`, checksums, and a
   named error for an unknown version.
4. **A sans-IO sync protocol.** `SessionHello`, scope subscription, a diff
   stream with a `SyncCursor`, resume after disconnect, and ScopeSnapshot fallback.
5. **One bounded policy language.** Paths, literals, comparison, boolean,
   membership, and existence. It is deterministic, has no clock, randomness, or
   network, and is stored as versioned data.
6. **Request-shaped writes.** An idempotency key, a `base_version`, per-key FIFO
   ordering, and four lifecycle states. `Queued` is live and not terminal.
   `Committed`, `Rejected`, and `Conflict` are terminal.
7. **Forward migration.** One migration framework runs on open, under the authority lease, before
   the first read. It verifies checksums, switches the manifest in one CAS,
   resumes after interruption, and never runs backward.
8. **A deprecation and support window.** A reader accepts the current format and
   one back. A format is removed only at a major, after twelve months of notice,
   and only with an automated migration. The 1.x line receives fixes for 24
   months after the next major. The last minor receives security fixes for 12
   months, and two minors stay supported. The wire keeps one extra read version
   for a major, because a tab cannot be force-upgraded.
9. **The wave sequence.** The work follows the wave table and dependency rules in
   `specs.md` sections 7.24 and 10.1. A wave gate must not require a later wave.
10. **The full version axes from section 8.2.** The product axis is `crate_version`. The contract axes are `key_encoding_version`, `snapshot_page_version`, `sync_wire_version`, `manifest_schema_version`, `policy_ast_version`, `receipt_error_version`, `descriptor_version`, `map_schema_version`, `tokenizer_version`, `event_version`, `audit_version`, `scope_version`, `projection_version`, and `cost_version`. The data axes are `commit_version`, `key_version`, `snapshot_version`, `scope_version_vector`, `projection_epoch`, `fact_version`, and `statistics_version`. The identity axes are `manifest_revision`, `manifest_digest`, `policy_version`, `map_digest`, and `descriptor_digest`. The fence axis is `lease_term`. The session axes are `session_minimum` and `replica_high_water`. Semver assumes a rebuild. Stored bytes and deployed clients do not, so they carry their own qualified versions.

## Invariants that must hold

- A 1.x reader reaches the same policy verdict, receipt state, and page boundary
  as every other 1.x reader.
- No migration loses a committed key.
- An interrupted migration leaves the previous epoch readable, and the next open
  restarts it.
- An unknown `snapshot_page_version` is refused before any record applies.
- No C1-C5 or A1-A12 contract depends on a Rust type layout, a serde default, or an enum
  discriminant.
- Every seam has two in-tree implementations. Golden vectors run against both.
- A cross-shard mutation returns `shard.cross_mutation_rejected` and applies nothing.
- A receipt has four lifecycle states, three of them terminal. `Queued` is live.
  None is added or reused inside 1.x.
- A rule change is a write with a receipt and an audit record.

## Decisions and technical choices

**Freeze C1-C5, A1-A12, and the Rust API.** Freeze the bytes, the declarative surface, and the Rust
API together. This is the strongest promise. It also makes API mistakes permanent for
the life of 1.x. Rejected: the cost lands on every later release.

**Freeze C1-C5 and A1-A12 only.** Keep formats stable and let the manifest, policy, and
wire move. Data survives and internal design stays flexible. But a deployed
browser cannot follow a grammar change, and an old receipt cannot be read by a
new server. Rejected.

**Version every surface and adapt.** Version everything and adapt across every combination. The
test matrix becomes a product of version axes, the vectors multiply, and no
client can rely on behavior. Rejected: an unbounded support matrix is a second
product.

**Freeze C1-C5 and A1-A12 (recommended).** Every register row
gets a version, a spec, and vectors. The Rust API gets stability attributes and a
documented escape hatch.

**Evolve by forward migration (recommended).** One current reader plus one
read-only legacy reader keeps the matrix small. A reader per format is simpler
and grows without bound.

**Freeze C4 and its AST (recommended).** The AST is the meaning and
surface syntax is sugar. A second syntax then needs no policy bump. A grammar-level C4 change makes parser details permanent.

### Release checklist

1. Every C1-C5 and A1-A12 row carries a version, spec, and changelog.
2. Golden vectors pass on native and wasm.
3. The qualification matrix passes.
4. A v1 snapshot opens on the release build and the previous minor.
5. An unknown `snapshot_page_version` returns `format.unsupported_version` and applies nothing.
6. A migration test loses no key and resumes after interruption.
7. Every contract has a second in-tree decoder.
8. The deprecation and support policy is published.
9. The falsifiers report measured numbers.

## Dependencies

- A1 (key encoding) and C2 (snapshot and page format) define the bytes. C1 (manifest schema) defines the
  surface.
- C4 (policy language) fixes AST and evaluation.
- Versioned writes and C5 (receipt and error semantics) fix the lifecycle states and the retry class.
- A5 (scope semantics), C3 (sync wire protocol), the replica, and A6 (projection staleness) fix negotiation and resume.
- A3 (event envelope) freezes with C5 (receipt and error semantics).
- Rule lifecycle and operations must land first, because a rule change is a
  versioned write.
- The qualification suite and falsifiers are hard prerequisites.

## Acceptance criteria

1. Any two 1.x readers return the same verdict for the same rule and fact set.
2. A page split at any boundary re-reads to the same key order.
3. A client on the 1.0 wire resumes after seven days offline without a full
   resync.
4. One idempotency key reaches one terminal state. A resubmission returns it.
5. A cross-shard write returns `shard.cross_mutation_rejected` and leaves no partial row.
6. A v1 ShardSnapshot opens on the release build and the previous minor.
7. An unknown `snapshot_page_version` returns `format.unsupported_version`
   and applies no record.
8. A migration interrupted at every step resumes without key loss.
9. A native client and a browser client produce identical results for every
   golden vector.
10. A stable API item never names an unstable type.
11. No downgrade is allowed after the first new-format write.

## What would falsify this design

- A v1 snapshot from one 1.x build fails to open on another.
- A policy verdict differs between native and wasm for one vector.
- A migration loses a key or cannot resume.
- A client on the N-1 wire cannot resume incrementally after a week offline.
- A cross-shard mutation produces a partial commit.
- A receipt reaches a fifth lifecycle state, or one idempotency key commits
  twice.

**What delays 1.0.** The tag waits if any is true:

- A falsifier misses its budget or lacks a measured number.
- A contract has only one in-tree implementation.
- The qualification matrix is missing a scenario.
- Migration from a pre-1.0 store loses a key or cannot resume.
- The deprecation and support policy is unpublished.

## Open questions

1. Is key encoding a sixth contract or annex A1 to C2? It is A1 to C2.
2. Does the support window start at the tag or at the next major?
3. Is the unstable surface a feature flag, a module, or an attribute?
4. Is an additive grammar change a minor release? If the AST does not change,
   yes.
5. Does 1.0 promise downgrade? No. One-way migration is honest
   and testable.
