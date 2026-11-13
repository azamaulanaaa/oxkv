# 7. Manifest v1

oxkv promises that a client can declare a scope and receive exactly the bytes that
policy allows, on any target. That promise is an ABI claim. A server, a browser
replica, and a native binding must agree on table layout, field names, index
behaviour, rule meaning, and operation shape. The ShardSnapshot header, the wire
handshake, and the receipt carry the digest of the document that defines them. If
those definitions live in more than one place, they drift silently. The manifest
must be the only declarative source. One compiler must turn it into every
artifact that needs them. C1 (manifest schema) freezes the schema, the compile
rules, and the named errors.

## What the greater goal demands

A data shard contains one user table and its system sub-namespaces. The manifest
declares a user table by name. The key encoder derives the table prefix from the
name. The shard map, and not the manifest, decides placement. The key encoder,
the shard map, and the scope engine must read that declaration.

A replica is read-only and holds a scope, which is a policy-filtered query over
user-table prefixes. The client must know which fields it can filter, which index
serves a filter, and which rules bound the result. The manifest must declare those
facts, or the client cannot reject an unservable query before it leaves the
device.

A write is a request with an idempotency key and a base version. Validation must
be deterministic and must run against a pinned `policy_version`. The manifest must
declare rule identifiers, selectors, and fact read sets as data. Mutable rule
bodies live in a separate content-addressed policy catalog.

The compiler generates the RPC surface from the manifest. That claim holds only if
the manifest declares each operation, its request and response fields, its target
table, and its errors.

## Required capabilities

- The manifest must declare user tables with a name, a key schema, and a field
  set. Each field must declare its type, encoding, filter use, and index use.
  The manifest must not declare an arbitrary byte prefix or a system table.
- The manifest must declare indexes as an ordered field list per table, with a
  `tokenizer_version`, a scorer, and integer scorer parameters.
- The manifest must declare rule identifiers, selectors, effects, scopes, cost
  bounds, strict flags, retention classes, audit visibility, and fact read sets.
  It must not embed a mutable rule body.
- The manifest must declare facts with an identity, canonical type, owner shard,
  fan-in cap, and staleness bound.
- The manifest must declare operations with a name, a target table, request and
  response fields, a streaming shape, an error set, and an auth class.
- The manifest must declare events, receipts, audit, projection, codecs, and
  control sections.
- The compiler must emit a neutral descriptor and five artifact families:
  storage layout, index configuration, client scope, write validators, and
  generated RPC stubs for native and browser.
- The manifest must carry a `manifest_schema_version`, a `manifest_revision`, and
  a `manifest_digest` of its canonical bytes.
- A validation pass must reject every ill-formed manifest with a named error
  before it writes an artifact.
- The compiler must be pure. It must not read the clock, the environment, the
  network, or an undeclared file.

## Invariants that must hold

1. **One source.** No generated artifact may state a fact the manifest omits, and
   no human may edit one.
2. **Determinism.** The same manifest bytes must produce byte-identical artifacts
   on every run, host, and target.
3. **Partition by table name.** Table names must be unique and must match
   `[a-z][a-z0-9_]*`. The manifest must not declare placement or a byte prefix.
4. **Resolution.** Every index field, rule path, and operation field must resolve
   to a declared field.
5. **Bound.** Every rule must carry a static cost inside the evaluation budget.
   A rule without a decidable bound must fail to compile.
6. **Single shard.** Every exposed operation must resolve to one shard for every
   admissible request key. The shard map proves that, not the manifest.
7. **Digest binding.** Every artifact and ShardSnapshot must carry the
   `manifest_digest`, and a reader must refuse a mismatch.
8. **Stable names.** An error name must never change meaning inside a schema major
   version.
9. **One evaluator.** The compiler must emit validators as rule descriptors, never
   a second policy implementation.

## Decisions and technical choices

**The authoring form is TOML and the canonical form is JSON.** The
compiler reads TOML and emits one canonical JSON encoding, which servers,
replicas, and bindings read. TOML gives explicit types, comments, and a small
grammar, so two parsers agree on the bytes. JSON gives a universal encoding and a
stable input to the digest. The TOML to JSON mapping must be total, lossless, and
part of C1.

| Form | Strength | Failure here |
| --- | --- | --- |
| YAML | Comments, familiar | Implicit typing splits readers on the same bytes |
| TOML | Explicit types, comments, small grammar | Deep repeated structures read poorly |
| JSON | Universal, one obvious encoding | No comments, so rationale must live elsewhere |
| Rust macro | Type-checked, refactorable | Ties the source to one language and one build |

A Rust macro is the strongest authoring tool for Rust users. It is the weakest
for this goal. The manifest must be stored as versioned data, read by a browser,
and replaced by a write with a receipt. A macro supports none of those.

**The manifest has three qualified version fields.** The `manifest_schema_version` names the language, for example `oxkv/manifest/v1`. The `manifest_revision` is a monotonic counter assigned when a manifest is committed as data. The `manifest_digest` is the hash of the canonical bytes. Artifacts, receipts, audit records, and ShardSnapshots carry the digest. The schema decides who reads the file. The revision orders changes. The digest decides whether two parties describe the same world. The active `policy_version` is not part of the `manifest_digest`.

**Policy bodies live in a separate catalog.** A rule body change creates a new `policy_version` and a new policy catalog record. It must not change the `manifest_digest`. A canary can then serve two policy versions under one manifest. The manifest declares rule identifiers and selectors, and a policy catalog location. The policy catalog is content-addressed and stored in the catalog table.

**Validation returns named errors, not prose.** Each name has one cause and a fixed shape. The name is the contract. The message is for humans. The manifest validates schema only. It must not require the shard map.

| Name | Condition |
| --- | --- |
| `manifest.unsupported_schema` | The schema major version is not 1 |
| `manifest.unknown_field` | A key is absent from the schema |
| `manifest.duplicate_name` | Two declarations share a name |
| `manifest.reserved_table_name` | A user table name is reserved or starts with `_` |
| `manifest.key_schema_missing` | A table has no key schema |
| `manifest.float_not_allowed` | Canonical JSON contains a float |
| `manifest.index_unknown_field` | An index names an undeclared field |
| `manifest.index_scope_unreachable` | An index serves no declared scope |
| `manifest.rule_unknown_path` | A rule path does not resolve |
| `manifest.rule_unbounded` | A rule exceeds the cost budget |
| `manifest.operation_cross_shard` | An operation can require two shards |
| `manifest.operation_unknown_field` | An operation names an undeclared field |
| `manifest.policy_catalog_missing` | The declared policy catalog does not resolve |
| `manifest.artifact_drift` | An artifact does not match the digest |

Example:
`manifest.reserved_table_name: table "_receipt" uses a reserved user table name`.

**Canonical JSON has one byte form.** It must allow only integers in the i64 range, strings, booleans, arrays, and objects. It must not contain floating point values. It must use one key order. It must reject duplicate keys. A manifest with a float fails with `manifest.float_not_allowed`.

**Generated artifacts are the mechanism, not a convenience.** Server, replica, and
binding must agree on one definition of a field. A hand-maintained client scope
drifts from the server layout at the first rename. A hand-written validator
becomes a second policy evaluator. A hand-written RPC stub encodes a field set
the server no longer serves. Generation moves each disagreement into one compile
step, where a named error stops the build, and a consistent ABI survives a rename.

## Dependencies

A1 (key encoding) supplies table-name and ordering rules. C2 (snapshot and page format)
supplies the digest field in the header. C4 (policy language) supplies the rule
AST and cost model. The policy catalog supplies immutable rule bodies. The static shard map supplies the table assignment.
Generated RPC uses operation declarations.
Policy projection uses rule facts and their declared shard residency.

## Acceptance criteria

1. A manifest with three tables, four indexes, five rules, and three operations
   compiles to the descriptor and all five artifact families.
2. The same input compiles to identical bytes across one hundred runs, native,
   and wasm.
3. Both readers compute one `manifest_digest` from the canonical JSON.
4. A TOML to canonical JSON round trip preserves every declared value. A lossy
   input fails to compile.
5. Each named error has a fixture and a test that matches the identifier.
6. A stale artifact fails regeneration with `manifest.artifact_drift`.
7. A manifest with a float fails with `manifest.float_not_allowed`.
8. A rule above the budget fails with `manifest.rule_unbounded`.
9. An operation that can reach two shards fails with
   `manifest.operation_cross_shard`.
10. A rule change creates a new `policy_version`, returns a receipt, and produces
    one audit record naming the `policy_version` and `manifest_digest`.
11. A rule body change leaves the `manifest_digest` unchanged.
12. A replica holding digest A rejects a diff produced under digest B.
13. A query that names an unfilterable field stops at the client with a named
    error.
14. A manifest with a reserved user table name fails with
    `manifest.reserved_table_name`.

## What would falsify this design

- A field rename requires a hand edit to a generated artifact.
- Two clients with the same digest disagree about whether a query is admissible.
- A rule bound cannot be decided at compile time.
- Compiler output changes across hosts or runs.
- Adding an index requires a wire protocol change.
- A needed change cannot be expressed as an additive minor change.

## Open questions

1. Should the compiler run as a separate command or inside the build?
2. How many schema minors must a reader accept?
3. Do computed fields belong in v1, or do they hide a rule the policy language
   should own?
4. Does compression and encryption policy belong in the manifest or in storage?
5. Does an operation declare its idempotency scope, or does the protocol fix one?
