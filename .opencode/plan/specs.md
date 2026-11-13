# oxkv 1.0 Consolidated Roadmap Specification

| Field | Value |
| --- | --- |
| Status | Proposed |
| Revision | 0.2 |
| Date | 2026-11-12 |
| Supersedes | Revision 0.1 |
| Scope | Resolve conflicts in `roadmap.md` and essays 01-32 |
| Normative language | RFC 2119 |
| Owner | Architecture review |
| Reviewers | Storage, policy, sync, search, operations, security |

## 1. Purpose and reading guide

This document is the single normative specification for the 1.0 roadmap. It records the conflicts between `roadmap.md` and essays 01-32. It gives one decision for each conflict. It defines the shared registries that all essays must use after this review.

This document does not replace the essays. The essays remain the detailed design. If an essay conflicts with this document, this document wins until the essay is updated.

Use this document in this order:

1. Read section 4 to agree on terms.
2. Read section 5 to agree on the system model.
3. Read section 6 to agree on the frozen surfaces.
4. Read section 7 to agree on the decisions.
5. Read section 8 to use the shared registries.
6. Read section 9 for the subsystem requirements.
7. Read section 10 for the work sequence.
8. Read section 11 for the conflict index.
9. Read section 12 for the per-essay edits.
10. Read section 13 before you call a contract frozen.

A reader who only needs one answer should use the conflict register in section 11. Each row names the decision that resolves it.

## 2. Scope and precedence

This document covers:

- The 1.0 declarative surface.
- The 1.0 byte contracts.
- The 1.0 write, read, sync, search, event, and audit semantics.
- The 1.0 work sequence and freeze gates.

This document does not cover:

- Post-1.0 features.
- Numeric service-level objectives.
- Commercial support terms.
- Implementation language choices beyond the frozen contracts.

Precedence order:

1. This document.
2. `roadmap.md`.
3. Essays 01-32.
4. Design notes and pull request descriptions.

If a lower document conflicts with a higher document, the higher document wins. The lower document must change.

## 3. Normative language

The key words MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY are to be interpreted as described in RFC 2119.

A requirement is testable. Each MUST has one of these forms:

- A golden vector.
- A property test.
- A fault-injection test.
- A static check.
- A measured falsifier.

A requirement without a test is not ready for the freeze.

## 4. Terms and naming rules

### 4.1 Core terms

| Term | Definition |
| --- | --- |
| **Authority** | The component that owns truth for a shard and serializes writes. |
| **Replica** | A read-only copy that holds a scope, an index view, and an outbox. |
| **Client** | The caller that submits requests and reads through a replica. |
| **Store** | The data layer that oxkv provides. |
| **System** | The full product, including authority, replicas, clients, and operations. |
| **User table** | A table that the manifest declares for application data. |
| **System table** | A reserved table that the store owns. |
| **Catalog table** | The top-level system table `_catalog`. It holds manifest, policy, map, and statistics records. |
| **Table** | A user table or a system table. A table has one prefix. |
| **Data shard** | A shard that contains exactly one user table and the system sub-namespaces for that table. |
| **Catalog shard** | A shard that contains the catalog table and its system sub-namespaces. |
| **Shard** | A contiguous range of the ordered key space. A data shard contains one user table. |
| **Scope** | A policy-filtered query over one or more user-table prefixes. A scope is the unit of replication. |
| **Scope identity** | A deterministic hash of the canonical scope declaration. |
| **Scope pin** | The tuple of manifest digest, policy version, map version, and scope identity. |
| **Scope version** | A version vector with one commit version per shard in the scope. |
| **ShardSnapshot** | A snapshot of one shard range at one commit version. |
| **ScopeSnapshot** | A deterministic, scope-filtered projection of one or more ShardSnapshots. |
| **Page** | A framed unit of snapshot bytes with a key range, identity, sequence, and integrity value. |
| **Commit version** | A monotonically increasing version that the authority assigns to a shard commit. |
| **Key version** | The version of one key record. It is the base for conflict checks. |
| **Version vector** | A map from shard identifier to commit version. |
| **Policy version** | A content-addressed version of a policy AST and its cost model. |
| **Manifest digest** | The content hash of the canonical manifest. |
| **Map version** | The version of the shard map. |
| **Map digest** | The content hash of the canonical shard map. |
| **Lease term** | A monotonically increasing fencing value for shard ownership. |
| **Projection epoch** | A versioned identity for one fact projection. |
| **Fact** | A value that a policy reads from a declared fact source. |
| **Fact source** | The shard or subsystem that owns a fact. |
| **Projection** | A versioned copy of facts inside the shard that evaluates the rule. |
| **Receipt** | A durable record of a write outcome. |
| **Queued** | A live receipt state. The write is not terminal. |
| **Committed** | A terminal receipt state. The write is durable. |
| **Rejected** | A terminal receipt state. The authority refused the write. |
| **Conflict** | A terminal receipt state. The base version did not match. |
| **Outbox** | Durable client state that holds intents before submission. |
| **Business event** | An authority-committed fact that represents a write side effect. |
| **Operational feed** | A separate stream for audit export, metrics, traces, budget signals, and lifecycle notices. |
| **Audit record** | A durable record of one write enforcement verdict. |
| **Audit chain** | A hash-linked sequence of audit records per shard. |
| **Authority read pin** | A bounded pin for a read on the authority. |
| **Replica local pin** | The applied version vector of a replica. It does not expire with authority retention. |
| **Resume cursor** | A bounded cursor for a sync diff stream. |
| **Search pin** | A version vector plus query fingerprint for search pagination. |
| **Coverage descriptor** | A record of answered shards, missing shards, and served versions. |
| **Exact result** | A result where every targeted shard served the pin. |
| **Coverage result** | A result with a coverage descriptor and an explicit partial status. |
| **Contract** | A byte or semantic surface that a 1.x reader must preserve. |
| **Annex** | A frozen surface that belongs to a core contract. |
| **Candidate** | A versioned surface that may break before 1.0. |
| **Frozen** | A surface that must not break inside 1.x. |
| **Wave** | A group of contract and implementation work with one gate. |
| **Vector** | A golden test input and expected output. |
| **Falsifier** | A measured benchmark with a threshold that can disprove a claim. |

### 4.2 Naming rules

| Item | Rule | Example |
| --- | --- | --- |
| User table name | `[a-z][a-z0-9_]*` | `accounts`, `ledger_entry` |
| System table name | Only `_catalog` at the top level in 1.0 | `_catalog` |
| System sub-namespace | One byte after the `0xFF` marker | `0x01` receipt |
| Error code | Lowercase dot namespace | `write.idempotency_key_reuse` |
| Cursor type | PascalCase with the `Cursor` suffix | `SyncCursor` |
| Contract version | Name plus major and minor | `snapshot-page/v1.0` |
| Policy AST version | `policy-ast/vN` | `policy-ast/v1` |
| Manifest schema | `oxkv/manifest/vN` | `oxkv/manifest/v1` |
| Map schema | `oxkv/map/vN` | `oxkv/map/v1` |
| Event envelope | `oxkv/event/vN` | `oxkv/event/v1` |
| Receipt envelope | `oxkv/receipt/vN` | `oxkv/receipt/v1` |

### 4.3 Forbidden ambiguous terms

These terms MUST NOT appear in a frozen contract. Use the replacement.

| Forbidden term | Problem | Replacement |
| --- | --- | --- |
| "the version" | Names at least eight different version axes | Use the qualified name, such as `commit_version` or `policy_version` |
| "the cursor" | Names at least eight cursor types | Use the cursor type, such as `SyncCursor` |
| "the event" | Names a business event or an operational signal | Use "business event" or "operational signal" |
| "the lease" | Names the authority lease or the local tab lease | Use "authority lease" or "local tab lease" |
| "shard equals table" | False when a shard also holds system sub-namespaces | Say "a data shard contains one user table and its system sub-namespaces" |
| "partial result" | Blurs exact and coverage results | Use "coverage result" or "exact result" |
| "fuel" | Names a policy budget in a runtime-specific way | Use "step budget" |
| "fact age" | Blurs version distance and wall time | Use "fact version distance" for the contract, and "fact age" only for monitoring |
| "global version" | Cross-shard reads use a vector | Use "scope version vector" |
| "the snapshot" | Names a shard snapshot or a scope snapshot | Use `ShardSnapshot` or `ScopeSnapshot` |

## 5. System model

### 5.1 Ordered key space

The key space is ordered by unsigned bytes. The order is lexicographic. The order does not depend on locale, platform, pointer width, or storage engine.

Key encoding v1 uses this form:

```text
0x01 || table-name || 0x00 || user-component...
0x01 || table-name || 0x00 || 0xFF || system-id || system-key...
```

The first byte is the key encoding version. The table name is ASCII. The zero byte ends the table name. A user component starts with a type tag. A system key starts with the `0xFF` marker, then a one-byte system identifier.

The user key validator MUST reject `0xFF` as a component type tag. The system key validator MUST accept only a registered system identifier.

### 5.2 Data shard layout

A data shard contains exactly one user table. The user table prefix covers the user key space and the system sub-namespaces for that table.

```text
Data shard S1
  User table "accounts"
    P = 0x01 "accounts" 0x00
    User keys:        P || 0x10 ...   text component
                      P || 0x11 ...   bytes component
                      P || 0x20 ...   u64 component
                      P || 0x21 ...   i64 component
                      P || 0x30 ...   bool component
    System keys:
      P || 0xFF || 0x01 || ...       receipt
      P || 0xFF || 0x02 || ...       event
      P || 0xFF || 0x03 || ...       audit
      P || 0xFF || 0x04 || ...       projection
      P || 0xFF || 0x05 || ...       index
      P || 0xFF || 0x06 || ...       lease
      P || 0xFF || 0x07 || ...       statistics
```

The shard range is `[P, successor(P))`. The range contains every user key and every system key for that table.

The user key validator rejects a key that contains `0xFF` after `P`. This rule keeps the user key space disjoint from the system key space.

### 5.3 Catalog shard layout

The catalog shard contains the top-level system table `_catalog`. The catalog table holds manifest, policy, map, and statistics records. It has the same system sub-namespaces as a data shard.

```text
Catalog shard S0
  System table "_catalog"
    Q = 0x01 "_catalog" 0x00
    Catalog keys:     Q || 0x10 ...   manifest, policy, map, stats
    System keys:
      Q || 0xFF || 0x01 || ...       receipt
      Q || 0xFF || 0x02 || ...       event
      Q || 0xFF || 0x03 || ...       audit
      Q || 0xFF || 0x06 || ...       lease
```

The catalog shard is the only shard that may use a top-level system table in 1.0. A later release may add top-level system tables with a new key encoding version.

### 5.4 Scope and replica model

A scope is a policy-filtered query over one or more user-table prefixes. A replica holds one scope.

A replica stores:

- User rows that the scope admits.
- Projection facts that the scope needs for local evaluation.
- Its own search index, built from scope members.
- Its own outbox and local tab lease state.

A replica does not store:

- Receipts that belong to other clients.
- Authority events.
- Authority audit records.
- Authority lease records.
- User rows outside its scope.

A ScopeSnapshot contains only the rows and facts that the scope admits. It does not contain authority-only system sub-namespaces.

### 5.5 Commit domains

A commit domain is one shard. A write, its index update, its receipt, its audit record, and its business events MUST commit in one shard transaction.

The catalog shard is a commit domain for rule activation, policy catalog changes, map changes, and manifest changes. A rule activation MUST produce a receipt and an audit record in the catalog shard.

A write MUST NOT span two commit domains. A cross-shard mutation MUST fail with `shard.cross_mutation_rejected`.

### 5.6 Scope snapshots and shard snapshots

A ShardSnapshot covers one shard range at one commit version. It contains the user table and all system sub-namespaces in that shard.

A ScopeSnapshot covers one scope at one scope version. It is a deterministic projection of one or more ShardSnapshots. It contains only scope user rows and the projection facts that the scope needs.

The replica hydrates a ScopeSnapshot. The authority uses a ShardSnapshot for backup, restore, and replication.

## 6. Contract and annex register

### 6.1 Core contracts

| ID | Contract | Version | Frozen parts | Free parts |
| --- | --- | --- | --- | --- |
| C1 | Manifest schema | `oxkv/manifest/v1` | Table declarations, key schema, fields, indexes, rule metadata, facts, operations, events, receipts, audit, projection, codecs, control | Rust builders and generated code |
| C2 | Snapshot and page format | `snapshot-page/v1` | Header, provenance, page identity, page sequence, checksums, framing, encoding, key order | Codec named in header, block size, cache |
| C3 | Sync wire protocol | `sync-wire/v1` | Framing, `SessionHello`, subscribe, diff, cursor, resume, flow control | Transport and batching |
| C4 | Policy language | `policy-ast/v1` | AST, evaluation order, determinism, step budget, explanation | Source syntax sugar and diagnostics |
| C5 | Receipt and error semantics | `receipt-error/v1` | States, idempotency, retry class, error registry, receipt envelope | Scheduler, outbox storage, logs |

### 6.2 Annexes

| ID | Annex | Parent | Version | Frozen parts | Free parts |
| --- | --- | --- | --- | --- | --- |
| A1 | Key encoding | C2 | `key-encoding/v1` | Version byte, table grammar, component tags, system marker, order, bounds | Table set and schemas |
| A2 | Tokenizer | C1 | `tokenizer/v1` | Identifier, version, Unicode data, normalization, case folding, segmentation | Ranking parameters |
| A3 | Event envelope | C5 | `event/v1` | Envelope fields, event identity, ordering, retention | Consumer library |
| A4 | Shard map | C1 | `oxkv/map/v1` | Routing, table assignment, bounds, version, digest | Placement control plane |
| A5 | Scope semantics | C3 | `scope/v1` | Identity, version vector, diffs, removal reasons | Scope composition |
| A6 | Projection staleness | C4 | `projection/v1` | Version-distance bound, epoch, watermarks | Numeric limits |
| A7 | Audit envelope | C5 | `audit/v1` | Record fields, hash chain, checkpoints | Export tooling |
| A8 | Cost and explain | C1 | `cost/v1` | Explain schema, cost error names | Weights and estimates |
| A9 | Cursor types | C3 | `cursor/v1` | Token formats, fields, expiry, errors | Server-side state |
| A10 | Descriptor | C1 | `descriptor/v1` | Method, message, field, code mapping | Language bindings |
| A11 | Read pin and staleness | C3 | `read-pin/v1` | Pin modes, session vector, staleness errors | Numeric bounds |
| A12 | Migration and backup | C2 | `migration/v1` | Backup manifest, restore steps, migration steps | Tooling and scheduling |

### 6.3 Register rules

- Every frozen surface MUST have one row in this register.
- Every row MUST name its parent if it is an annex.
- Every row MUST have a version, a specification, a changelog, and a vector corpus.
- Every row MUST have a migration path.
- No essay may use the word "freeze" unless it names a register row.
- `32-one-zero-freeze.md` MUST list every row in this register.

## 7. Normative decisions

### 7.1 D1. Shard, table, and system-key model

**Decision.** A data shard contains exactly one user table. The user table prefix contains the user key space and the system sub-namespaces for that table. A system key uses the `0xFF` marker and a one-byte system identifier. No table splits across shards in 1.0.

**Rules.**

- A user table name MUST match `[a-z][a-z0-9_]*`.
- A user table name MUST NOT start with `_`.
- The only top-level system table in 1.0 is `_catalog`.
- The manifest MUST declare user tables only. It MUST NOT declare system tables.
- The manifest MUST declare a table name and a key schema. It MUST NOT declare an arbitrary byte prefix.
- The key encoding MUST derive the table prefix from the table name.
- The shard map MUST assign every user table to exactly one data shard.
- The shard map MUST assign `_catalog` to the catalog shard.
- A data shard MUST contain the system sub-namespaces for its user table.
- A system sub-namespace MUST NOT appear in a user scope.
- The user key validator MUST reject `0xFF` as a component tag.
- The system key validator MUST accept only a registered system identifier.
- A table MUST NOT split across shards in 1.0.
- The phrase "shard equals table" is a shorthand. The literal rule is "a data shard contains one user table and its system sub-namespaces."

**Why.** Receipts, events, audit records, projection facts, index postings, and lease records must commit in the same shard transaction as the data. They need keys in the same shard range. If they are separate top-level tables, their prefixes may not be contiguous with the user table. The `0xFF` system marker keeps every system key inside the user table range.

**Rejected alternatives.**

- **One user table per shard with separate system shards.** This breaks single-shard atomicity.
- **System tables as top-level prefixes.** Their ranges may not be contiguous with the user table range. This breaks the contiguous shard range.
- **Shard identifier in every key.** This makes table movement a full rekey. It also breaks the rule that table prefixes are independent of placement.
- **Table split.** This breaks the scope-to-shard fan-out rule and the snapshot unit.

**Examples.**

- A receipt for key `accounts/42` is stored at `P_accounts || 0xFF || 0x01 || receipt-key`. It commits with the account row.
- The audit record for that write is stored at `P_accounts || 0xFF || 0x03 || audit-sequence`.
- The search posting for that row is stored at `P_accounts || 0xFF || 0x05 || posting-key`.
- The lease record for the shard is stored at `P_accounts || 0xFF || 0x06 || lease-key`.

**Acceptance.**

- A property test over random keys returns one shard per key.
- A property test proves that a user key never starts with `0xFF` after the table prefix.
- A property test proves that a system key is inside the user table range.
- A manifest with a reserved user table name fails with `manifest.reserved_table_name`.
- A map with a split table fails with `map.table_split_unsupported`.
- A cross-shard write fails with `shard.cross_mutation_rejected` and changes no state.

**Essay edits.** 01, 04, 05, 06, 07, 10, 12, 17, 18, 22, and 24 must replace "shard equals table" with the D1 model. 05 must add the system marker and system identifiers. 07 must declare table names, not prefixes. 22 must remove split points.

### 7.2 D2. Snapshot and page artifacts

**Decision.** Use two named artifacts. A ShardSnapshot covers one shard at one commit version. A ScopeSnapshot covers one scope at one scope version. A replica hydrates a ScopeSnapshot. A page has a deterministic identity and sequence. A content seal is separate from the authority lease term.

**Rules.**

- A ShardSnapshot MUST include the user table and every system sub-namespace in the shard.
- A ScopeSnapshot MUST include only the user rows that the scope admits.
- A ScopeSnapshot MUST include only the projection facts that the scope needs.
- A ScopeSnapshot MUST NOT include receipt, event, audit, or lease system sub-namespaces.
- The authority MUST build a ScopeSnapshot deterministically from one or more ShardSnapshots.
- The same inputs MUST produce the same ScopeSnapshot bytes on native and wasm32.
- A page MUST carry `page_id`, `page_sequence`, `shard_id`, `commit_version`, key range, record count, codec identifier, uncompressed length, and CRC32C.
- `page_id` MUST be a deterministic hash of `shard_id`, `commit_version`, `page_sequence`, and the key range.
- `page_sequence` MUST be strictly increasing inside one snapshot.
- The page directory MUST list every page with its offset, length, first key, last key, `page_id`, `page_sequence`, and integrity value.
- The directory MUST carry its own integrity value.
- The snapshot content digest MUST cover the directory and every page digest.
- A snapshot content seal MUST cover the content digest and the lease term.
- The lease term MUST NOT enter the content digest.
- A sealed snapshot MUST remain valid across a lease change.
- The header MUST carry the provenance block in section 8.1.
- The header MUST be a canonical text preamble.
- The body MUST be binary pages.
- The reader MUST reject an unknown major version with `format.unsupported_version`.
- The reader MUST reject an unknown required feature with `format.missing_required_feature`.
- The reader MUST reject a corrupt page with `format.corrupt_page`.
- The reader MUST reject a page outside the shard bounds with `format.page_out_of_bounds`.
- The reader MUST reject a truncated snapshot with `format.truncated_snapshot`.

**Why.** A shard snapshot is the authority and backup unit. A scope snapshot is the replication unit. If the replica hydrates a raw shard snapshot, it stores out-of-scope rows. If the lease term enters the content digest, a leader change invalidates immutable bytes. If a page has no identity, the replica cannot refetch or evict it safely.

**Rejected alternatives.**

- **One snapshot type for both roles.** A shard snapshot is too large for a selective scope. A scope snapshot cannot serve backup.
- **Lease term in the content digest.** This breaks byte identity across leader changes.
- **Page identity from byte offset only.** An offset is not stable when the snapshot is rewritten or a page is refetched.
- **Text body.** The hot path needs binary pages. A text header is enough for inspection.

**Acceptance.**

- A v1 reader reads a v1 snapshot offline.
- A v1 reader rejects a v2 major snapshot with `format.unsupported_version`.
- A one-byte change in a page produces `format.corrupt_page` with page id and byte offset.
- A page outside the shard range produces `format.page_out_of_bounds`.
- A snapshot cut at any offset produces `format.truncated_snapshot`.
- Two writers of the same ordered input produce identical content bytes and digests.
- A sealed snapshot remains readable after a lease term change.
- A ScopeSnapshot contains zero user rows outside the scope.
- A ScopeSnapshot build is byte-identical on native and wasm32.

**Essay edits.** 06 must add page identity, page sequence, provenance, and the seal split. 13 and 16 must use ScopeSnapshot for hydration. 15 must store ScopeSnapshot pages. 04 must use the seal in tier movement.

### 7.3 D3. Replica data and search index

**Decision.** A replica stores scope-projected pages. It builds its search index from scope members. Ranking uses document-local features only in 1.0. The authority per-shard index is not the replica index.

**Rules.**

- A replica MUST store only scope-projected user rows and the projection facts that the scope needs.
- A replica MUST NOT store a user row outside its scope.
- A replica MUST build its search index from scope members.
- The replica index build MUST use the frozen tokenizer and the manifest scorer.
- The authority per-shard index MUST NOT be mirrored as the replica index in 1.0.
- Ranking MUST use document-local features only.
- Global corpus statistics MUST NOT affect a 1.0 ranking.
- The manifest MUST declare the scorer and its integer parameters.
- The scorer MUST NOT use floating point.
- The replica MUST expose the tokenizer version and index version.
- A tokenizer mismatch MUST return `search.tokenizer_mismatch`.
- An index digest mismatch MUST return `search.index_mismatch`.

**Why.** Scope closure and offline search cannot both hold if the replica mirrors an authority index that contains out-of-scope postings. A scope-filtered index build is deterministic. Document-local scoring avoids a global statistics artifact that a selective replica cannot reproduce.

**Rejected alternatives.**

- **Mirror authority index bytes.** This leaks out-of-scope postings and breaks scope closure.
- **Global corpus statistics.** A selective replica does not have the corpus. Global statistics need a separate scope-scoped artifact.
- **Read-time filtering of mixed pages.** The replica still holds out-of-scope bytes. This breaks the policy boundary.

**Acceptance.**

- A search at a pin returns no hit outside the scope.
- A search hit point-reads to an in-scope row at the same pin.
- A replica index build is byte-identical on native and wasm32.
- A flipped index byte returns `search.index_mismatch`, not a short result.
- A tokenizer mismatch returns `search.tokenizer_mismatch`.
- Ranking is identical on authority and replica for the same document-local scorer.

**Essay edits.** 15 must remove index-byte mirroring. 18 must make replica build the primary path and document-local scoring the 1.0 rule. 19 must state document-local scoring. 20 must remove global statistics from ranking.

### 7.4 D4. Sync wire and RPC layering

**Decision.** The sync wire contract is the custom sans-IO frame protocol in essay 14. Connect is the RPC envelope for generated RPC. Connect carries sync frames as opaque bytes. One `SessionHello` negotiates the session.

**Rules.**

- The sync protocol MUST be a sans-IO state machine.
- The sync protocol MUST define its own frame types, diff semantics, cursor, and resume rules.
- Connect MUST be the RPC envelope for generated RPC.
- A Connect server stream MAY carry sync frames as opaque bytes.
- RPC MUST NOT re-encode a sync diff entry into a second schema.
- One `SessionHello` MUST negotiate the RPC descriptor, sync protocol version, manifest digest, map version, capabilities, and limits.
- `SessionHello` MUST NOT negotiate a policy version. A scope subscription carries its policy version.
- A data call MUST NOT precede `SessionHello`.
- One session MUST NOT renegotiate.
- A scope subscription MAY carry the canonical policy AST or scope descriptor as opaque bytes.
- RPC MUST NOT decode or rewrite that policy payload.
- The in-process transport MUST be the reference transport.
- Every other transport MUST produce the same message sequence for the same input transcript.
- A transport MAY change framing and lifecycle. It MUST NOT change ordering, retry, or error meaning.

**Why.** The sync protocol must stay stable for years. A framework's versioning and status codes must not become the sync contract. Connect gives generated RPC and browser support without a proxy. The sync frames remain the frozen contract.

**Rejected alternatives.**

- **A custom RPC framework.** This makes the project own framing, flow control, multiplexing, and every binding.
- **gRPC in the browser.** This needs a proxy and creates a second protocol.
- **Connect as the sync frame format.** This ties the sync contract to an external framework and loses the frozen frame vectors.
- **Two handshakes.** This creates two version negotiation paths and two failure modes.

**Acceptance.**

- A browser completes `SessionHello`, subscribe, snapshot, tail, resume, and receipt over Connect without a proxy.
- The in-process and network transports produce the same sync message sequence for one transcript.
- An unnegotiated data call returns `sync.negotiation_required`.
- A client below the minimum manifest version returns `sync.incompatible_manifest`.
- A policy payload survives the RPC layer byte for byte.
- A Connect transport error maps to a named protocol error, not a raw status.

**Essay edits.**

- 14 must say "custom sync frames, transport-independent."
- 25 must say "Connect is the RPC envelope and carries sync frames as opaque bytes."
- Roadmap item 25 must say "Connect, not gRPC."

### 7.5 D5. Policy projection and staleness

**Decision.** The authorization fact bound is a version distance per fact source. Time is monitoring data only. Default lag keeps a receipt `Queued` with a deadline. Strict mode rejects with `authorization.stale_facts`. Projection invalidation follows fact-set changes, not every policy predicate change.

**Rules.**

- The authorization fact bound MUST be a version distance per fact source.
- Time MAY be recorded for monitoring.
- Time MUST NOT enter a policy verdict.
- Default behavior: a write over the bound MUST stay `Queued` with reason `projection.lagging`.
- The authority MUST enforce a queue deadline.
- At the deadline, the receipt MUST become `Rejected` with `write.queue_timeout`.
- Strict mode: a rule or table MAY declare strict authorization.
- A strict write over the bound MUST become `Rejected` with `authorization.stale_facts`.
- Projection invalidation MUST depend on a fact-set change, schema change, ownership change, or revocation.
- Projection invalidation MUST NOT depend on every policy predicate change.
- A canary cohort MUST use a projection epoch that includes its policy version.
- A policy version change MUST NOT invalidate a projection that reads the same facts with the same schema.
- A projection MUST be idempotent, monotone, and a pure function of the fact log at a stated version.
- A projection MUST reject a client write with `projection.client_write_rejected`.
- A projection MUST reject a watermark regression with `projection.watermark_regression`.
- A projection MUST fail closed on a checksum error.
- A verdict MUST record the policy version, projection epoch, per-source watermarks, fact versions read, rule identity, and decision.

**Why.** A time-based verdict depends on wall clock, which breaks determinism and audit replay. A global invalidation on every policy change makes a canary impossible. A `Queued` receipt with no deadline violates the operations rule that no receipt stays Queued. A projection that accepts client writes becomes a second truth.

**Rejected alternatives.**

- **Wall-clock age in the verdict.** This makes the verdict non-deterministic and hard to replay.
- **Global invalidation on every policy change.** This breaks two-version canaries.
- **Unbounded `Queued`.** This hides a stuck projection.
- **Fail open on projection lag.** This is an authorization bypass.
- **A remote read on commit.** This makes the authorization shard a gate for every write.

**Acceptance.**

- A non-strict write one version over the bound returns `Queued` with `projection.lagging`.
- The same write at the queue deadline returns `Rejected` with `write.queue_timeout`.
- A strict write one version over the bound returns `Rejected` with `authorization.stale_facts`.
- A canary cohort serves policy v1 and a control cohort serves policy v2 at the same time.
- A policy predicate change that reads the same facts does not invalidate the projection.
- A fact-set change invalidates the projection and forces a rebuild.
- A projection rebuild reaches the same watermark as an uninterrupted feed.
- An audit record replays the verdict from the fact log.

**Essay edits.** 12 must replace "age" with "version distance" and define strict versus default. 17 must scope invalidation and define the queue deadline. 27 must use per-cohort projection epochs. 28 must scope "no receipt stays Queued" to the queue deadline.

### 7.6 D6. Receipts, idempotency, and client cancellation

**Decision.** Keep four receipt states. Only the authority writes a verdict about data. Client cancellation is a local outbox disposition, not a receipt. Every terminal verdict is recorded under its idempotency key.

**Rules.**

- The receipt states MUST be `Queued`, `Committed`, `Rejected`, and `Conflict`.
- 1.x MUST NOT add a fifth state.
- Only the authority MAY write a receipt verdict about data.
- A client MAY mark a local outbox entry `CancelRequested` or `Abandoned`.
- A local `CancelRequested` or `Abandoned` state MUST NOT appear in a receipt vector.
- A late authority verdict MUST win for the data.
- If the authority commits after a local cancel, the receipt MUST be `Committed`.
- Every terminal verdict, including `Rejected`, MUST be recorded under its idempotency key.
- The same key and the same body MUST return the same receipt.
- The same key and a different body MUST return `write.idempotency_key_reuse` and MUST NOT create a second receipt.
- A `Rejected` verdict MUST NOT change data, index, or business events.
- A `Rejected` verdict MUST write a receipt and an audit record.
- A `Rejected` receipt MUST consume the idempotency key for the same body.
- A corrected body MUST use a new idempotency key.
- A receipt MUST be immutable after it reaches a terminal state.
- A receipt MUST name the audit record that justified it.
- A receipt MUST name the policy version and manifest digest used at commit.
- A receipt MUST name the resulting shard commit version for `Committed`.
- A receipt MUST name the current key version for `Conflict`.
- A receipt MUST NOT stay `Queued` past the authority queue deadline.

**Why.** The receipt is the client's proof. A client-written `Rejected` receipt can contradict a later commit. A fifth state would break the frozen state machine. If a `Rejected` verdict does not consume the key, the same key can produce a second terminal receipt. A receipt without an audit link is not enforceable.

**Rejected alternatives.**

- **A client-written `Rejected` receipt.** This breaks authority exclusivity and immutability.
- **A fifth `Canceled` state.** This breaks the 1.0 freeze and every retry table.
- **A `Rejected` verdict with no receipt.** The client cannot distinguish a rejection from a lost request.
- **A `Rejected` verdict with no audit record.** The verdict is not reviewable.
- **A `Rejected` verdict that does not consume the key.** The same key can produce two terminal outcomes.

**Acceptance.**

- A client cancels offline. The outbox shows `Abandoned`. No receipt is created.
- The authority commits the canceled write. The receipt is `Committed`. The client reconciles.
- A `Rejected` write changes no key, no index entry, and no business event.
- A `Rejected` write has one receipt and one audit record.
- Three deliveries of one request produce one version and three identical receipts.
- A reused key with a different body returns `write.idempotency_key_reuse` and writes no data.
- A terminal receipt never changes state.
- A receipt past the queue deadline is `Rejected` with `write.queue_timeout`.

**Essay edits.** 03 must state that a rejection writes a receipt and audit, and that the key is consumed. 09 must define key-reuse behavior and receipt fields. 10 must remove client-written `Rejected` and define local `Abandoned`. 12 must link the audit record. 32 must keep four states.

### 7.7 D7. Business events and operational signals

**Decision.** Use two streams. The business event stream is authority-only, atomic with a committed write, at-least-once, and ordered per key. The operational feed carries audit export, metrics, traces, budget signals, lifecycle notices, and alerts. Acknowledgement is per key.

**Rules.**

- The business event stream MUST be authority-only.
- A business event MUST commit with its write.
- A business event MUST be readable if and only if its write committed.
- Delivery MUST be at least once.
- Order MUST be per key.
- Acknowledgement MUST be per key.
- One unacknowledged event MUST block only its key.
- The consumer library MUST partition work by key.
- A per-shard cursor MUST NOT be the 1.0 consumer contract.
- The operational feed MUST use a separate envelope and retention.
- The operational feed MUST NOT be the only path to a business side effect.
- An effect payload MUST be self-contained within a declared bound.
- A large object MAY travel by reference only for optional enrichment.
- The reference MUST name an immutable object with a digest and a version.
- A consumer that needs a required effect field MUST NOT depend on a reference.
- `committed_at` MUST be an injected commit parameter.
- The core MUST NOT read a clock.
- The envelope MUST carry `origin_request_id`, not the raw idempotency key.
- The envelope MUST carry the pinned policy version and rule identity.
- The event id MUST derive from shard, key, commit version, rule identity, and emission index.
- The same commit MUST produce the same event id on native and wasm32.
- Event retention MUST have a time bound, a byte bound, or both.
- A cursor behind retention MUST return `event.retention_gap` with the oldest position.
- A poison event MUST end in a dead-letter record or a terminal error.
- The dead-letter record MUST NOT contain secrets.

**Why.** A business event is a write fact. An audit export is not a write fact. If both share one envelope, the event contract must tolerate events that no write produced. If a consumer uses a per-shard cursor, one poison key blocks every key on the shard. If an effect payload is only a reference, the consumer must read the authority, which breaks its isolation.

**Rejected alternatives.**

- **One stream for business events and audit.** This breaks "event iff write committed."
- **Per-shard consumer cursor.** This creates head-of-line blocking across keys.
- **Raw idempotency key in the event.** This leaks client identity and correlation.
- **Authority read in the consumer handler.** This makes the effect path depend on the authority.
- **Large payloads inline with no bound.** This breaks the event size and retention budgets.

**Acceptance.**

- A committed write that matches a rule emits one event with a stable id.
- A rejected or conflict write emits no business event.
- A poison event on key A does not block key B.
- A consumer cursor resumes with no skipped event.
- A consumer can build every required effect field from the payload.
- A reference-only optional field does not block the effect.
- The event envelope and id are byte-identical on native and wasm32.
- An audit export request reads the operational feed, not the business event stream.
- A budget signal is a metric or an operational signal, not a business event.

**Essay edits.** 12 must say audit export uses the operational feed. 20 must say budget signals are metrics. 24 must define per-key acknowledgement and the envelope fields. 26 must use per-key cursors and self-contained payloads. 27 must send lifecycle notices on the operational feed.

### 7.8 D8. Result modes and merge order

**Decision.** Exact mode is the default. Coverage mode is opt-in and explicit. Search never uses coverage for a page that claims exact ranking or pagination. Each query class has one comparator. Shard identity is not a semantic tie-break.

**Rules.**

- Exact mode MUST require every targeted shard to serve the pin.
- If one shard cannot serve the pin, exact mode MUST return a named error and no data.
- Coverage mode MUST be opt-in.
- A coverage result MUST carry a coverage descriptor.
- The coverage descriptor MUST list answered shards, missing shards, and served versions.
- A caller MUST NOT infer completeness from a row count.
- Search MUST NOT use coverage mode for a page that claims exact ranking or exact pagination.
- A coverage search MUST mark the result partial and MUST NOT advance an exact pagination cursor.
- The search comparator MUST be `(score descending, canonical key ascending)`.
- The general query comparator MUST be `(declared sort key ascending, canonical key ascending)`.
- The shard identifier MUST NOT be a semantic tie-break in a query order.
- The scope sync order MUST be `(shard identifier ascending, key ascending)`.
- The scope sync order is a concatenation order. It is not a query order.
- A cursor MUST bind the pin, query fingerprint, and last sort tuple.
- A cursor mismatch MUST return `search.cursor_invalid`.
- A page MUST NOT repeat or skip a row while the pin is readable.

**Why.** A silent partial result looks exact. A shard-id tie-break changes page boundaries when shards are added or reordered. A search cursor that can switch pins can repeat or skip rows.

**Rejected alternatives.**

- **Silent partial results.** The caller cannot detect missing data.
- **A global merge that includes shard id before key.** This makes page boundaries depend on placement.
- **Offset pagination.** Concurrent writes shift the offset.
- **A search cursor without a query fingerprint.** A changed query can reuse an old cursor.

**Acceptance.**

- An exact read with one unavailable shard returns a named error and no data.
- A coverage read returns the descriptor and no claim of exactness.
- A coverage search page is marked partial and does not advance an exact cursor.
- A search with deliberate ties returns the same order on authority, native, wasm32, and in-process.
- A changed query, scope, sort, or pin returns `search.cursor_invalid`.
- A page with concurrent writes returns every original hit exactly once.
- A scope sync merge is stable under injected latency.

**Essay edits.** 13 must rename its partial result to coverage and remove the exactness claim. 19 must state exact search and the search comparator. 20 must remove the shard-id tie-break and define coverage. 22 must separate sync order from query order. 23 must align its read result with exact and coverage modes.

### 7.9 D9. Scope exactness and replica residency

**Decision.** Scope membership is exact at a complete coverage pin. Admission limits are checked at subscribe. A 1.0 offline scope must fit the declared device budget. Eviction may remove only out-of-scope or superseded pages.

**Rules.**

- Scope membership MUST be exact at the pinned scope version when coverage is complete.
- `max_members`, `max_bytes`, and `max_shards` MUST be admission limits.
- An over-limit scope MUST fail at subscribe with `scope.over_limit`.
- The store MUST NOT truncate an exact scope at runtime.
- Replica residency is separate from scope membership.
- For the 1.0 offline promise, a scope MUST fit the declared device budget.
- A scope that does not fit the device budget MUST fail at subscribe with `scope.device_budget_exceeded`.
- Eviction MAY remove out-of-scope pages and superseded pages.
- Eviction MUST NOT remove an in-scope logical member.
- A larger-than-memory scope MUST use a separate degraded contract.
- oxkv MUST NOT call that contract exact offline.
- Two replicas at the same scope version and with complete coverage MUST hold the same logical membership.
- Two replicas MAY have different residency for the same membership.

**Why.** A scope is a view, not a cache. If eviction can remove an in-scope member, an offline read cannot distinguish "no row" from "evicted row." If membership depends on device memory, two replicas can disagree. A separate degraded contract keeps the exact contract honest.

**Rejected alternatives.**

- **Cache semantics with eviction.** This breaks the offline promise.
- **Runtime truncation at `max_keys`.** This creates a silent partial scope.
- **Device-dependent membership.** This breaks deterministic scope identity.
- **A scope larger than the device with a hard memory ceiling.** This cannot keep the exact offline promise.

**Acceptance.**

- An over-limit scope fails at subscribe with `scope.over_limit` and transfers no data.
- A scope over the device budget fails at subscribe with `scope.device_budget_exceeded`.
- Two replicas at the same scope version hold the same member set.
- Eviction of an out-of-scope page does not change membership.
- An offline read of an in-scope member returns the row or a named error.
- A degraded scope is marked degraded and does not claim exact offline.
- A scope that fits the budget serves every member offline.

**Essay edits.** 13 must keep the view rule and remove truncation. 15 must remove the larger-than-RAM exact promise and define residency. 20 must make `max_keys` an admission limit. 31 must measure scope bytes against the declared budget.

### 7.10 D10. Pins, offline reads, and read-your-writes

**Decision.** Separate authority read pins, replica local pins, resume cursors, and receipt retention. A session token carries a version vector minimum. Read-your-writes is an explicit mode.

**Rules.**

- An authority read pin MUST be bounded.
- An expired authority pin MUST return `read.pin_expired`.
- A replica local pin MUST be the applied version vector.
- A replica local pin MUST remain readable offline.
- A replica local pin MUST NOT expire with authority retention.
- A resume cursor MUST be bounded by authority diff retention.
- An expired resume cursor MUST return `sync.cursor_expired` and offer a snapshot.
- Receipt retention MUST be separate from all read pins.
- A session token MUST carry a version vector minimum, not a scalar.
- `read_your_writes=true` MUST be an explicit mode.
- In `read_your_writes=true`, the client MUST raise the session minimum to the receipt commit version.
- A replica that cannot apply the session minimum MUST block until it applies or return `read.behind_commit`.
- The default read mode MUST be bounded staleness.
- In the default mode, local visibility MAY lag the terminal receipt.
- A read response MUST report the served version vector and the staleness measure.
- A read that cannot meet its bound MUST return `read.stale` and no data.
- A read MUST NOT silently serve data below its bound.
- A read MUST NOT silently serve data above its exact pin.

**Why.** Authority retention and replica state have different lifetimes. A scalar session minimum cannot express a cross-shard vector. A silent read-your-writes promise conflicts with local apply lag.

**Rejected alternatives.**

- **One pin lifetime for all reads.** Authority retention would expire offline data.
- **A scalar session minimum.** It cannot represent independent shard versions.
- **Silent read-your-writes.** It can return stale data or block without a name.
- **A read that ignores its bound.** It hides replication lag.

**Acceptance.**

- A 24-hour offline session reads and searches its scope.
- A 7-day offline session resumes without a full resync when the cursor is valid.
- An expired authority pin returns `read.pin_expired`.
- An expired sync cursor returns `sync.cursor_expired` and offers a snapshot.
- A session vector minimum is enforced across replicas.
- A read-your-writes read blocks or returns `read.behind_commit`.
- A read over its staleness bound returns `read.stale` and no data.
- A read at an exact pin returns that pin or a named error.

**Essay edits.** 15 must separate local pin from authority retention. 19 must define authority pin expiry. 23 must use a version vector and define read-your-writes. 10 must align local visibility. 32 must state the offline window.

### 7.11 D11. Hydration and leases

**Decision.** Hydration peak storage is part of the device budget. A snapshot content seal is separate from the authority lease term. A replica does not hold an authority write lease. A local tab lease is allowed.

**Rules.**

- Hydration peak storage MUST be part of the device budget.
- A scope class MUST declare `max_hydration_overhead`.
- A scope that cannot fit the peak MUST fail at subscribe with `scope.hydration_budget_exceeded`.
- Hydration MUST build a new generation beside the served generation.
- Hydration MUST publish the new generation with one pointer swap.
- A reader MUST see the old generation or the new generation. It MUST NOT see a mixture.
- A crash MUST leave exactly one published generation.
- A pre-fetched frame MUST wait for a complete verified snapshot.
- The applied watermark MUST advance only across a contiguous prefix of applied frames.
- A duplicate frame MUST be discarded.
- A missing sequence number MUST force a re-request from the last applied sequence.
- A snapshot content seal MUST cover the content digest and the lease term.
- The lease term MUST NOT enter the content digest.
- A sealed snapshot MUST remain valid across a lease change.
- A new authority MAY serve the same sealed snapshot.
- The lease term fences new writes and unsealed in-flight pages only.
- A replica MUST NOT hold an authority write lease.
- A local tab lease MAY exist for multi-tab coordination.
- The local tab lease MUST NOT commit data and MUST NOT fence authority writes.
- One cursor advancer MUST exist per replica identity.
- A follower tab MUST read through shared storage or the leader.

**Why.** A new generation beside the old one doubles peak storage. If the peak is not in the budget, a scope that fits at rest can fail during hydration. If the lease term enters the content digest, a leader change forces a full rehydration of immutable bytes. If a replica holds an authority lease, it becomes a writer.

**Rejected alternatives.**

- **In-place hydration with visibility bits.** A crash can expose a torn state.
- **Lease term in the content digest.** This breaks byte identity.
- **A replica authority lease.** This breaks the read-only rule.
- **Independent tab cursors.** This duplicates tail entries and diverges versions.

**Acceptance.**

- A crash at any phase boundary converges to the same state after restart.
- A reader loop during hydration sees a non-decreasing version and no hole.
- A duplicate frame leaves state unchanged.
- A missing sequence number produces a re-request.
- A corrupted page is rejected and refetched.
- A crash during the swap leaves exactly one published generation.
- A sealed snapshot remains readable after a lease change.
- A scope that exceeds the hydration peak fails at subscribe.
- Two tabs produce one cursor advance and no duplicate tail entries.

**Essay edits.** 15 must include hydration peak in the ceiling and define the tab lease. 16 must separate seal from lease and define the peak budget. 04 must treat the seal as content metadata, not a lease fence.

### 7.12 D12. Migration, backup, and restore

**Decision.** Use one migration framework with three scopes. Key encoding is an annex of the snapshot/page contract. Backup uses a version vector. Restore preserves receipts and event state or marks them expired.

**Rules.**

- One migration framework MUST cover artifact migration, shard format migration, and key encoding migration.
- Key encoding MUST be an annex of the snapshot/page contract.
- Artifact migration MUST rewrite snapshot and page files from version N to N+1.
- Artifact migration MUST produce a new digest and list every layout change.
- Shard format migration MUST follow these steps:
  1. Quiesce writes.
  2. Drain the scheduler.
  3. Rewrite the shard under a new epoch.
  4. Verify checksums.
  5. Flip the pointer.
- Reads MAY continue from replicas at the old format.
- Pointer-flip rollback is allowed only before the first write under the new format.
- After the first new-format write, the store MUST NOT downgrade.
- Backup MUST be a set of ShardSnapshots with a version vector.
- The operator MAY create a global consistency point only after a global write quiesce.
- Backup MUST include receipt, idempotency, event outbox, and audit state.
- Restore MUST preserve terminal receipts and event cursors, or mark them expired with a named error.
- Restore MUST NOT re-execute an effect.
- Restore MUST verify every object before it serves a read.
- Restore MUST write a new epoch.
- Restore MUST NOT mutate the source epoch.
- A backup pin MUST block compaction until the backup completes or fails.
- A lost pin MUST return `ops.backup_pin_lost`.
- A checksum mismatch MUST return `ops.restore_checksum_mismatch` and leave the source epoch unchanged.
- A migration MUST end every authority-known receipt in `Committed`, `Rejected`, or `Conflict`.
- A migration MUST NOT claim to end a client-local outbox entry that the authority never saw.
- A client that reconnects after migration with an old local outbox entry MUST receive `ops.client_outbox_stale`.

**Why.** The essays define four migration models. Four models mean four test matrices and four rollback stories. A backup with one global version cannot exist across independent shards without a global quiesce. A restore that loses receipts can duplicate an effect.

**Rejected alternatives.**

- **Permanent online dual-read.** This doubles read paths and is hard to prove.
- **Pointer rollback after the first new-format write.** This can expose a format that cannot read the new bytes.
- **A global backup point without a quiesce.** This claims a consistency point that the system cannot provide.
- **Restore without receipts.** A retry can re-execute a committed effect.
- **Restore that re-executes effects.** This violates at-most-once intent.

**Acceptance.**

- A crash at every migration step resumes without key loss.
- A rollback before the first new-format write returns the old format.
- A rollback after the first new-format write is refused with `ops.downgrade_not_allowed`.
- A backup completes under write load and records a version vector.
- A restore reproduces every key, value, index entry, receipt, and event cursor at the captured vector.
- A lost pin returns `ops.backup_pin_lost`.
- A checksum mismatch returns `ops.restore_checksum_mismatch`.
- A restore does not re-execute a committed effect.
- A client with an old outbox entry receives `ops.client_outbox_stale` and a rebase path.

**Essay edits.** 05 must make key encoding an annex and remove permanent dual-read. 06 must align the upgrade tool with the framework. 28 must use a version vector, include receipts and events, and restrict rollback. 32 must state no downgrade after the first new-format write.

### 7.13 D13. Manifest and policy catalog

**Decision.** The manifest declares schema, layout, and static configuration. Policy bodies live in a separate content-addressed policy catalog. The active policy version is not part of the manifest digest. The manifest compiler validates schema only. The shard map validates placement.

**Rules.**

- The manifest digest MUST cover schema, layout, and static configuration.
- The manifest digest MUST NOT change when only a rule body changes.
- The active policy version MUST NOT be part of the manifest digest.
- A policy version MUST be a separate content-addressed object.
- The manifest MUST declare rule identifiers and selectors. It MUST NOT embed mutable rule bodies.
- The manifest MAY declare a policy catalog location.
- A scope pin MUST contain the manifest digest, policy version, map version, and scope identity.
- Manifest v1 MUST include these sections:
  - `schema`: schema version, revision, digest.
  - `tables`: user table names, key schema, field schema, limits.
  - `indexes`: field lists, tokenizer identity, positions, filter and search flags, overhead budget, scorer.
  - `rules`: rule identifiers, selectors, effects, scopes, cost bounds, strict flags, retention classes, audit visibility, fact read sets.
  - `facts`: fact identity, canonical type, owner shard, fan-in cap, staleness bound.
  - `operations`: name, target table, request fields, response fields, streaming shape, error set, auth class.
  - `events`: kinds, payload schema, required effect fields, retention class.
  - `receipts`: retention horizon, reason codes, retry classes.
  - `audit`: visibility class, retention class, export class.
  - `projection`: epoch rules, watermark rules, invalidation rules.
  - `codecs`: format versions, tokenizer versions, digest algorithms.
  - `control`: backup, restore, migrate, rule activate, rule rollback, DLQ redrive, quota, audit export.
- The manifest MUST NOT include shard placement.
- The shard map MUST include table-to-shard assignment, bounds, map version, and digest.
- Manifest validation MUST NOT require the shard map.
- A deployment validator MUST bind the manifest digest and the map digest.
- The manifest compiler MUST emit a neutral descriptor.
- The generator MUST generate RPC stubs from the descriptor.
- Control operations MUST use a `control` namespace.
- Control operations MUST use the same generator, error mapping, and authentication as data operations.
- Manifest canonical JSON MUST allow only integers in the i64 range, strings, booleans, arrays, and objects.
- Manifest canonical JSON MUST NOT contain floating point values.
- Manifest canonical JSON MUST use one key order.
- Manifest canonical JSON MUST reject duplicate keys.
- A rule body change MUST create a new policy version and a new policy catalog record.
- A rule body change MUST NOT change the manifest digest.
- A manifest change MUST NOT silently change the active policy version.

**Why.** If the manifest embeds rule bodies, every rule change changes the manifest digest. Every replica then rejects a diff under digest B and rehydrates. A canary cannot serve two policy versions under one manifest. If the manifest validates placement, the manifest and map have a circular dependency. If the manifest contains floats, canonical JSON has no stable byte form.

**Rejected alternatives.**

- **Rules embedded in the manifest.** This breaks canaries and incremental sync.
- **Manifest validates the shard map.** This creates a cycle between 07 and 22.
- **Manifest includes placement.** This duplicates the shard map and creates two truths.
- **Floats in canonical JSON.** This breaks byte identity.
- **Hand-written control RPC.** This creates a second surface that can drift.

**Acceptance.**

- A rule body change produces a new policy version and leaves the manifest digest unchanged.
- Two policy versions serve two cohorts at the same time.
- A scope pin contains the manifest digest, policy version, map version, and scope identity.
- A manifest with a missing table key schema fails with `manifest.key_schema_missing`.
- A manifest with a reserved user table name fails with `manifest.reserved_table_name`.
- A manifest with a float fails with `manifest.float_not_allowed`.
- A map with an unmapped table fails with `map.table_unmapped`.
- A control operation has one generated method and one error map.
- Two clean builds produce byte-identical descriptors.

**Essay edits.** 07 must remove rule bodies from the manifest digest, remove map validation, and add the missing sections. 10 must move retention and reason codes into the manifest. 12 must add audit and staleness sections. 17 must add facts and projection sections. 18 and 19 must add tokenizer, scorer, and sortable fields. 24 must add event schemas and required fields. 25 must use the descriptor and control namespace. 27 must use the policy catalog. 28 must use control operations.

### 7.14 D14. Policy AST, evaluation, and audit cardinality

**Decision.** Freeze the canonical policy AST, evaluation semantics, and cost model. Source syntax is a versioned front end. One production evaluator serves read scope and write validation. A test-only differential oracle is allowed. Policy evaluation is per row. A batch verdict is the conjunction of row verdicts.

**Rules.**

- The canonical policy AST, evaluation semantics, and cost model MUST freeze at 1.0.
- The source syntax MUST be versioned.
- The source syntax MUST map to the AST.
- Execution MUST use the AST, not the source text.
- One production evaluator MUST serve read scope and write validation.
- A test-only differential oracle MAY exist.
- The test oracle MUST NOT enter a production authorization path.
- The test oracle MUST run only in tests, fuzzing, and qualification.
- The evaluator MUST be total. Every expression MUST return a verdict or a named budget failure.
- The evaluator MUST NOT read a clock, draw randomness, perform I/O, inspect the host, or keep state.
- The evaluator MUST use a step budget with named nodes.
- The evaluator MUST use a depth budget.
- The evaluator MUST use a fixed memory arena.
- Budget exhaustion MUST return `policy.budget_exhausted` and map to `Rejected`.
- Budget exhaustion MUST NOT map to `Conflict`.
- Budget exhaustion MUST NOT default to allow.
- Policy evaluation MUST be per row.
- The context MUST bind `before` and `after`.
- Read scope evaluation MUST bind `before` and `after` to the same row.
- Write validation MUST bind `before` to the stored value and `after` to the proposed value.
- A batch verdict MUST be the conjunction of row verdicts.
- If one row rejects, the batch MUST reject and change no row.
- A request MUST produce one receipt.
- A request MUST produce one audit record with a bounded decision list.
- Each decision entry MUST name the row key, rule identity, verdict, and reason code.
- The audit record MUST carry the policy version, manifest digest, map version, projection epoch, and fact versions.
- The evaluator MUST produce a structured explanation, not only a boolean.
- The explanation MUST be stable across native and wasm32.

**Why.** A grammar freeze makes parser details permanent. An AST freeze preserves meaning while allowing syntax sugar. A second production evaluator can diverge and grant data outside the admitting scope. A per-batch single verdict loses the row that failed. A per-row audit record can explode the audit volume.

**Rejected alternatives.**

- **Freeze grammar text.** This makes diagnostics and syntax sugar permanent.
- **A second production evaluator.** This breaks the single-evaluator rule.
- **A per-batch boolean verdict.** This hides the failing row and rule.
- **A per-row audit record for every row.** This creates an audit volume problem for wide writes.
- **Fuel as a public contract.** Fuel is runtime-specific and opaque.

**Acceptance.**

- Two readers with the same AST and fact set return the same verdict.
- A policy with an unknown field fails to compile with `manifest.rule_unknown_path`.
- A policy over budget returns `policy.budget_exhausted` and no write.
- A read scope and a write validation agree for the same row and policy version.
- A batch with one rejected row returns `Rejected` and changes no row.
- The audit record lists the failing row, rule, and reason.
- The test oracle and production evaluator agree on every vector.
- The fuzzer runs 24 hours with no panic, no non-total result, and no nondeterministic verdict.

**Essay edits.** 03 must use per-row evaluation and one request audit record with a decision list. 08 must freeze the AST, not the grammar. 12 must use the audit decision list. 29 must move the second evaluator to a test-only oracle. 32 must freeze the AST and evaluation semantics.

### 7.15 D15. Contracts, annexes, and freeze governance

**Decision.** Keep five core contracts. Add a normative register of annexes. 1.0 freezes every core contract and every annex. The phrase "five contracts" is a summary, not a complete list.

**Rules.**

- The core contracts MUST be manifest, snapshot/page, sync wire, policy, and receipt/error.
- The annexes MUST be key encoding, tokenizer, event envelope, shard map, scope, projection staleness, audit, cost/explain, cursor types, descriptor, read pin, and migration/backup.
- Each annex MUST name its parent contract, version, vector corpus, and migration path.
- 1.0 MUST freeze every core contract and every annex.
- `32-one-zero-freeze.md` MUST list every core contract and every annex.
- `30-qualification-suite.md` MUST carry a vector corpus for every core contract and every annex.
- No essay may say "freezes" without a register row.
- No frozen surface may depend on a Rust type layout, a serde default, or an enum discriminant.
- A break to a frozen surface MUST create a new major contract version and a migration note.
- An additive change to a minor contract MUST be documented and must not change old bytes.

**Why.** The essays already freeze at least ten surfaces. The qualification corpus, migration matrix, and support window depend on the complete list. A "five contracts" claim that omits key encoding, tokenizer, event envelope, map, audit, and cursor types is false.

**Rejected alternatives.**

- **Five contracts only.** This leaves the most important bytes outside the freeze.
- **Freeze everything without a register.** The reader cannot tell what is frozen.
- **Freeze the Rust API.** This freezes implementation mistakes for the life of 1.x.

**Acceptance.**

- Every frozen surface has a register row.
- Every row has a version, spec, changelog, vectors, and migration path.
- `32` lists every row.
- `30` runs a vector for every row on native and wasm32.
- A build check fails when an essay says "freeze" without a register row.

**Essay edits.** 05, 12, 13, 17, 18, 20, 22, 24, and 25 must name their register row. 30 and 32 must use the full register. 18 and 24 must stop calling their surfaces "in practice" frozen and name the annex.

### 7.16 D16. Error and receipt registry

**Decision.** Use one canonical error registry. Every error code has one cause, one receipt mapping, one retry class, and one wire shape. Codes use lowercase dot namespaces. New codes are additive.

**Rules.**

- One canonical error registry MUST define every error code.
- Codes MUST use lowercase dot namespaces.
- One code MUST describe one cause.
- Two codes MUST NOT describe the same cause.
- Each code MUST declare its kind, receipt state, retry class, terminal flag, and wire shape.
- A code with no receipt MUST say "no receipt."
- A code that maps to a receipt MUST say which state.
- A code that maps to `Conflict` MUST name the current key version field.
- A code that maps to `Rejected` MUST say whether the idempotency key is consumed.
- A code that maps to `Queued` MUST declare a retry class and a deadline.
- The registry MUST replace all earlier names.
- A new code MUST NOT reuse an old name.
- A removed code MUST stay reserved.
- The build MUST fail when a code is missing from the registry.
- The build MUST fail when a code appears twice.
- The build MUST fail when a receipt references an unknown code.

**Why.** The essays use at least five naming styles and several duplicate names. A client cannot branch on prose. A frozen taxonomy with duplicates is not frozen. A retry class is part of the receipt contract, not a comment.

**Rejected alternatives.**

- **CamelCase error names.** They do not namespace by domain.
- **Slash namespaces mixed with dot namespaces.** They split the error surface.
- **Prose messages as the contract.** They cannot be frozen or translated.
- **Duplicate names for one cause.** They create client divergence.

**Acceptance.**

- The build rejects an unknown code.
- The build rejects a duplicate code.
- The registry covers every error in the essays.
- Every receipt state maps to one or more codes.
- Every code has one retry class.
- A golden vector pins each code and its wire shape.

**Essay edits.** All essays must use the registry. The specific renames are in section 8.3.

### 7.17 D17. Version axes and provenance

**Decision.** Use qualified version names. Use a provenance block with a required subset per artifact. Separate content digest from content seal.

**Rules.**

- oxkv MUST use qualified version names.
- A bare "version" MUST NOT appear in a specification or a public API.
- The version axes MUST be grouped into contract axes, data axes, identity axes, fence axes, and session axes.
- Every artifact MUST carry the required provenance subset.
- A reader MUST reject an artifact with a missing required provenance field.
- A reader MUST reject an artifact with an unknown required version.
- A content digest MUST cover the artifact bytes.
- A content seal MUST cover the content digest and the authority lease term.
- A content seal MUST NOT enter the content digest.
- A reader MUST verify the content digest before it verifies the seal.
- A reader MUST verify the seal before it serves the artifact.
- A page identity MUST use the qualified `page_id`.
- A page sequence MUST use the qualified `page_sequence`.
- A cursor MUST name the version fields it carries.
- A receipt MUST name the commit version, policy version, manifest digest, and audit reference.

**Why.** The essays call at least twenty different things "version." A bare version in a contract invites the wrong field. A lease term in the content digest breaks byte identity. A missing provenance field makes a cross-shard read ambiguous.

**Rejected alternatives.**

- **Three version axes.** The system has more than twenty.
- **A single global version.** Independent shards cannot share a scalar order.
- **Lease term in the content digest.** This breaks immutable bytes.
- **Provenance outside the artifact.** A reader cannot validate the artifact alone.

**Acceptance.**

- A lint fails on a bare "version" in a public contract.
- A snapshot with a missing manifest digest fails with `format.missing_provenance`.
- A snapshot with an unknown format version fails with `format.unsupported_version`.
- A page with a missing `page_id` fails with `format.corrupt_page`.
- A content digest mismatch fails before the seal check.
- A valid content digest with a stale seal fails with `replication.stale_epoch`.
- A receipt names every required version field.

**Essay edits.** 02, 05, 06, 07, 14, 16, 19, 23, and 32 must use the version axes and provenance fields.

### 7.18 D18. Reserved keyspace and cursor types

**Decision.** Use the `0xFF` system marker and registered system identifiers. Use `_catalog` as the only top-level system table in 1.0. Use one cursor type per cursor role.

**Rules.**

- The system marker MUST be `0xFF`.
- The system identifier MUST be one byte.
- The system identifiers MUST be:
  - `0x01` receipt
  - `0x02` event
  - `0x03` audit
  - `0x04` projection
  - `0x05` index
  - `0x06` lease
  - `0x07` statistics
- The catalog table `_catalog` MUST use the same marker and identifiers.
- The catalog table MUST store manifest, policy, map, and statistics documents in its user key space.
- The catalog table MUST use the same system identifiers for receipt, event, audit, and lease.
- A user table name MUST match `[a-z][a-z0-9_]*`.
- A user table name MUST NOT start with `_`.
- A user key MUST NOT use `0xFF` as a component tag.
- A system key MUST use one registered system identifier.
- A system sub-namespace MUST NOT appear in a user scope.
- A data-shard system sub-namespace MUST belong to the shard that owns its user table.
- A catalog system sub-namespace MUST belong to the catalog shard.
- The manifest validator MUST reject a reserved user table name with `manifest.reserved_table_name`.
- The key validator MUST reject an unknown system identifier with `key.unknown_system_id`.
- The cursor types MUST be:
  - `KeyCursor`
  - `SyncCursor`
  - `HydrationCursor`
  - `ReceiptCursor`
  - `EventCursor`
  - `SearchCursor`
  - `ProjectionCursor`
  - `CostCursor`
- Each cursor type MUST have its own versioned token format, fields, expiry rule, and named error.
- A cursor MUST NOT carry fields from another cursor type.
- A cursor token MUST be opaque to the client.
- A cursor token MUST be tamper-evident.
- A cursor token MUST be valid after an authority restart, subject to its retention.

**Why.** A single "cursor" type cannot carry a key range, a version vector, a query fingerprint, and a receipt identity. A top-level system table per subsystem breaks contiguous shard ranges. A reserved name scheme without a key encoding rule is not enforceable.

**Rejected alternatives.**

- **One cursor type for all roles.** The fields and expiry rules conflict.
- **Top-level system tables.** Their prefixes may not be contiguous with the user table.
- **Reserved user table names only.** They do not define where system keys live.
- **A signed cursor only.** A signature does not solve retention or field shape.

**Acceptance.**

- A user key containing `0xFF` fails with `key.reserved_system_marker`.
- A system key with an unknown identifier fails with `key.unknown_system_id`.
- A user table named `_receipt` fails with `manifest.reserved_table_name`.
- A catalog key uses the catalog prefix and a registered catalog identifier.
- Each cursor type has a golden vector.
- A cursor from one type fails in another type with `sync.cursor_type_mismatch`.
- A tampered cursor fails with `sync.cursor_invalid`.
- A cursor past retention fails with `sync.cursor_expired`.

**Essay edits.** 01 must define the cursor types. 05 must add the system marker and identifiers. 06 must use `page_id` and `page_sequence`. 10 must use `ReceiptCursor`. 14 must use `SyncCursor`. 16 must use `HydrationCursor`. 17 must use `ProjectionCursor`. 18 and 19 must use `SearchCursor`. 20 must use `CostCursor`. 24 must use `EventCursor`. 27 must use the catalog table.

### 7.19 D19. Audit, authentication, and security boundaries

**Decision.** Audit every write enforcement verdict in the per-shard audit chain. Use a separate access log for read scope evaluation. Authenticate before authorization. Run the external audit witness asynchronously. Use step budgets, not fuel. No regular expressions in the 1.0 search language.

**Rules.**

- Every write enforcement verdict MUST have one record in the per-shard audit chain.
- The audit record MUST carry:
  - request identity
  - idempotency key
  - principal
  - shard
  - base version
  - policy version
  - manifest digest
  - map version
  - projection epoch
  - fact versions
  - rule identity
  - decision
  - reason code
  - input digest
  - previous hash
  - record hash
- The audit chain MUST be append-only and hash-linked per shard.
- The audit chain MUST have signed checkpoints.
- The audit chain MUST support an external witness.
- The external witness MUST run asynchronously after commit.
- The commit path MUST NOT wait for the witness.
- A failed audit append MUST reject the write with `audit.unavailable`.
- A failed audit append MUST NOT write a receipt and MUST NOT change state.
- Read scope evaluation MUST use a separate access log.
- The access log MUST NOT write to the write audit chain.
- The access log MUST have its own retention class.
- Authentication MUST run before authorization.
- An unauthenticated request MUST NOT receive a receipt.
- A denied request MUST receive `Rejected` with a coarse reason code.
- A denial MUST NOT reveal whether a hidden record exists.
- A request signature MUST bind the body, idempotency key, base version, nonce, and expiry.
- A retry with the same idempotency key and same body MUST return the original receipt, even after the original signature expires.
- A replay with a used nonce and a different body MUST fail with `auth.replayed_request`.
- A revoked credential MUST fail after the revocation staleness bound.
- Policy evaluation MUST use a step budget with named nodes.
- A public contract MUST NOT use the word "fuel."
- The 1.0 search language MUST NOT include regular expressions.
- A later release MAY add regular expressions with a new contract version.
- The evaluator MUST read one pinned fact projection.
- The evaluator MAY read a bounded set of facts from that projection.
- Secrets MUST NOT enter policy, audit, error text, traces, or events.
- The manifest and policy MUST name key identifiers, not key material.
- A snapshot SHOULD be encrypted at rest. The encryption metadata MUST be reserved in the header.

**Why.** A write verdict without an audit record is not enforceable. A read scope evaluation at scale cannot enter the immutable write chain. A synchronous witness puts the network on the commit path. A replay check that rejects an idempotent retry breaks the receipt contract. A regex engine expands the search surface and the frozen contract.

**Rejected alternatives.**

- **Audit every read verdict in the write chain.** This is a volume and availability problem.
- **A synchronous external witness.** This violates the no-network-on-commit rule.
- **Fuel as a public budget.** This is runtime-specific and opaque.
- **Regex in 1.0.** It needs a new automaton contract, cost model, and vectors.
- **A denial that confirms the record exists.** This is an information leak.

**Acceptance.**

- A committed write has one audit record.
- A rejected write has one audit record.
- A read scope evaluation writes to the access log, not the audit chain.
- An unauthenticated request returns `auth.unauthenticated` and no receipt.
- A replayed request with a used nonce and a different body fails with `auth.replayed_request`.
- A retry with the same key and body returns the original receipt.
- A failed audit append rejects the write with `audit.unavailable`.
- A failed audit append leaves no receipt and changes no state.
- The audit chain verifies to the last checkpoint.
- An external witness receives a checkpoint after commit.
- A policy with a regex fails to compile with `policy.regex_not_supported`.
- A policy with a network import fails to compile.
- A 24-hour policy fuzz run finds no panic and no native/wasm divergence.

**Essay edits.** 12 must use the decision list and audit fields. 29 must use the access log, async witness, step budget, and no-regex rule. 08 must use the step budget. 18 and 19 must state no regex. 20 must use the query budget, not policy fuel.

### 7.20 D20. Write scheduler

**Decision.** A request has `queued`, `in_flight`, and `terminal` states. At most one request per key is in flight. Multiple requests per key may be queued. Per-key FIFO orders all non-terminal requests. The scheduler does not evaluate policy and does not write terminal receipt states.

**Rules.**

- A request MUST have one of three scheduler states: `queued`, `in_flight`, `terminal`.
- At most one request per key MAY be `in_flight`.
- Multiple requests per key MAY be `queued`.
- A request MUST NOT be selected while an earlier request on the same key is non-terminal.
- Per-key FIFO MUST order all non-terminal requests.
- Commit order MUST equal arrival order after terminal removals.
- A blocked key MUST NOT delay a ready request on a different key in the same shard.
- A blocked shard MUST NOT reduce another shard's selections.
- An explicit dependency edge MUST name an earlier request in the same shard.
- Admission MUST reject a cycle, a forward reference, a cross-shard edge, and fan-in overflow with a named error.
- The scheduler MUST NOT infer a dependency.
- Each call MUST inspect at most `B` positions and `D` edges.
- Exhaustion MUST return a partial decision with `more_work`.
- A ready request MUST be selected within the declared starvation bound.
- The scheduler MUST be a pure function of state and budget.
- The scheduler MUST NOT read a clock, draw randomness, or perform I/O.
- The scheduler MUST NOT evaluate policy.
- The scheduler MUST NOT write a terminal receipt state.
- The scheduler MUST NOT commit data.
- The scheduler MUST return one reason for each unselected request.

**Why.** "Unresolved" in the earlier essay conflates queued and in-flight. If I1 means all non-terminal requests, a per-key queue cannot exist. If it means in-flight, the FIFO rule works. A scheduler that evaluates policy or writes receipts becomes a second authority.

**Rejected alternatives.**

- **At most one unresolved request per key.** This forbids a per-key queue.
- **A dependency DAG with inferred edges.** This makes a pure function stateful and can reorder writes incorrectly.
- **Priorities in the scheduler.** This adds starvation and aging state.
- **A scheduler that commits.** This creates a second authority.

**Acceptance.**

- A property test proves per-key FIFO across all interleavings.
- A blocked key does not delay a disjoint ready key in the same shard.
- A blocked shard does not reduce another shard's selections over N calls.
- Golden vectors prove equal state and budget yield equal batches on native and wasm32.
- Instrumentation proves the `B` and `D` bounds on every call.
- A test proves the starvation bound for every ready request.
- Admission rejects cycles, forward edges, cross-shard edges, and fan-in overflow.
- Replay rebuilds the batch sequence with no divergence.
- Every unselected request carries one reason.
- A build gate rejects clock, randomness, and I/O in the scheduler.

**Essay edits.** 11 must define the three scheduler states and change I1 to in-flight. 10 must align the outbox queue with the scheduler states. 09 must align base-version checks with the commit path.

### 7.21 D21. Native and wasm parity

**Decision.** One portable core runs on native and wasm32. Platform adapters supply storage, time, entropy, scheduling, and transport. No platform conditional may appear on a semantic path. Native and wasm32 produce identical bytes and identical verdicts.

**Rules.**

- All semantics MUST live in one core library.
- The core MUST compile for native and wasm32.
- The core MUST NOT fork behavior on target.
- An adapter MUST supply only storage, time, entropy, scheduling, and transport.
- An adapter MUST NOT decide a verdict or shape a byte.
- The core MUST read time and entropy through interfaces.
- A test adapter MUST supply scripted time and entropy.
- The core MUST use canonical encoding with fixed-width integers and explicit byte order.
- The core MUST NOT use floating point in a frozen format.
- The core MUST NOT require 64-bit atomics that wasm32 cannot provide.
- Native MAY use threads. Results MUST NOT depend on thread count.
- Browser MAY lack threads. Results MUST NOT depend on thread availability.
- The same input sequence MUST produce identical observable outputs on both targets.
- A snapshot written on one target MUST hydrate the other target.
- A re-encode MUST produce identical bytes.
- A fault MUST produce the same named error and terminal state on both targets.
- An offline replica MUST answer reads and searches from its scope as a connected replica would at the same version.
- The qualification suite MUST run the same vectors on both targets.

**Why.** The five frozen contracts are byte contracts. A contract that holds on one target and fails on another is not frozen. A platform conditional on a semantic path is a hidden second implementation.

**Rejected alternatives.**

- **Two implementations.** They diverge at the first edge case.
- **A native fast path that skips a check.** It passes benchmarks and breaks portability.
- **A wasm-only core with a native wrapper.** It does not prove byte identity.
- **A platform-specific clock or randomness source inside the core.** It makes verdicts non-reproducible.

**Acceptance.**

- One input log produces byte-identical keys, snapshots, pages, wire frames, receipts, and errors on native and wasm32.
- A snapshot from one target hydrates the other.
- A static check fails when a core module reads the clock, entropy, environment, or locale directly.
- A static check fails when a platform conditional appears on a semantic path.
- A native-to-wasm-to-native round trip preserves bytes and results.
- The same input under two executors produces the same output.
- Threads on native and no threads on wasm produce the same result.
- A failing vector on one target fails the suite.

**Essay edits.** 02 must use the version axes, injected time, and the test-only oracle rule. 01 and 06 must use canonical encoding. 14 and 25 must use sans-IO cores. 30 must run vectors on both targets.

### 7.22 D22. Qualification suite and falsifiers

**Decision.** The qualification suite starts in wave 0 and grows with every contract. Golden vectors are append-only per contract version. Falsifiers measure the claims before the freeze.

**Rules.**

- The qualification suite MUST start in wave 0.
- The suite MUST run one scenario matrix on native, browser, and native again.
- A scenario MUST NOT have a target-specific expectation.
- Fault injection MUST cover link cut, cursor expiry, conditional-write loss, corrupt page, truncated frame, crash at each commit step, and duplicate delivery.
- Control MUST use a logical clock, a fixed seed, canonical ordering, and normalized traces.
- The suite MUST hold an append-only golden corpus for every core contract and every annex.
- A released vector MUST NOT change. A break MUST create a new contract version and a new vector.
- CI MUST reject an edit to a released vector.
- The suite MUST run a differential comparator across targets and versions.
- The suite MUST fuzz every decoder and the policy evaluator.
- The fuzz oracle MUST check totality, determinism, step monotonicity, memory bound, and native/wasm parity.
- The suite MUST produce one report and one narration from the same trace.
- The browser tier MUST gate the release. The pull request tier MAY run one browser target. The nightly tier MUST run the full browser matrix.
- The falsifier suite MUST measure sync amplification, policy cost per write, conflict rate, cold start, and read p99.
- Each falsifier MUST name one workload, one definition, one threshold, and one action.
- A falsifier breach MUST fail the release gate.
- A missing falsifier number MUST delay the tag.

**Why.** A freeze is credible only when an outside party can check it. A suite that cannot fail is a demo. A vector that changes after release is not frozen. A falsifier that has no threshold is a number, not evidence.

**Rejected alternatives.**

- **A separate harness per target.** It creates two definitions of passing.
- **Regenerated vectors.** A bug can become the oracle.
- **A nightly-only browser tier.** The ABI claim stays unproven for pull requests.
- **A benchmark without thresholds.** It cannot falsify anything.

**Acceptance.**

- A native-to-browser-to-native round trip produces identical export bytes.
- CI rejects an edit to a released vector.
- Every core contract and annex has a versioned vector corpus.
- An older artifact opens and reads its recorded values.
- A newer artifact fails with `format.unsupported_version`.
- A conflicting write ends `Conflict` with the current key version.
- A rule change is checked against the pinned policy version and writes one audit record.
- A disconnect at any injected frame resumes without a full resync.
- A customer can run the suite offline in one command.
- The falsifier report names the slope, interval, verdict, hardware class, toolchain version, and commit.
- A crossed threshold fails the gate.

**Essay edits.** 30 must use the full contract register and start in wave 0. 31 must use the scope budget, document-local scoring, and coverage modes. 32 must use the suite and falsifiers as hard prerequisites.

### 7.23 D23. Freeze and support policy

**Decision.** 1.0 freezes every core contract and every annex. A reader accepts the current format and one back. The wire keeps one extra read version for a major. The support window is published.

**Rules.**

- 1.0 MUST freeze every core contract and every annex.
- A 1.x reader MUST accept the current format and one back.
- A 1.x reader MUST reject an unknown major version with `format.unsupported_version`.
- A 1.x wire peer MUST accept the current protocol and one extra read version for the major.
- An additive policy AST change MAY be a minor release only when the AST does not change.
- An additive error code MAY be a minor release.
- A change to a stored byte layout MUST be a major release and a migration.
- The 1.x line MUST receive fixes for 24 months after the next major.
- The last minor MUST receive security fixes for 12 months.
- Two minors MUST stay supported.
- A format MUST be removed only at a major, after 12 months of notice, and only with an automated migration.
- A downgrade after the first write under a new format MUST NOT be supported.
- The deprecation and support policy MUST be published before the tag.

**Why.** Stored bytes and deployed clients do not recompile. A semver promise that only covers the crate is not enough. A downgrade after a new-format write can lose data.

**Rejected alternatives.**

- **Freeze the Rust API.** It freezes implementation mistakes.
- **Freeze only the bytes.** A deployed browser cannot follow a grammar change.
- **Never freeze.** The support matrix becomes a product.
- **Unlimited downgrade.** It doubles the reader matrix and can lose data.

**Acceptance.**

- A v1 snapshot opens on the release build and the previous minor.
- A 1.0 wire client resumes after seven days offline without a full resync.
- An unknown version returns `format.unsupported_version` and applies no record.
- A migration interrupted at every step resumes without key loss.
- A downgrade after the first new-format write returns `ops.downgrade_not_allowed`.
- The support policy is published before the tag.

**Essay edits.** 32 must list every frozen surface and the support window. 06 must align its reader window. 28 must align migration and downgrade. 30 must carry old artifacts.

### 7.24 D24. Revised work sequence

**Decision.** Use contract waves and implementation waves. Do not use the linear phase list as a gate. A contract precedes every implementation that depends on it. A wave gate must not require a later wave.

**Rules.**

- The owner MUST declare a contract node before any implementation that depends on it.
- An implementation MAY use a fake behind a declared contract.
- A wave gate MUST NOT require a test from a later wave.
- A wave MUST NOT close on a test that needs a later wave.
- The qualification harness MUST start in wave 0.
- The falsifier harness MAY start early. Its gate stays before wave 7.
- The Phase D milestone MUST say "reads exactly its query." Search joins the milestone in Phase E.
- Item 18 MUST start only after the item 19 read-pin contract is declared.
- The manifest compiler MUST stage its emitters. Layout and indexes start in wave 1. Scope starts in wave 3. RPC starts in wave 5.
- The error registry MUST stay a candidate until 32 freezes it. New codes MUST be additive.
- A contract MAY be declared in a wave before its implementation.
- An implementation MUST NOT define a new contract behavior.

**Wave table.**

| Wave | Contract work | Implementation work | Gate |
| --- | --- | --- | --- |
| 0 | Registries, ADRs, and harness | Fault harness and vector skeleton | Registries approved and DAG checked in |
| 1 | 05, 06, 07, 08, 09, 13, 19 pin, 22, 14, 10, 24, 17, 29 auth/audit, and 25 layering | 01, 02, 03, and 04 with one storage backend | Cross-contract review and candidate vectors |
| 2 | 11, 12, and 27 contract | 09, 10 in-process, 11, 12, 17, 24 outbox, and 27 | Write terminal state, atomic event, audit record, and local projection |
| 3 | 15, 16, 18, 19, 20, and 23 | 13, 14, 15, 16, 18, 19, 20, and 23 | Offline scope read/search, resume, fallback, and cost explain |
| 4 | 21 and final 22 | 21, 22, and 23 SLO | Leader kill, stale epoch, and bounded staleness |
| 5 | 26, 27, and 28 contract | 24 consumer, 25, 26, and 27 | Email demo, control RPC, and canary rollback |
| 6 | 28, full 29, 30, and 31 | 28, 29, 30, and 31 | Backup/restore, migration, fuzz, and falsifiers |
| 7 | 32 | Final qualification | Freeze register complete and support policy published |

**Freeze states.**

- `draft`: may break.
- `candidate`: versioned. A break needs a new version and a migration note.
- `frozen`: no break inside 1.x.
- Phase B and Phase C items become candidates.
- `32-one-zero-freeze.md` freezes them.

**Why.** The current phase list has cycles. Phase D includes search, but search is Phase E. Phase E builds the index before the read contract. Phase A builds multi-backend tiering before the formats are versioned. Phase B compiles scope and RPC before those contracts exist. Phase C freezes the error taxonomy before later errors exist. Phase H depends on Phase I.

**Rejected alternatives.**

- **Keep the linear phase list.** The gates remain circular.
- **Move all contracts to Phase I.** Implementation would guess the bytes.
- **Let each essay define its own order.** The result is another cycle.

**Acceptance.**

- A dependency graph is checked in.
- No wave gate requires a later wave.
- Every implementation node has a declared contract.
- The qualification harness runs in wave 0.
- A failing contract candidate can still change before wave 7.
- A frozen contract cannot change without a new major version.

**Essay edits.** `roadmap.md` must use the wave table and the dependency rules. 30 must start in wave 0. 31 must run early. 32 must gate on the full register.

## 8. Shared registries

### 8.1 Provenance block

| Field | Type | Required for | Notes |
| --- | --- | --- | --- |
| `format_version` | major.minor | Snapshot, page | Contract version |
| `key_encoding_version` | integer | Snapshot, page, cursor | Annex version |
| `manifest_digest` | 32-byte hash | All artifacts | Schema and layout |
| `map_version` | integer | All routing artifacts | Shard map version |
| `map_digest` | 32-byte hash | All routing artifacts | Shard map digest |
| `policy_version` | 32-byte hash | Scope snapshot, verdict, event | Content address |
| `shard_id` | bytes | Shard snapshot, page | Opaque and stable |
| `prefix_bounds` | bytes pair | Shard snapshot, page | Inclusive lower and exclusive upper |
| `commit_version` | integer | Shard snapshot, page | Monotonic per shard |
| `version_vector` | map | Scope snapshot, read pin | One entry per shard |
| `scope_identity` | 32-byte hash | Scope snapshot, pin, cursor | Canonical hash |
| `tokenizer_versions` | map | Index and search artifacts | One entry per field |
| `statistics_version` | integer or hash | Search artifacts | Optional in 1.0 |
| `projection_epoch` | integer or hash | Policy-filtered artifacts | Fact projection |
| `projection_watermarks` | map | Policy-filtered artifacts | One entry per fact source |
| `content_digest` | 32-byte hash | Snapshot, page, manifest, map, policy | Artifact bytes only |
| `content_seal` | structure | Snapshot, page | Content digest plus lease term plus signature |
| `page_id` | 32-byte hash | Page | Deterministic page identity |
| `page_sequence` | integer | Page | Strictly increasing in one snapshot |

### 8.2 Version axes

| Group | Axis | Example | Used by |
| --- | --- | --- | --- |
| Product | `crate_version` | `1.0.0` | Build and package |
| Contract | `key_encoding_version` | `1` | Keys, pages, cursors |
| Contract | `snapshot_page_version` | `1.0` | Snapshots and pages |
| Contract | `sync_wire_version` | `1` | Wire frames and resume |
| Contract | `manifest_schema_version` | `oxkv/manifest/v1` | Manifest |
| Contract | `policy_ast_version` | `policy-ast/v1` | Policy execution |
| Contract | `receipt_error_version` | `receipt-error/v1` | Receipts and errors |
| Contract | `descriptor_version` | `descriptor/v1` | RPC descriptor |
| Contract | `map_schema_version` | `oxkv/map/v1` | Shard map |
| Contract | `tokenizer_version` | `tokenizer/v1` | Search index |
| Contract | `event_version` | `oxkv/event/v1` | Business events |
| Contract | `audit_version` | `audit/v1` | Audit records |
| Contract | `scope_version` | `scope/v1` | Scope identity and diffs |
| Contract | `projection_version` | `projection/v1` | Projection staleness |
| Contract | `cost_version` | `cost/v1` | Explain and cost |
| Data | `commit_version` | `1842` | Shard commit |
| Data | `key_version` | `7` | One key record |
| Data | `snapshot_version` | `1842` | Shard snapshot |
| Data | `scope_version_vector` | `{S1: 18, S2: 9}` | Cross-shard read |
| Data | `projection_epoch` | `41` | Fact projection |
| Data | `fact_version` | `12` | One fact |
| Data | `statistics_version` | `3` | Search statistics |
| Identity | `manifest_revision` | `57` | Manifest history |
| Identity | `manifest_digest` | `b3:...` | Manifest bytes |
| Identity | `policy_version` | `b3:...` | Policy AST bytes |
| Identity | `map_digest` | `b3:...` | Map bytes |
| Identity | `descriptor_digest` | `b3:...` | Descriptor bytes |
| Fence | `lease_term` | `9` | Shard ownership |
| Session | `session_minimum` | `{S1: 18}` | Client session |
| Session | `replica_high_water` | `{S1: 17}` | Replica state |

### 8.3 Error and receipt registry

The receipt state column applies when the error occurs in an admitted write request. A read, sync, or compile error has no receipt. A write error with no valid idempotency key has no receipt. A failed audit append rejects the write before the authority can write a durable receipt. The `audit.unavailable` row therefore has no receipt.

| Code | Kind | Receipt state | Retry class |
| --- | --- | --- | --- |
| `key.unknown_version` | Key | Rejected | Never |
| `key.malformed` | Key | Rejected | Never |
| `key.non_canonical` | Key | Rejected | Never |
| `key.outside_table` | Key | Rejected | Never |
| `key.invalid_utf8` | Key | Rejected | Never |
| `key.too_large` | Key | Rejected | Never |
| `key.not_found` | Key | None | Never |
| `key.reserved_system_marker` | Key | Rejected | Never |
| `key.unknown_system_id` | Key | Rejected | Never |
| `format.unsupported_version` | Format | None | Never |
| `format.missing_required_feature` | Format | None | Never |
| `format.corrupt_page` | Format | None | Refetch |
| `format.page_out_of_bounds` | Format | None | Never |
| `format.truncated_snapshot` | Format | None | Refetch |
| `format.missing_provenance` | Format | None | Never |
| `format.page_too_large` | Format | None | Never |
| `manifest.unsupported_schema` | Manifest | None | Never |
| `manifest.unknown_field` | Manifest | None | Never |
| `manifest.duplicate_name` | Manifest | None | Never |
| `manifest.reserved_table_name` | Manifest | None | Never |
| `manifest.key_schema_missing` | Manifest | None | Never |
| `manifest.float_not_allowed` | Manifest | None | Never |
| `manifest.index_unknown_field` | Manifest | None | Never |
| `manifest.index_scope_unreachable` | Manifest | None | Never |
| `manifest.rule_unknown_path` | Manifest | None | Never |
| `manifest.rule_unbounded` | Manifest | None | Never |
| `manifest.operation_cross_shard` | Manifest | None | Never |
| `manifest.operation_unknown_field` | Manifest | None | Never |
| `manifest.artifact_drift` | Manifest | None | Regenerate |
| `manifest.policy_catalog_missing` | Manifest | None | Never |
| `manifest.json_syntax` | Manifest | None | Never |
| `manifest.missing_field` | Manifest | None | Never |
| `manifest.type_mismatch` | Manifest | None | Never |
| `manifest.invalid_value` | Manifest | None | Never |
| `policy.parse_error` | Policy | None | Never |
| `policy.unknown_field` | Policy | Rejected | Never |
| `policy.budget_exhausted` | Policy | Rejected | Never |
| `policy.not_total` | Policy | Rejected | Never |
| `policy.ast_unsupported` | Policy | None | Never |
| `policy.regex_not_supported` | Policy | None | Never |
| `rule.compile_failed` | Rule | Rejected | After fix |
| `rule.dry_run_failed` | Rule | Rejected | After fix |
| `rule.approval_required` | Rule | Rejected | After approval |
| `rule.rollout_limit` | Rule | Rejected | After rollout |
| `write.missing_base_version` | Write | None | Never |
| `write.stale_base_version` | Write | Rejected | After re-read |
| `write.version_conflict` | Write | Conflict | After rebase |
| `write.idempotency_key_reuse` | Write | None | Never |
| `write.queue_timeout` | Write | Rejected | After re-read |
| `write.operation_unknown` | Write | Rejected | Never |
| `write.batch_too_large` | Write | Rejected | Never |
| `write.value_too_large` | Write | Rejected | Never |
| `shard.cross_mutation_rejected` | Shard | Rejected | Never |
| `receipt.expired` | Receipt | None | After re-read |
| `receipt.not_found` | Receipt | None | Resubmit |
| `receipt.state_invalid` | Receipt | None | Never |
| `outbox.full` | Outbox | None | After drain |
| `outbox.dependency_cycle` | Outbox | None | Never |
| `outbox.cross_shard_edge` | Outbox | None | Never |
| `outbox.forward_reference` | Outbox | None | Never |
| `outbox.fan_in_exceeded` | Outbox | None | Never |
| `outbox.duplicate_request` | Outbox | None | Never |
| `projection.lagging` | Projection | Queued | Backoff |
| `projection.epoch_invalid` | Projection | Rejected | After rebuild |
| `projection.watermark_regression` | Projection | Rejected | After rebuild |
| `projection.checksum_failed` | Projection | Rejected | After rebuild |
| `projection.client_write_rejected` | Projection | Rejected | Never |
| `projection.fan_in_exceeded` | Projection | Rejected | Never |
| `projection.read_set_exceeded` | Projection | Rejected | Never |
| `projection.fact_set_mismatch` | Projection | None | After rebuild |
| `projection.fact_type_mismatch` | Projection | None | After rebuild |
| `projection.feed_sequence_invalid` | Projection | None | After rebuild |
| `authorization.stale_facts` | Policy | Rejected | After catch-up |
| `scope.over_limit` | Scope | None | Never |
| `scope.device_budget_exceeded` | Scope | None | Never |
| `scope.hydration_budget_exceeded` | Scope | None | Never |
| `scope.identity_mismatch` | Scope | None | Never |
| `scope.policy_version_unknown` | Scope | None | After policy sync |
| `scope.shard_unreachable` | Scope | None | Backoff |
| `scope.system_key_rejected` | Scope | None | Never |
| `scope.remove_unknown_member` | Scope | None | Never |
| `scope.unknown_shard` | Scope | None | Never |
| `scope.missing_pin_shard` | Scope | None | Never |
| `sync.unsupported_version` | Sync | None | Never |
| `sync.unknown_capability` | Sync | None | Never |
| `sync.scope_rejected` | Sync | None | After change |
| `sync.cursor_expired` | Sync | None | Snapshot fallback |
| `sync.cursor_invalid` | Sync | None | New cursor |
| `sync.cursor_type_mismatch` | Sync | None | New cursor |
| `sync.gap_too_large` | Sync | None | Snapshot fallback |
| `sync.snapshot_unavailable` | Sync | None | Backoff |
| `sync.rate_limited` | Sync | None | Backoff |
| `sync.unauthorized` | Sync | None | After auth |
| `sync.shard_unavailable` | Sync | None | Backoff |
| `sync.negotiation_required` | Sync | None | Never |
| `sync.incompatible_manifest` | Sync | None | Never |
| `read.pin_expired` | Read | None | New pin |
| `read.pin_unreadable` | Read | None | New pin |
| `read.behind_commit` | Read | None | Backoff |
| `read.not_hydrated` | Read | None | Backoff |
| `read.not_resident` | Read | None | Backoff |
| `read.stale` | Read | None | Backoff |
| `search.pin_expired` | Search | None | New pin |
| `search.cursor_invalid` | Search | None | New query |
| `search.fanout_exceeded` | Search | None | Narrow query |
| `search.index_mismatch` | Search | None | Rebuild |
| `search.tokenizer_mismatch` | Search | None | Rebuild |
| `search.tokenizer_unknown` | Search | None | Never |
| `search.token_budget_exceeded` | Search | None | Narrow query |
| `index.overhead_exceeded` | Search | Rejected | Never |
| `query.budget_exceeded` | Query | None | Narrow query |
| `map.unknown_version` | Map | None | Never |
| `map.overlap` | Map | None | Never |
| `map.gap` | Map | None | Never |
| `map.duplicate_id` | Map | None | Never |
| `map.empty_range` | Map | None | Never |
| `map.table_unmapped` | Map | None | Never |
| `map.table_split_unsupported` | Map | None | Never |
| `replication.stale_epoch` | Replication | Rejected | After re-acquire |
| `replication.lease_expired` | Replication | Rejected | After re-acquire |
| `replication.lease_held` | Replication | None | After hint |
| `replication.only_copy_eviction` | Replication | None | Operator action |
| `replication.tier_unavailable` | Replication | None | Backoff |
| `replication.warmup_budget_exceeded` | Replication | None | Continue with cursor |
| `replication.placement_violation` | Replication | Rejected | Never |
| `event.unknown_version` | Event | None | Never |
| `event.retention_gap` | Event | None | Snapshot backfill |
| `event.poison` | Event | None | Dead-letter |
| `event.payload_too_large` | Event | None | Never |
| `consumer.too_slow` | Consumer | None | Backoff |
| `consumer.handler_failed` | Consumer | None | Backoff |
| `consumer.dead_lettered` | Consumer | None | Operator action |
| `consumer.cursor_expired` | Consumer | None | Snapshot backfill |
| `auth.unauthenticated` | Auth | None | After auth |
| `auth.stale_credentials` | Auth | None | After refresh |
| `auth.forbidden` | Auth | Rejected | Never |
| `auth.replayed_request` | Auth | None | Never |
| `audit.unavailable` | Audit | None | Backoff |
| `audit.chain_invalid` | Audit | None | Operator action |
| `audit.export_denied` | Audit | None | After auth |
| `shard.frozen_for_maintenance` | Operations | Rejected | After maintenance |
| `ops.backup_pin_lost` | Operations | None | Restart backup |
| `ops.restore_checksum_mismatch` | Operations | None | Restart restore |
| `ops.migration_incomplete` | Operations | None | Resume migration |
| `ops.downgrade_not_allowed` | Operations | None | Never |
| `ops.client_outbox_stale` | Operations | None | After rebase |
| `ops.quota_exceeded` | Operations | Rejected | After quota |
| `ops.rate_limited` | Operations | None | Backoff |
| `rpc.unknown_method` | RPC | None | Never |
| `rpc.descriptor_mismatch` | RPC | None | Regenerate client |
| `rpc.codec_unsupported` | RPC | None | Reconnect |
| `internal.invariant_violation` | Internal | None | Never |
| `internal.resource_exhausted` | Internal | Rejected | Backoff |

### 8.4 Reserved keyspace

| System ID | Label | Owner | Scope visible | Commit domain |
| --- | --- | --- | --- | --- |
| `0x01` | `receipt` | Authority | No | Data shard or catalog shard |
| `0x02` | `event` | Authority | No | Data shard or catalog shard |
| `0x03` | `audit` | Authority | No | Data shard or catalog shard |
| `0x04` | `projection` | Authority | No | Data shard |
| `0x05` | `index` | Authority | No | Data shard |
| `0x06` | `lease` | Authority | No | Data shard or catalog shard |
| `0x07` | `statistics` | Authority | No | Data shard |

### 8.5 Cursor and identity types

| Type | Fields | Expiry | Error |
| --- | --- | --- | --- |
| `KeyCursor` | Table, snapshot version, last key, direction | Snapshot retention | `sync.cursor_expired` |
| `SyncCursor` | Scope generation, version vector, retention floor, auth tag | Diff retention | `sync.cursor_expired` |
| `HydrationCursor` | Snapshot identity, page sequence, verified digest | Snapshot retention | `sync.cursor_expired` |
| `ReceiptCursor` | Last receipt identity and sequence | Receipt retention | `receipt.expired` |
| `EventCursor` | Per-key sequence map or compact watermark | Event retention | `event.retention_gap` |
| `SearchCursor` | Pin, query fingerprint, sort tuple | Pin lifetime | `search.cursor_invalid` |
| `ProjectionCursor` | Source watermarks and epoch | Projection retention | `projection.epoch_invalid` |
| `CostCursor` | Per-shard frontier and version set | Query budget | `query.budget_exceeded` |

| Identity | Scope | Lifetime |
| --- | --- | --- |
| `principal_id` | Authenticated client | Credential lifetime |
| `replica_id` | One origin and storage | Replica lifetime |
| `session_id` | One client session | Session lifetime |
| `request_id` | One submitted request | Receipt retention |
| `receipt_id` | One terminal verdict | Receipt retention |
| `event_id` | One business event | Event retention |
| `audit_sequence` | One audit record per shard | Audit retention |
| `opaque_cursor_id` | One cursor token | Cursor retention |

### 8.6 Budget and limit registry

| Budget | Scope | Error |
| --- | --- | --- |
| `policy_step_budget` | One policy evaluation | `policy.budget_exhausted` |
| `policy_depth_budget` | One policy evaluation | `policy.budget_exhausted` |
| `policy_memory_arena` | One policy evaluation | `policy.budget_exhausted` |
| `query_cost_budget` | One query | `query.budget_exceeded` |
| `query_fanout_budget` | One query | `search.fanout_exceeded` |
| `scope_member_budget` | One scope | `scope.over_limit` |
| `scope_byte_budget` | One scope | `scope.over_limit` |
| `scope_shard_budget` | One scope | `scope.over_limit` |
| `scope_device_budget` | One replica | `scope.device_budget_exceeded` |
| `scope_hydration_budget` | One hydration | `scope.hydration_budget_exceeded` |
| `cursor_depth_budget` | One cursor | `query.budget_exceeded` |
| `subscription_budget` | One session | `sync.scope_rejected` |
| `diff_rate_budget` | One replica | `sync.rate_limited` |
| `event_payload_budget` | One event | `event.payload_too_large` |
| `event_backlog_budget` | One shard | `consumer.too_slow` |
| `batch_size_budget` | One commit | `write.batch_too_large` |
| `key_size_budget` | One key | `key.too_large` |
| `value_size_budget` | One value | `write.value_too_large` |
| `page_size_budget` | One page | `format.page_too_large` |
| `outbox_size_budget` | One replica | `outbox.full` |
| `projection_fan_in_budget` | One rule | `projection.fan_in_exceeded` |
| `fact_read_set_budget` | One rule | `projection.read_set_exceeded` |
| `index_overhead_budget` | One table | `index.overhead_exceeded` |
| `token_count_budget` | One field | `search.token_budget_exceeded` |
| `token_length_budget` | One token | `search.token_budget_exceeded` |
| `shard_quota` | One shard | `ops.quota_exceeded` |
| `client_rate_limit` | One principal | `ops.rate_limited` |

### 8.7 Retention registry

| Retention | Minimum rule | Error on expiry |
| --- | --- | --- |
| `receipt_retention` | `max(max_outbox_age, max_ack_delay, restore_window, audit_window)` | `receipt.expired` |
| `event_retention` | `max(consumer_redelivery_horizon, restore_window)` | `event.retention_gap` |
| `audit_retention` | Longest justified write effect plus one incident window | `audit.chain_invalid` |
| `policy_version_retention` | At least `audit_retention` | `scope.policy_version_unknown` |
| `manifest_revision_retention` | At least `audit_retention` | `manifest.artifact_drift` |
| `read_pin_retention` | Bounded by authority compaction | `read.pin_expired` |
| `sync_cursor_retention` | Bounded by diff retention | `sync.cursor_expired` |
| `search_pin_retention` | Bounded by segment retention | `search.pin_expired` |
| `projection_retention` | At least the longest rule evaluation window | `projection.epoch_invalid` |
| `tombstone_retention` | At least the longest legal resume window | `sync.cursor_expired` |
| `backup_pin_retention` | Until the backup completes or fails | `ops.backup_pin_lost` |
| `consumer_completion_retention` | At least `max(event_retention, restore_window)` | `consumer.cursor_expired` |

### 8.8 Digest and checksum registry

| Use | Algorithm | Coverage |
| --- | --- | --- |
| Page integrity | CRC32C | Page payload |
| Snapshot directory | BLAKE3 | Directory bytes |
| Snapshot content | BLAKE3 | Directory and page digests |
| Manifest digest | BLAKE3 | Canonical JSON |
| Policy version | BLAKE3 | Canonical AST |
| Descriptor digest | BLAKE3 | Canonical descriptor |
| Map digest | BLAKE3 | Canonical map |
| Audit record hash | BLAKE3 | Previous hash and record body |
| Audit checkpoint | BLAKE3 plus signature | Chain tip |
| Event id | BLAKE3 | Shard, key, commit version, rule id, emission index |
| Receipt id | BLAKE3 | Client id, idempotency key, request digest |
| Content seal | BLAKE3 plus Ed25519 signature | Content digest and lease term |

`BLAKE3` is the 1.0 default. A reader MUST support BLAKE3. A future major MAY add SHA-256 with a new format major. CRC32C is used only for page corruption detection. It is not a security hash.

## 9. Subsystem specifications

This section gives the requirements for each subsystem group. The decisions in section 7 are normative. A subsystem requirement adds detail. It does not override a decision.

### 9.1 Storage, keys, and shard map

**Covered essays.** 01, 05, 22.

**Requirements.**

- The store MUST present each shard as one ordered map from immutable byte keys to versioned byte values.
- The store MUST support point reads and point writes.
- The store MUST support range scans with inclusive lower and upper bounds.
- The store MUST support ascending and descending byte order.
- The store MUST support bidirectional cursors at a pinned snapshot version.
- The store MUST support atomic batches.
- A batch MUST commit all operations or none.
- A batch MUST be single-shard.
- A cross-shard batch MUST return `shard.cross_mutation_rejected` and change no state.
- The store MUST support versioned reads at a caller-supplied commit version.
- The store MUST apply operations inside a batch in deterministic order.
- The store MUST preserve committed state across restart.
- The store MUST recover to the last committed batch.
- The store MUST return named errors for a missing key, invalid range, unknown cursor version, and cross-shard batch.
- The store MUST enforce key, value, batch, and page limits.
- The comparator MUST be lexicographic over unsigned bytes.
- The comparator MUST NOT depend on locale, platform, pointer width, or engine.
- The B-tree engine and the LSM engine MUST return the same ordered results for the same operation log.
- The key encoding MUST use the v1 form in section 5.1.
- The key encoding MUST include the `0xFF` system marker.
- The key encoding MUST reject a user key with `0xFF` after the table prefix.
- The key encoding MUST use half-open table bounds `[P, successor(P))`.
- The table prefix `P` MUST be `0x01 || table-name || 0x00`.
- The table name MUST match the grammar in section 4.2.
- The manifest MUST declare the key schema.
- The shard map MUST assign every user table to one data shard.
- The shard map MUST assign `_catalog` to the catalog shard.
- The shard map MUST reject overlap, gap, duplicate identifier, empty range, split table, and unknown version.
- The shard map MUST produce the same route on native and wasm32.
- The shard map MUST NOT read stored data.
- The shard map MUST carry a version and a digest.

**Invariants.**

- Every key belongs to exactly one shard.
- Every table belongs to exactly one shard.
- A user key never collides with a system key.
- A range scan returns every live key inside the bounds once.
- A cursor at version V returns the same remaining sequence on repeat.
- A read at version V observes exactly the writes committed at or before V.
- A batch becomes visible at one commit version or not at all.

**Errors.** `key.unknown_version`, `key.malformed`, `key.non_canonical`, `key.outside_table`, `key.invalid_utf8`, `key.too_large`, `key.not_found`, `key.reserved_system_marker`, `key.unknown_system_id`, `write.batch_too_large`, `write.value_too_large`, `shard.cross_mutation_rejected`, `map.unknown_version`, `map.overlap`, `map.gap`, `map.duplicate_id`, `map.empty_range`, `map.table_unmapped`, `map.table_split_unsupported`.

**Acceptance.**

- A point read at version V returns the committed value or an absence result. A generated RPC get MAY map absence to `key.not_found` when the operation declares a required row.
- A range scan returns every live key inside the bounds once.
- A forward cursor and a reverse cursor over one pinned version return the same key set.
- A cursor resumes after page N with no duplicate and no skip while writes continue.
- A batch commits all operations or none.
- A cross-shard batch returns `shard.cross_mutation_rejected` and changes no state.
- A restart returns every committed batch and no uncommitted batch.
- The B-tree engine and the LSM engine return the same result for the same operation log.
- A user key with `0xFF` fails with `key.reserved_system_marker`.
- A system key with an unknown identifier fails with `key.unknown_system_id`.
- A split table fails with `map.table_split_unsupported`.
- The map routes every valid key to one shard on native and wasm32.

### 9.2 Manifest, policy, and facts

**Covered essays.** 07, 08, 17.

**Requirements.**

- The manifest MUST be the only source of user table, field, index, rule metadata, fact, operation, event, receipt, audit, projection, codec, and control declarations.
- The manifest MUST compile to a neutral descriptor.
- The manifest compiler MUST be pure.
- The manifest compiler MUST NOT read the clock, environment, network, or an undeclared file.
- The same manifest bytes MUST produce byte-identical artifacts on every run, host, and target.
- The manifest MUST carry a schema version, a revision, and a digest.
- The manifest digest MUST NOT include the active policy version.
- The policy catalog MUST be a separate content-addressed store.
- A policy version MUST be immutable.
- A policy version MUST stay resolvable for the audit retention floor.
- The policy AST MUST be canonical.
- The policy AST MUST be total and bounded.
- The policy AST MUST use one production evaluator.
- The test oracle MUST stay outside the production build.
- Policy evaluation MUST be per row.
- A batch verdict MUST be the conjunction of row verdicts.
- A write request MUST produce one receipt and one audit record with a bounded decision list.
- The policy context MUST bind `before` and `after`.
- The evaluator MUST NOT read a clock, draw randomness, perform I/O, inspect the host, or keep state.
- The evaluator MUST use a step budget, a depth budget, and a fixed memory arena.
- Budget exhaustion MUST be `policy.budget_exhausted` and `Rejected`.
- A policy that names an undeclared field MUST fail to compile with `manifest.rule_unknown_path`.
- A rule above the cost bound MUST fail to compile with `manifest.rule_unbounded`.
- A fact declaration MUST name the fact identity, canonical type, owner shard, fan-in cap, and staleness bound.
- A fact MUST change only through an authority write.
- A client MUST NOT write a projection.
- A projection MUST reject a client write with `projection.client_write_rejected`.

**Invariants.**

- One manifest digest describes one schema and layout.
- Two clients with the same manifest digest agree on a query's admissibility.
- A rule body change does not change the manifest digest.
- A policy version change does not change old verdicts.
- A fact change has a new fact version.
- A verdict names the policy version, projection epoch, and fact versions.
- A write validation and a read scope evaluation use the same evaluator.

**Errors.** `manifest.unsupported_schema`, `manifest.unknown_field`, `manifest.duplicate_name`, `manifest.reserved_table_name`, `manifest.key_schema_missing`, `manifest.float_not_allowed`, `manifest.index_unknown_field`, `manifest.index_scope_unreachable`, `manifest.rule_unknown_path`, `manifest.rule_unbounded`, `manifest.operation_cross_shard`, `manifest.operation_unknown_field`, `manifest.artifact_drift`, `manifest.policy_catalog_missing`, `policy.parse_error`, `policy.unknown_field`, `policy.budget_exhausted`, `policy.not_total`, `policy.ast_unsupported`, `policy.regex_not_supported`, `projection.client_write_rejected`.

**Acceptance.**

- A manifest with three tables, four indexes, five rules, and three operations compiles to all artifact families.
- The same manifest compiles byte-identically on native and wasm32.
- A stale artifact fails with `manifest.artifact_drift`.
- A rule body change creates a new policy version and leaves the manifest digest unchanged.
- A policy with an unknown path fails with `manifest.rule_unknown_path`.
- A policy over budget fails with `policy.budget_exhausted`.
- A write and a read scope agree on the same row and policy version.
- A batch with one rejected row changes no row.
- The audit record lists every row decision.
- The test oracle agrees with the production evaluator on every vector.
- A client write to a projection fails with `projection.client_write_rejected`.

### 9.3 Write path, receipts, scheduler, and enforcement

**Covered essays.** 03, 09, 10, 11, 12.

**Requirements.**

- A write request MUST carry an idempotency key.
- A write request MUST carry a base version for each mutated key.
- A request without a base version MUST fail with `write.missing_base_version` and change no data.
- The authority MUST assign commit versions.
- The authority MUST compare the base version for each key at commit.
- A base mismatch MUST produce `Conflict` with `write.version_conflict`.
- A base ahead of the current version MUST produce `Rejected` with `write.stale_base_version`.
- A conflict MUST be terminal for the intent.
- A rebase MUST use a new idempotency key.
- A retry with the same key and same body MUST return the same receipt.
- A retry with the same key and a different body MUST fail with `write.idempotency_key_reuse` and create no second receipt.
- The authority MUST check authentication before authorization.
- The authority MUST check cross-shard mutation before the version check.
- The authority MUST check the version before policy.
- A stale base with a policy-violating body MUST return `Conflict`, not `Rejected`.
- A cross-shard mutation MUST return `shard.cross_mutation_rejected` before any state change.
- Pre-write policy MUST be reject-only. It MUST NOT rewrite the batch.
- Post-write event derivation MUST be pure.
- Post-write event derivation MUST run inside the commit step.
- A post-write failure MUST abort the batch.
- A `Rejected` verdict MUST write a receipt and an audit record.
- A `Rejected` verdict MUST NOT write data, index, or business events.
- The commit MUST write data, index, receipt, audit, and business events in one shard transaction.
- The scheduler MUST use `queued`, `in_flight`, and `terminal` states.
- At most one request per key MAY be `in_flight`.
- Multiple requests per key MAY be `queued`.
- Per-key FIFO MUST order all non-terminal requests.
- The scheduler MUST NOT evaluate policy.
- The scheduler MUST NOT write terminal receipt states.
- The scheduler MUST use explicit intra-shard dependency edges.
- The scheduler MUST reject a cycle, a forward reference, a cross-shard edge, and fan-in overflow.
- The scheduler MUST use a step budget.
- The scheduler MUST be a pure function of state and budget.
- The scheduler MUST NOT read a clock, draw randomness, or perform I/O.
- Enforcement MUST pin the policy version at commit.
- Enforcement MUST record the pinned version in the audit record.
- Enforcement MUST measure fact staleness as a version distance.
- Enforcement MUST use the D5 strict and default behaviors.
- The audit record MUST be one per write request.
- The audit record MUST carry a bounded decision list for the rows.
- The audit chain MUST be hash-linked per shard.
- A failed audit append MUST reject the write with `audit.unavailable`.
- A failed audit append MUST NOT write a receipt and MUST NOT change state.
- The receipt MUST name the audit record.

**Invariants.**

- A terminal receipt is immutable.
- One idempotency key produces at most one terminal receipt.
- A committed write is durable.
- A rejected write changes no data.
- A conflict leaves all keys unchanged.
- No write path performs network I/O.
- No hook performs I/O, reads a clock, or keeps state.
- A post-write event and its data commit together or not at all.

**Errors.** `write.missing_base_version`, `write.stale_base_version`, `write.version_conflict`, `write.idempotency_key_reuse`, `write.queue_timeout`, `write.operation_unknown`, `write.batch_too_large`, `write.value_too_large`, `shard.cross_mutation_rejected`, `policy.budget_exhausted`, `authorization.stale_facts`, `projection.lagging`, `receipt.expired`, `receipt.not_found`, `receipt.state_invalid`, `outbox.full`, `outbox.dependency_cycle`, `outbox.cross_shard_edge`, `audit.unavailable`.

**Acceptance.**

- A write with no base version fails with `write.missing_base_version` and changes no data.
- Two clients rename one record from one base. One gets `Committed`, one gets `Conflict`.
- A base ahead of the current version fails with `write.stale_base_version`.
- A duplicate submit returns the same receipt three times.
- A reused key with a different body fails with `write.idempotency_key_reuse`.
- A rejected write changes no key, index entry, or business event.
- A rejected write has one receipt and one audit record.
- A crash at any commit step leaves data, index, receipt, audit, and event together or absent.
- A blocked key does not delay a ready key in the same shard.
- A blocked shard does not reduce another shard's selections.
- A scheduler replay produces the same batches.
- A policy budget error returns `Rejected` with `policy.budget_exhausted`.
- A stale-base policy violation returns `Conflict`.
- A cross-shard write fails before scheduling.

### 9.4 Policy projection and scope

**Covered essays.** 13, 17.

**Requirements.**

- A fact MUST have one owner shard.
- A fact MUST change only through an authority write.
- A projection MUST be a versioned copy of facts inside the consuming shard.
- A projection MUST carry an epoch and a per-source watermark.
- A projection MUST live in the `0x04` system sub-namespace.
- A projection MUST be hidden from user scopes.
- A projection MUST reject client writes.
- A projection feed MUST support snapshot, ordered diff, cursor, resume, and fresh snapshot fallback.
- A projection apply MUST be idempotent.
- A projection apply MUST reject a version regression.
- A projection MUST fail closed on a checksum error.
- A projection invalidation MUST follow fact-set, schema, ownership, or revocation changes.
- A projection invalidation MUST NOT follow every policy predicate change.
- A canary cohort MUST use a projection epoch that includes its policy version.
- A lagging projection MUST return `projection.lagging` and keep the receipt `Queued`.
- A strict write over the bound MUST return `authorization.stale_facts` and `Rejected`.
- A scope MUST be declarable as data.
- A scope MUST have a deterministic identity.
- A scope identity MUST be a hash of the canonical query, rule-set selector, projection, ordering, and limits.
- A scope version MUST be a version vector.
- A scope read MUST use one version vector.
- A scope MUST report removals with the key, version, and reason.
- A scope MUST report churn per version.
- A scope MUST declare `max_members`, `max_bytes`, and `max_shards`.
- An over-limit scope MUST fail at subscribe.
- A scope MUST compile a cross-prefix query into an explicit shard plan.
- A scope MUST keep a cursor per shard for sync.
- A scope read with an unreachable shard MUST return a coverage result, not an exact result.
- A scope MUST record the policy version and projection epoch behind its materialization.
- A replica MUST answer its scope offline at its local pin.
- A replica MUST expose its version vector and lag.
- Only an authority diff MAY create a scope member.

**Invariants.**

- Exact membership equals the records in the declared prefixes that satisfy the predicate at the pinned version vector, within the limits.
- A scope identity is deterministic across machines and time.
- A scope version never decreases.
- Membership may shrink. Every shrink has an explicit remove.
- A partial scope is explicit and never claims exactness.
- A record that stops matching leaves the scope.
- A record that re-enters receives a new add.
- A policy change does not reinterpret an already emitted diff.

**Errors.** `projection.lagging`, `projection.epoch_invalid`, `projection.watermark_regression`, `projection.checksum_failed`, `projection.client_write_rejected`, `projection.fan_in_exceeded`, `projection.read_set_exceeded`, `authorization.stale_facts`, `scope.over_limit`, `scope.device_budget_exceeded`, `scope.hydration_budget_exceeded`, `scope.identity_mismatch`, `scope.policy_version_unknown`, `scope.shard_unreachable`.

**Acceptance.**

- Two clients declare the same query on different machines and get the same scope identity.
- A record stops matching and the replica receives one remove.
- An offline query returns absent at a known version.
- A cross-prefix scope returns one version vector.
- An unreachable shard returns a coverage result with the reached vector.
- An over-limit scope fails at subscribe and transfers no data.
- A policy change opens a new diff interval and old diffs still parse.
- A 24-hour offline replica resumes without a full resync when the cursor is valid.
- A search returns no record outside the scope.
- A scope read and a search at one pin agree.
- A projection rebuild reaches the same watermark as an uninterrupted feed.
- A canary cohort and a control cohort serve different policy versions.
- A fact-set change invalidates the affected projection.

### 9.5 Snapshot, hydration, and replica

**Covered essays.** 06, 15, 16.

**Requirements.**

- A ShardSnapshot MUST cover one shard at one commit version.
- A ShardSnapshot MUST include the user table and every system sub-namespace.
- A ScopeSnapshot MUST cover one scope at one version vector.
- A ScopeSnapshot MUST include only scope user rows and needed projection facts.
- A ScopeSnapshot MUST NOT include receipt, event, audit, or lease data.
- A ScopeSnapshot build MUST be deterministic.
- A page MUST carry `page_id`, `page_sequence`, `shard_id`, `commit_version`, key range, record count, codec, uncompressed length, and CRC32C.
- The page directory MUST list every page with offset, length, first key, last key, `page_id`, `page_sequence`, and integrity value.
- The directory MUST carry its own integrity value.
- The snapshot content digest MUST cover the directory and page digests.
- The snapshot content seal MUST cover the content digest and lease term.
- The lease term MUST NOT enter the content digest.
- A sealed snapshot MUST remain valid across a lease change.
- A reader MUST verify the content digest before the seal.
- A reader MUST reject an unknown major version with `format.unsupported_version`.
- A reader MUST reject an unknown required feature with `format.missing_required_feature`.
- A reader MUST reject a corrupt page with `format.corrupt_page`.
- A reader MUST reject a page outside the shard bounds with `format.page_out_of_bounds`.
- A reader MUST reject a truncated snapshot with `format.truncated_snapshot`.
- Hydration MUST stream pages, verify checksums, and hand off to the diff tail at one version.
- Hydration MUST build a new generation beside the served generation.
- Hydration MUST publish with one pointer swap.
- Hydration MUST use a durable journal for phase, snapshot identity, verified pages, applied watermark, and swap commit.
- Hydration MUST resume at the last durable boundary.
- Hydration MUST NOT publish a partial page.
- Hydration MUST NOT serve a page that fails verification.
- Hydration MUST expose phase, verified bytes, published version, lag, and target version.
- A cold replica MUST return `read.not_hydrated` with progress, not an empty result.
- A hydrated replica MUST serve the old generation during a new hydration.
- A reader MUST see one version step at handoff.
- A replica MUST store ScopeSnapshot pages without re-encoding.
- A replica MUST keep one monotonic applied version vector.
- A replica MUST use one cursor advancer per replica identity.
- A replica MUST use a local tab lease for multi-tab coordination.
- A replica MUST NOT hold an authority write lease.
- A replica MUST expose applied version, authority watermark, lag in versions and time, and hydration state.
- A replica MUST verify checksums on ingest.
- A replica MUST refetch a corrupt page.
- A replica MUST NOT serve a page that fails verification.
- A replica MUST keep resident bytes under its ceiling.
- A replica MUST include hydration peak in its budget.
- A replica MUST evict only out-of-scope or superseded pages.
- A replica MUST NOT apply a local mutation to the mirror.
- A replica MUST keep the outbox separate from the mirror.

**Invariants.**

- Every mirrored page equals the authority page for the same page identity and version.
- The applied version vector never decreases.
- One cursor advancer per replica identity.
- A read pins one version vector.
- A get and a search at one pin agree.
- A crash leaves exactly one published generation.
- A duplicate frame leaves state unchanged.
- A missing sequence number forces a re-request.
- A cold read never looks like an empty scope.

**Errors.** `format.unsupported_version`, `format.missing_required_feature`, `format.corrupt_page`, `format.page_out_of_bounds`, `format.truncated_snapshot`, `format.missing_provenance`, `format.page_too_large`, `read.not_hydrated`, `read.not_resident`, `read.pin_expired`, `read.behind_commit`, `scope.hydration_budget_exceeded`, `sync.cursor_expired`.

**Acceptance.**

- A v1 reader reads a v1 snapshot offline.
- A v1 reader rejects a v2 major snapshot with `format.unsupported_version`.
- A one-byte page change returns `format.corrupt_page` with page id and offset.
- A snapshot cut at any offset returns `format.truncated_snapshot`.
- A ScopeSnapshot contains zero out-of-scope user rows.
- A crash at any hydration phase converges to the same state.
- A reader loop sees a non-decreasing version and no hole.
- A crash during the swap leaves exactly one generation.
- A corrupt page is refetched and never served.
- A policy or manifest change during hydration restarts hydration and mixes no identities.
- A 1 GiB snapshot hydrates under the page budget.
- A replica offline within retention transfers only missing pages and the tail.
- A replica beyond retention receives a named resnapshot outcome and keeps the old generation readable.
- A multi-tab test produces one cursor advance and no duplicate tail entries.
- A sealed snapshot remains readable after a lease change.

### 9.6 Sync wire and generated RPC

**Covered essays.** 14, 25.

**Requirements.**

- The sync core MUST be sans-IO.
- The sync core MUST exchange values, not sockets.
- The sync core MUST produce the same output for the same input sequence.
- One `SessionHello` MUST negotiate the RPC descriptor, sync protocol version, manifest digest, map version, capabilities, and limits.
- `SessionHello` MUST NOT negotiate a policy version.
- A scope subscription MUST carry its policy version and scope pin.
- A data call MUST NOT precede `SessionHello`.
- One session MUST NOT renegotiate.
- The sync protocol MUST define subscribe, diff, cursor, resume, snapshot fallback, and flow control.
- The diff stream MUST be ordered and gap-free.
- Each diff MUST state shard, key, operation, value or tombstone, and version.
- Each diff MUST state when a key leaves the scope.
- The cursor MUST be opaque, bounded, tamper-evident, and valid after restart.
- The cursor MUST carry a scope generation, version vector, and retention floor.
- The replica MUST detect a gap before it applies a later version.
- Two applies of one diff MUST equal one apply.
- The authority MUST NOT send a byte outside the scope.
- Snapshot fallback MUST send a page manifest, checksummed pages, snapshot version, and tail cursor.
- The snapshot-to-tail handoff MUST be atomic for the replica.
- Flow control MUST be credit based in bytes and messages.
- The authority MUST NOT send beyond credit.
- A slow replica MUST NOT cause unbounded buffering.
- Keepalive MUST detect a dead peer.
- Every failure MUST map to a named error.
- Connect MUST be the RPC envelope.
- A Connect server stream MAY carry sync frames as opaque bytes.
- RPC MUST NOT re-encode a sync diff.
- The descriptor MUST be deterministic from the manifest.
- The descriptor MUST list every method, message, field number, error code, and streaming shape.
- A data call MUST NOT exist without negotiation.
- A write method MUST NOT exist without an idempotency key and base version.
- Every receipt and error variant MUST map to one generated code.
- An unknown server code MUST map to a named client error that preserves the raw code.
- A slow consumer MUST return `consumer.too_slow` without stalling a shard.
- Control operations MUST use the `control` namespace.
- Control operations MUST use the same generator, error mapping, and authentication as data operations.

**Invariants.**

- The sync core has no clock, socket, or filesystem.
- Every transport carries the same messages with the same meaning.
- The observed version never decreases.
- A replay returns one diff sequence.
- A gap is detected before a later diff applies.
- Scope confinement holds under fuzzing.
- Snapshot plus tail covers the window with no hole.
- An unknown version fails with a named error.
- A frozen message type has a golden vector.

**Errors.** `sync.unsupported_version`, `sync.unknown_capability`, `sync.scope_rejected`, `sync.cursor_expired`, `sync.cursor_invalid`, `sync.cursor_type_mismatch`, `sync.gap_too_large`, `sync.snapshot_unavailable`, `sync.rate_limited`, `sync.unauthorized`, `sync.shard_unavailable`, `sync.negotiation_required`, `sync.incompatible_manifest`, `consumer.too_slow`, `rpc.unknown_method`, `rpc.descriptor_mismatch`, `rpc.codec_unsupported`.

**Acceptance.**

- A replica disconnects for a simulated day and resumes from a cursor with only missed diffs.
- A browser completes negotiation, subscription, snapshot, tail, and resume over Connect without a proxy.
- The in-process and network transports produce the same message sequence for one transcript.
- A property test applies each diff stream twice and compares state.
- An injected gap forces snapshot fallback with no lost version.
- An expired cursor returns `sync.cursor_expired` and offers a snapshot.
- An unsupported version returns `sync.unsupported_version` with the supported range.
- A slow consumer causes bounded memory and a retryable close with the cursor intact.
- A scope fuzz run produces zero out-of-scope bytes.
- Every message type has a golden vector.
- A write method without an idempotency key or base version fails to build.
- An unnegotiated data call returns `sync.negotiation_required`.
- An unknown method returns `rpc.unknown_method`.
- A control operation is generated from the descriptor.

### 9.7 Search and cost

**Covered essays.** 18, 19, 20.

**Requirements.**

- The index MUST be per shard.
- Index maintenance MUST be inside the write transaction.
- The data page, index page, and commit record MUST form one atomic unit.
- A delete MUST tombstone every posting in the same commit.
- An update MUST be a delete plus an insert in one transaction.
- The tokenizer MUST be named by identifier and version.
- A segment MUST record the tokenizer pair.
- A reader MUST reject a mismatched tokenizer with `search.tokenizer_mismatch`.
- The manifest MUST declare indexed fields, positions, searchable flags, filterable flags, overhead budget, and scorer.
- Compaction MUST merge segments and preserve the token stream.
- Compaction MUST NOT change the result set for a retained version.
- A write or compaction that exceeds the index overhead budget MUST fail with `index.overhead_exceeded`.
- The index MUST carry a checksum per segment and a digest per commit record.
- A mismatch MUST return `search.index_mismatch`.
- The replica MUST build its index from scope members.
- The replica index build MUST use the frozen tokenizer and manifest scorer.
- The authority per-shard index MUST NOT be the replica index.
- Search MUST use one pinned version vector.
- Every posting MUST carry visibility metadata.
- A search and a get at one pin MUST agree.
- A search MUST filter candidates through the same policy evaluator used for reads and writes.
- Scope filtering MUST run before top-k selection.
- Pagination MUST use keyset cursors over the full sort tuple.
- A cursor MUST bind the pin, query fingerprint, and last sort tuple.
- A cursor mismatch MUST return `search.cursor_invalid`.
- A cross-prefix search MUST merge ordered shard streams deterministically.
- The search comparator MUST be `(score desc, canonical key asc)`.
- Ranking MUST use document-local features only.
- Ranking MUST use integer arithmetic.
- A table MUST declare one scorer and its integer parameters.
- A search MUST NOT mutate the index, visibility watermark, or outbox.
- An expired pin MUST return `search.pin_expired` with requested and oldest readable versions.
- A fan-out above the bound MUST fail with `search.fanout_exceeded`.
- A cost model MUST produce an explain record.
- The explain record MUST name:
  - manifest version
  - policy version
  - shard map version
  - statistics version
  - policy normalization
  - shard set
  - per-shard plan
  - merge shape
  - total shards
  - total keys
  - total cost
  - active budget
  - verdict
- Explain MUST read metadata only and produce no event.
- Admission MUST compare the planned shard set with the scope and replica ceilings.
- A scope above the ceiling MUST fail at subscribe with `scope.over_limit`.
- A query above its runtime budget MUST fail with `query.budget_exceeded`.
- A coverage result MUST carry a coverage descriptor.
- A query MUST NOT return a silent subset.

**Invariants.**

- At every version V, a search pinned at V observes exactly the documents committed at or before V.
- A search never returns a hit that a get denies at the same pin.
- A search never omits a row that a get returns at the same pin.
- A page never repeats or skips a row under concurrent writes.
- One query, scope, sort, and pin produce one order on authority, native, wasm32, and replica.
- A delete leaves no posting after its commit.
- A tokenizer version never changes behavior.
- A rebuild produces byte-identical segments.
- A search page and a get at one pin agree.
- Scope filtering does not leak a hidden count.

**Errors.** `search.pin_expired`, `search.cursor_invalid`, `search.fanout_exceeded`, `search.index_mismatch`, `search.tokenizer_mismatch`, `search.tokenizer_unknown`, `search.token_budget_exceeded`, `query.budget_exceeded`, `index.overhead_exceeded`, `scope.over_limit`, `scope.shard_unreachable`.

**Acceptance.**

- A writer commits updates and deletes between page one and page two. The reader gets every original hit once.
- A deleted row never appears at or after the delete.
- An updated row never mixes old and new fields.
- Three prefixes produce one merged result with deliberate ties.
- The order is byte-identical on authority, native, wasm32, and in-process.
- A restricted principal gets no hidden row and no hidden count.
- An expired pin returns `search.pin_expired`.
- A changed query, scope, sort, or pin returns `search.cursor_invalid`.
- A replica offline for a day answers its scope at its local pin and exposes lag.
- A crash between row and index writes leaves neither visible.
- A corrupted segment returns `search.index_mismatch`.
- A fan-out above the bound fails with `search.fanout_exceeded`.
- Explain output is byte-identical on native and wasm32.
- A scope above the shard ceiling fails at subscribe and transfers no data.
- A query above its runtime budget returns `query.budget_exceeded` and a coverage descriptor.
- Shuffled shard responses produce identical pages.

### 9.8 Replication, shard map, and bounded staleness

**Covered essays.** 21, 22, 23.

**Requirements.**

- One driver interface MUST expose acquire, renew, release, append, commit index, read at index, member set, and transfer.
- One fence value MUST cross the interface.
- The fence value MUST be monotonic.
- The fence value MUST travel with every log record.
- The storage boundary MUST compare the write fence with the owner fence.
- A higher fence MUST win.
- A lower fence MUST return `replication.stale_epoch`.
- The log MUST live on low-latency durable storage.
- The log MUST NOT live on an object store.
- A record counts as appended only after its bytes are durable.
- The default read mode MUST be named and bounded.
- A leader read MUST confirm its lease or commit index.
- A follower read MUST use an explicit version pin.
- Membership changes MUST be one member at a time.
- A configuration change MUST be a log entry.
- Two membership changes MUST NOT run at once.
- A new member MUST catch up before it counts toward a quorum.
- The leader-kill test MUST run in CI.
- The leader-kill test MUST assert one terminal receipt per write.
- Both drivers MUST pass one conformance suite with a simulated clock and network.
- The lease driver MUST be a degenerate case of the same interface.
- A shard MUST NOT run a lease and a consensus group as independent authorities.
- The shard map MUST be static and versioned.
- The shard map MUST route every key to exactly one shard.
- The shard map MUST reject overlap, gap, duplicate identifier, empty range, split table, and unknown version.
- The shard map MUST compile a cross-prefix scope into an explicit scatter-gather plan.
- A cross-shard mutation MUST fail with `shard.cross_mutation_rejected`.
- A map version and digest MUST travel with the manifest, snapshot, scope, and handshake.
- A map mismatch MUST return `sync.incompatible_manifest` or `map.unknown_version`.
- Bounded-staleness reads MUST use a version pin.
- `AtLeast(V)` MUST be the default read mode.
- `Exactly(V)` MUST serve pagination and audit replay.
- A read MUST declare `max_staleness` before it runs.
- A read MAY narrow the scope default.
- A read MAY widen the bound only by explicit override.
- A read MAY declare an unbounded bound for offline use.
- The response MUST report data age when the bound is unbounded.
- A replica MUST compare its freshness to the bound.
- A read over the bound MUST return `read.stale` and no data.
- A read MUST NOT serve below its session minimum.
- A session token MUST carry a version vector minimum.
- A read-your-writes read MUST raise the session minimum to the receipt commit version.
- A read-your-writes read MUST block or return `read.behind_commit`.
- A cross-shard read MUST pin each shard separately.
- A cross-shard read MUST merge from the version vector.
- A cross-shard read MUST NOT claim one global version.
- A read MUST record requested bound, served staleness, and outcome.
- The SLO report MUST give availability, p99 staleness, over-bound rate, and leader-kill recovery time.

**Invariants.**

- At most one leader holds a valid fence for a shard.
- The fence never returns to an earlier value.
- A committed write survives the loss of any minority.
- A receipt reaches exactly one terminal state.
- A read never returns state older than its bound.
- No driver byte appears on the wire.
- Per-key event order survives a leader change.
- One shard has one ownership mechanism.
- The map is total and disjoint.
- A placement change creates a new map version.
- A cross-shard read never claims a global version.

**Errors.** `replication.stale_epoch`, `replication.lease_expired`, `replication.lease_held`, `replication.only_copy_eviction`, `replication.tier_unavailable`, `replication.warmup_budget_exceeded`, `replication.placement_violation`, `map.unknown_version`, `map.overlap`, `map.gap`, `map.duplicate_id`, `map.empty_range`, `map.table_unmapped`, `map.table_split_unsupported`, `read.stale`, `read.pin_expired`, `read.behind_commit`, `sync.incompatible_manifest`.

**Acceptance.**

- The leader-kill test runs in CI and every write reaches one terminal receipt.
- Repeated kills lose no committed write and duplicate no commit.
- Both drivers pass one conformance suite.
- A stale leader receives `replication.stale_epoch` and writes nothing.
- A read never returns a version older than its bound.
- A membership change completes under load with no split brain.
- Log append latency meets its p99 budget.
- Wire bytes are identical under both drivers.
- A map property test returns one shard per key on native and wasm32.
- A map validator rejects overlap, gap, duplicate id, empty range, split table, and unknown version.
- A scope over N prefixes yields at most N sub-queries.
- A two-shard write fails with `shard.cross_mutation_rejected`.
- A stale map digest is refused at handshake.
- A 24-hour offline session reads and searches its scope.
- A leader kill leaves bounded reads available for the full bound.
- A read-your-writes read blocks or returns `read.behind_commit`.
- A cross-shard read returns a version vector, not a scalar.
- The SLO report names every required metric.

### 9.9 Events and effect consumer

**Covered essays.** 24, 26.

**Requirements.**

- The business event stream MUST be authority-only.
- A business event MUST commit with its write.
- A rejected or conflict write MUST emit no business event.
- The event envelope MUST be self-describing, canonical, and versioned.
- An unknown event version MUST return `event.unknown_version`.
- The event id MUST derive from shard, key, commit version, rule identity, and emission index.
- The event envelope MUST carry:
  - `envelope_version`
  - `event_id`
  - `shard_id`
  - `table`
  - `key`
  - `key_sequence`
  - `commit_version`
  - `committed_at`
  - `rule_id`
  - `policy_version`
  - `emission_index`
  - `payload`
  - `payload_schema`
  - `origin_request_id`
- `committed_at` MUST be an injected commit parameter.
- The envelope MUST NOT carry the raw idempotency key.
- The event MUST carry the pinned policy version.
- Delivery MUST be at least once.
- Order MUST be per key.
- Acknowledgement MUST be per key.
- One unacknowledged event MUST block only its key.
- The consumer library MUST partition work by key.
- Retention MUST have a time bound, a byte bound, or both.
- A cursor behind retention MUST return `event.retention_gap` with the oldest position.
- The consumer MUST expose lag per key.
- The consumer MUST use one handler per event type.
- The handler MUST receive the event, an idempotency key, and a deadline.
- The handler MUST NOT receive an authority write handle.
- The consumer MUST use a completion record and a provider idempotency key.
- The consumer MUST NOT offer exactly-once delivery.
- An ack MUST follow the effect.
- A nack MUST schedule a retry and must not advance the cursor.
- Retry MUST use jittered exponential backoff with an attempt cap and a time cap.
- A terminal failure MUST move the event to a dead-letter queue.
- The dead-letter write and cursor advance MUST commit together.
- A poison event MUST be terminal on the first attempt when the cause is deterministic.
- An operator MUST be able to redrive one dead-letter record with the original key and an audit record.
- The consumer MUST expose lag, attempts, and dead-letter depth.
- The consumer MUST NOT write the authority.
- A data change found during an effect MUST become a new request with a new key and base version.
- The core MUST be sans-IO and deterministic.
- The core MUST take a clock and a transport.

**Invariants.**

- An event is readable if and only if its write committed.
- The event id is deterministic.
- Per-key event order equals commit order for that key.
- Rule evaluation cannot observe delivery state, ack state, or the network.
- No side effect occurs before the append.
- The envelope carries the pinned policy version and rule identity.
- Retention never discards an event without a signal.
- Native and wasm32 produce byte-identical envelopes and ids.
- A consumer crash may duplicate work. It must not skip work.
- A handler cannot write the authority.

**Errors.** `event.unknown_version`, `event.retention_gap`, `event.poison`, `event.payload_too_large`, `consumer.too_slow`, `consumer.handler_failed`, `consumer.dead_lettered`, `consumer.cursor_expired`, `audit.unavailable`.

**Acceptance.**

- A committed write that matches a rule emits exactly one event with a stable id.
- A rejected or conflict write emits no event.
- A kill after commit and before the relay restart leaves the event readable.
- A consumer disconnect and resume skips no event.
- Duplicates stay within the at-least-once bound.
- A consumer that stops acknowledging receives `event.retention_gap`, not silence.
- A rule with a network import fails to load.
- A replay of one commit produces the same event id and bytes on native and wasm32.
- A poison event on one key does not block another key.
- A dead-letter record commits with its cursor advance.
- A handler without an authority handle fails at the boundary.
- A malformed payload returns `event.poison` and never reaches the handler.
- A restart resumes from the last ack.
- Lag, attempts, and dead-letter depth are visible per key.

### 9.10 Rule lifecycle, operations, and security

**Covered essays.** 27, 28, 29.

**Requirements.**

- A policy version MUST be immutable and content-addressed.
- The policy catalog MUST be stored in the catalog table.
- A rule change MUST be a write with an idempotency key and a base version.
- A rule change MUST return one receipt in `Queued`, `Committed`, `Rejected`, or `Conflict`.
- A rule change MUST produce one audit record.
- A rule change MUST name the policy id, both versions, cohort, role, and dry-run digest.
- A stale base MUST return `Conflict`.
- A rule change that passes admission and then fails compile, budget check, or dry run MUST return `Rejected` with a named reason.
- A rule change that fails before admission MUST return a protocol error and no receipt.
- A dry run MUST use a pinned sample.
- A dry run MUST produce a structural diff and a semantic diff.
- A dry-run digest MUST join the approval.
- A rollout MUST activate a version for one cohort.
- A scope MUST pin its policy version during sync.
- A write MUST resolve its policy version at admission.
- The authority MUST serve at most two versions of one policy id.
- A rollout MUST have a deadline.
- A canary over budget MUST abort.
- A rollback MUST activate a prior version.
- A rollback MUST NOT edit content or require a rebuild.
- A rollback receipt MUST record a new write.
- The audit MUST answer which writes a version judged, per shard and tenant.
- A read-widen change MUST NOT auto-promote.
- A read-widen change MUST require a security approver.
- A write-deny change MUST require a table owner and a platform operator.
- A multi-table change MUST decompose by table and cohort.
- A multi-table change MUST produce one receipt per table.
- The catalog MUST stay writable while data-plane policy denies every row.
- Backup MUST define one consistency point per shard and a version vector for the set.
- Backup MUST record manifest digest, format version, policy version, map version, object list, and checksum per object.
- Backup MUST copy only immutable objects reachable at the captured vector.
- Backup MUST hold a retention pin until it completes or fails.
- Restore MUST write a new epoch.
- Restore MUST verify every object before it serves a read.
- Restore MUST preserve terminal receipts and event cursors, or mark them expired with a named error.
- Restore MUST NOT mutate the source epoch.
- Restore MUST NOT re-execute an effect.
- A migration window MUST publish its interval.
- A migration MUST read format N and N-1 and write only N.
- A migration MUST quiesce writes, drain the scheduler, rewrite under a new epoch, verify, and flip the pointer.
- A write during maintenance MUST return `shard.frozen_for_maintenance`.
- Reads MAY continue from replicas during maintenance.
- Compaction MUST have a byte-per-second budget and a files-per-hour budget.
- Compaction pause MUST have an expiry.
- Compaction MUST report backlog, oldest input age, and bytes rewritten.
- Metrics MUST cover:
  - manifest version
  - conflict rate
  - checksum failures
  - restore failures
  - lag
  - cursor age
  - resume rate
  - full-resync rate
  - policy time
  - budget rejects
  - receipt outcomes
  - oldest Queued age
  - error rates
- Tracing MUST cover one write, one query, and one sync.
- Quotas MUST be per shard.
- Rate limits MUST apply per shard, client, and operation class.
- Admission MUST reject before work starts.
- A denied write MUST return `Rejected` with a named reason.
- A denied read MUST return a named error.
- Health MUST be a pure function over sampled counters.
- A sick shard MUST name its top cause.
- Authentication MUST verify a credential and derive a subject before policy.
- A signed request MUST bind the body, idempotency key, base version, nonce, and expiry.
- Revocation MUST have a staleness bound.
- The audit chain MUST be hash-linked per shard with signed checkpoints.
- An external witness MUST be asynchronous.
- The manifest and policy MUST name key identifiers, not key material.
- Policy MUST have no secret access.
- Audit, errors, and traces MUST carry fixed tokens.
- Snapshots SHOULD encrypt at rest.
- Secrets MUST NOT enter policy, audit, error text, traces, or events.
- A control action MUST be audited.
- A control action MUST NOT impersonate a data write.

**Invariants.**

- Active rule content is immutable.
- One base version produces at most one committed activation per cohort.
- A verdict is a pure function of the policy version, row, subject, and projected facts.
- A version does not serve a cohort before dry run and approval are durable.
- A receipt names the version that judged the write.
- A shard holds at most two versions of one policy id.
- A multi-table change decomposes by table and cohort.
- A rollout terminates at its deadline.
- A rollback does not depend on the bad version.
- A backup has one consistency point per shard and a version vector.
- A restore never loses a committed write.
- A migration never loses a committed write.
- A control action is audited and cannot impersonate a data write.
- Authentication precedes authorization.
- A denial does not confirm a hidden record.
- A secret never enters a policy, audit, error, trace, or event.

**Errors.** `policy.budget_exhausted`, `policy.unknown_field`, `manifest.rule_unknown_path`, `manifest.rule_unbounded`, `rule.compile_failed`, `rule.dry_run_failed`, `rule.approval_required`, `rule.rollout_limit`, `write.version_conflict`, `write.idempotency_key_reuse`, `shard.frozen_for_maintenance`, `ops.backup_pin_lost`, `ops.restore_checksum_mismatch`, `ops.migration_incomplete`, `ops.downgrade_not_allowed`, `ops.client_outbox_stale`, `ops.quota_exceeded`, `ops.rate_limited`, `auth.unauthenticated`, `auth.stale_credentials`, `auth.forbidden`, `auth.replayed_request`, `audit.unavailable`, `audit.chain_invalid`, `audit.export_denied`.

**Acceptance.**

- Each activation returns one receipt in one of four states.
- A stale base returns `Conflict` and activates nothing.
- A failed compile, budget, or dry run returns `Rejected` with a named reason.
- A pinned sample and one candidate produce a byte-identical diff.
- A canary and a control cohort serve different versions.
- A rollback is one activation write and rebuilds nothing.
- A deny-all validator on one tenant leaves other tenants' writes `Committed`.
- A read-widen change with one approver returns `Rejected` with `rule.approval_required`.
- A third live version of one policy id returns `Rejected` with `rule.rollout_limit`.
- A canary over budget aborts and restores the prior pointer.
- The audit returns write counts per shard and tenant for a retained version.
- The catalog accepts a rollback while data-plane policy denies every row.
- A multi-table change produces one receipt per table.
- A 10 GB backup completes under write load.
- A restore reproduces every key, value, index, receipt, and event cursor at the captured vector.
- A lost pin returns `ops.backup_pin_lost`.
- A checksum mismatch returns `ops.restore_checksum_mismatch`.
- A migration drains every authority-known receipt to a terminal state.
- A write during maintenance returns `shard.frozen_for_maintenance`.
- A migration from N-1 to N and its rollback complete inside one window when rollback is allowed.
- A quota breach rejects at admission with a named error.
- A conflict storm marks a shard sick within five minutes and names the cause.
- Every control action is audited with actor, shard, version, and result.
- Two backup targets and two telemetry exporters pass the same contract tests.
- An unauthenticated request returns `auth.unauthenticated` before policy.
- A replayed request with a used nonce and a different body returns `auth.replayed_request`.
- A retry with the same idempotency key and body returns the original receipt.
- A revoked credential fails after the revocation staleness bound.
- A failed audit append rejects the write with `audit.unavailable`.
- A failed audit append leaves no receipt and changes no state.
- A changed audit byte or missing record fails verification.
- A canary secret does not appear in the manifest, policy, audit, logs, errors, or traces.

### 9.11 Qualification suite and falsifiers

**Covered essays.** 30, 31.

**Requirements.**

- The suite MUST start in wave 0.
- The suite MUST run one scenario matrix on native, browser, and native again.
- A scenario MUST NOT have a target-specific branch.
- A released vector MUST NOT change.
- CI MUST reject an edit to a released vector.
- The suite MUST hold a vector corpus for every core contract and annex.
- The suite MUST inject a link cut, cursor expiry, conditional-write loss, corrupt page, truncated frame, crash at each commit step, and duplicate delivery.
- The suite MUST use a logical clock, fixed seed, canonical ordering, and normalized traces.
- The suite MUST compare targets and versions.
- The suite MUST fuzz every decoder and the policy evaluator.
- The fuzz oracle MUST check totality, determinism, step monotonicity, memory bound, and native/wasm parity.
- The suite MUST produce one report and one narration from the same trace.
- The pull request tier MUST run native vectors and one browser target.
- The nightly tier MUST run the full browser matrix, old artifacts, and fuzzing.
- The release candidate MUST run the full cross product.
- The browser tier MUST gate the release.
- The falsifier suite MUST use one workload spec with a hash.
- The spec MUST declare dataset size, selectivity, rules, facts, clients, shard map, policy version, concurrency schedule, trace, axes, and metrics.
- The suite MUST use synthetic generators with skew.
- The suite MUST replay a scrubbed trace.
- The suite MUST use three drivers: micro, in-process cluster, and replay.
- Only the in-process cluster MAY pass or fail a falsifier.
- The suite MUST measure sync amplification `A`, policy cost `P`, conflict rate `K`, cold start `S`, and read p99 `L`.
- Each metric MUST have one definition, one workload, and one threshold.
- A lost update MUST fail at any rate.
- A cross-shard mutation MUST fail unless it returns `shard.cross_mutation_rejected`.
- A receipt MUST reach one terminal state.
- An idempotent replay MUST NOT duplicate an effect.
- A crossed slope in two of three runs MUST fail the gate.
- A non-zero correctness counter MUST fail the gate.
- A committed baseline MUST be required for the 1.0 spec set.
- The baseline MUST name hardware class, toolchain version, and commit.
- The wasm target MUST report the same operation hash as native.
- The report MUST declare a breach, the metric, and the action.
- A silent miss MUST fail the gate.

**Invariants.**

- One logical input produces one normalized trace on every target.
- A receipt has exactly one terminal state.
- A read scope and a write validation return one verdict for one expression and fact set.
- A resume from a valid cursor loses no committed change.
- A snapshot restore never exposes a partial page or a torn handoff.
- An older artifact opens and reads its recorded values.
- A newer artifact fails with `format.unsupported_version`.
- Policy evaluation terminates within its budget.
- No oracle depends on wall-clock time, memory address, or map iteration order.
- A failing target fails the suite.
- A skip needs a named reason in the report.
- A benchmark that cannot fail is a demo, not evidence.

**Acceptance.**

- A native-to-browser-to-native round trip produces identical export bytes.
- CI rejects an edit to a released vector.
- Every core contract and annex has a versioned vector corpus.
- An older artifact opens and reads its recorded values.
- A newer artifact fails with `format.unsupported_version`.
- A conflicting write ends `Conflict` with the current key version.
- A rule change is checked against the pinned policy version and writes one audit record.
- A disconnect at any injected frame resumes without a full resync.
- A customer runs the suite offline in one command.
- One spec runs all five falsifier families across tiers.
- Each run writes a report with slope, interval, and verdict.
- The gate fails on a crossed threshold in two of three runs.
- Each tier 2 result names the tier 1 group that explains it.
- The in-process and TCP transports agree within 5% on point-read p99.
- A committed baseline exists for the 1.0 spec set.
- The wasm target reports the same operation hash as native.
- Tier 1 finishes within 30 minutes.
- Tier 3 finishes within 4 hours nightly.
- The report declares a breach, the metric, and the action.

### 9.12 Native and wasm parity

**Covered essays.** 02.

**Requirements.**

- All semantics MUST live in one core library.
- The core MUST compile for native and wasm32.
- The core MUST NOT fork behavior on target.
- An adapter MUST supply only storage, time, entropy, scheduling, and transport.
- An adapter MUST NOT decide a verdict or shape a byte.
- The core MUST read time and entropy through interfaces.
- A test adapter MUST supply scripted time and entropy.
- The core MUST use canonical encoding with fixed-width integers and explicit byte order.
- The core MUST NOT use floating point in a frozen format.
- The core MUST NOT require 64-bit atomics that wasm32 cannot provide.
- Native MAY use threads.
- Browser MAY lack threads.
- The same input sequence MUST produce identical observable outputs on both targets.
- A snapshot written on one target MUST hydrate the other.
- A re-encode MUST produce identical bytes.
- A fault MUST produce the same named error and terminal state on both targets.
- An offline replica MUST answer reads and searches from its scope as a connected replica would at the same version.
- The qualification suite MUST run the same vectors on both targets.
- A static check MUST reject a core module that reads the clock, entropy, environment, or locale directly.
- A static check MUST reject a platform conditional on a semantic path.

**Invariants.**

- Semantic identity: one ordered input produces one observable output set.
- Byte identity: one artifact decodes on both targets and re-encodes to the same bytes.
- No ambient input: clock, randomness, locale, environment, hostname, thread count, and file metadata do not affect a decision or a byte.
- Ordering identity: the same logical keyspace iterates in the same order on both targets.
- Failure identity: the same fault produces the same named error and terminal state.
- Offline equality: an offline replica answers as a connected replica at the same version.
- Parallelism is optional: concurrency may change latency, not results.

**Errors.** All named errors in section 8.3, with the same code and shape on both targets.

**Acceptance.**

- One input log produces byte-identical keys, snapshots, pages, wire frames, receipts, and errors on native and wasm32.
- A snapshot from one target hydrates the other.
- A static check fails when a core module reads the clock, entropy, environment, or locale directly.
- A static check fails when a platform conditional appears on a semantic path.
- A native-to-wasm-to-native round trip preserves bytes and results.
- The same input under two executors produces the same output.
- Threads on native and no threads on wasm produce the same result.
- A failing vector on one target fails the suite.

## 10. Work sequence and dependency graph

### 10.1 Dependency table

The wave number is the earliest wave that may declare or implement the item. A contract may be declared before its implementation. An implementation MUST NOT define a new contract behavior.

| Item | Wave | Prerequisites | Gate |
| --- | --- | --- | --- |
| 01 Ordered KV | 1 | None | Ordered KV vectors |
| 02 Native and wasm | 1 | 01 | Byte identity vectors |
| 03 Hooks | 1 | 01, 07 contract, 08 contract | Hook vectors with null policy and null event sink |
| 04 Tiering and lease | 1 | 01, 05 contract, 06 contract, 09 contract, 10 contract | Lease kill and tier identity |
| 05 Key encoding | 1 | 07 contract, 22 contract | Key vectors and system marker |
| 06 Snapshot and page | 1 | 05, 07 contract, 17 contract | Snapshot and page vectors |
| 07 Manifest | 1 | 05 contract, 22 contract | Manifest vectors |
| 08 Policy and AST | 1 | 07 contract | Policy vectors and AST vectors |
| 09 Versioned writes | 2 | 05, 07, 08, 10 contract, 11 contract | Conflict vectors |
| 10 Receipts and outbox | 2 | 09, 14 contract | Receipt state vectors |
| 11 Scheduler | 2 | 09, 10, 22 contract | Scheduler vectors |
| 12 Enforcement and audit | 2 | 08, 09, 10, 11, 17, 24 contract, 29 auth/audit contract | Revocation and audit vectors |
| 13 Scope | 3 | 07, 08, 10, 14 contract, 22 contract | Scope vectors |
| 14 Sync wire | 3 | 05, 06, 10, 13 contract | Wire vectors |
| 15 Client replica | 3 | 06, 13, 14, 16, 18 contract, 19 contract | Replica vectors |
| 16 Hydration | 3 | 06, 14, 15, 19 contract | Crash matrix |
| 17 Projection | 2 | 07, 08, 09, 10, 24 contract | Projection vectors |
| 18 Inverted index | 3 | 06, 07, 19 contract | Tokenizer and index vectors |
| 19 Snapshot-consistent search | 3 | 13, 14, 18 contract | Search vectors |
| 20 Cost model | 3 | 07, 13, 18, 19 | Explain vectors |
| 21 Replication seam | 4 | 04, 09, 10, 11, 13, 14, 24 contract | Leader-kill test |
| 22 Shard map | 4 | 05, 07, 13 | Map vectors and coverage |
| 23 Bounded staleness | 4 | 10, 14, 15, 19, 21 | SLO report |
| 24 Event stream | 5 | 03, 07, 09, 10, 11, 17 | Event vectors |
| 25 Generated RPC | 5 | 07, 10, 14, 24 | RPC vectors |
| 26 Effect consumer | 5 | 03, 07, 10, 22, 24, 25, 28 contract | Consumer matrix |
| 27 Rule lifecycle | 5 | 07, 08, 09, 10, 12, 13, 17, 24 | Lifecycle vectors |
| 28 Operations | 6 | 06, 07, 09, 10, 17, 21, 22, 23, 24, 27, 29 contract | Backup, restore, migration tests |
| 29 Security | 6 | 07, 08, 09, 12, 13, 14, 15, 17, 19, 20, 25, 27, 28 | Fuzz and tamper tests |
| 30 Qualification | 0 and 6 | All contract nodes | Full matrix and report |
| 31 Falsifiers | 0 and 6 | 05, 06, 07, 09, 10, 11, 13, 14, 15, 16, 17, 18, 19, 20, 22, 23, 28, 30 | Falsifier report |
| 32 1.0 freeze | 7 | 15, 17, 21, 24, 27, 28, 29, 30, 31 | Release checklist |

### 10.2 Dependency rules

- A contract node MUST be declared before any implementation that depends on it.
- An implementation MAY use a fake behind a declared contract.
- A wave gate MUST NOT require a test from a later wave.
- A wave MUST NOT close on a test that needs a later wave.
- A contract candidate MAY break before wave 7.
- A frozen contract MUST NOT break inside 1.x.
- The error registry is a candidate until wave 7.
- The qualification harness starts in wave 0.
- The falsifier harness starts in wave 0 and gates at wave 6.
- The manifest compiler stages its emitters. Layout and indexes start in wave 1. Scope starts in wave 3. RPC starts in wave 5.
- Item 18 starts after the item 19 read-pin contract is declared.
- The Phase D milestone says "reads exactly its query." Search joins in Phase E.

## 11. Conflict register

| ID | Conflict | Evidence | Decision | Status |
| --- | --- | --- | --- | --- |
| C01 | Phase order is cyclic | 07 needs 22, 22 needs 07, 28 needs 29 and 32, 32 needs 28, 27 needs 30, 30 needs 29 | D24 | Resolved |
| C02 | Phase D milestone includes search | Roadmap Phase D vs search in Phase E | D24 | Resolved |
| C03 | Index before its read contract | Phase E lists 18 before 19. 18 says it cannot close before 19 | D3, D24 | Resolved |
| C04 | Tiering before formats | Phase A has three tiers and two drivers. The ordering rule forbids it | D24 | Resolved |
| C05 | Manifest compiles scope and RPC too early | Phase B vs scope in D and RPC in G | D13, D24 | Resolved |
| C06 | Error taxonomy freezes too early | Phase C item 9 vs later errors in 22, 23, 27, 28, and 29 | D16, D24 | Resolved |
| C07 | "Freezes" in Phase B and C vs 1.0 freeze | Phase items say freeze. 31 can change a contract. 32 is the freeze | D15, D24 | Resolved |
| C08 | Custom wire protocol vs Connect | 14 rejects an RPC framework. 25 rejects a custom frame protocol | D4 | Resolved |
| C09 | Five contracts vs many frozen surfaces | 32 says five. 05, 12, 13, 17, 18, 20, 22, 24, and 25 also freeze surfaces | D15 | Resolved |
| C10 | Shard equals table vs system keys | 01, 04, 05, and 06 say shard equals table. 10, 12, 17, 18, and 24 need system keys in the same transaction | D1, D18 | Resolved |
| C11 | Table split vs no split | 05 says no split. 22 allows split points | D1 | Resolved |
| C12 | Manifest prefix example breaks key grammar | 07 uses `acct/`. 05 allows `[a-z][a-z0-9_]*` | D1, D13 | Resolved |
| C13 | Snapshot is per shard vs per scope | 06 says one shard. 13 and 16 say scope. 31 measures scope bytes | D2 | Resolved |
| C14 | Replica mirrors pages vs stores no out-of-scope rows | 15 mirrors authority pages. 13 forbids out-of-scope rows | D2, D3 | Resolved |
| C15 | Replica mirrors index vs builds index | 15 stores index bytes. 18 builds from diffs | D3 | Resolved |
| C16 | Global ranking stats vs scope replica | 19 uses corpus stats. 15 has scope-only data | D3 | Resolved |
| C17 | Projection bound is time vs version | 12 uses age. 17 uses version distance | D5 | Resolved |
| C18 | Strict projection returns Rejected vs Queued | 12 says Rejected. 17 says Queued. 28 says no Queued | D5 | Resolved |
| C19 | Policy change invalidates projection vs canary | 17 invalidates on policy change. 27 serves two policy versions | D5 | Resolved |
| C20 | Client cancel writes Rejected vs one immutable receipt | 10 lets the client write Rejected. 09 and 10 I4 forbid it | D6 | Resolved |
| C21 | Rejected leaves no trace vs audit and receipt | 03 says no trace. 10 and 12 require receipt and audit | D6 | Resolved |
| C22 | Rejected does not consume the idempotency key | 03 open question 3 vs 09 one terminal receipt | D6 | Resolved |
| C23 | Per-key ack vs per-shard cursor | 24 promises per-key ack. 26 recommends per-shard cursor | D7 | Resolved |
| C24 | Event stream is the only path vs audit and budget events | 24 says only committed writes. 12 and 20 add other events | D7 | Resolved |
| C25 | Event payload by reference vs self-contained | 24 allows references. 26 handler must not read authority | D7 | Resolved |
| C26 | Search fails whole vs partial coverage | 19 fails whole. 20 allows partial. 13 promises partial | D8 | Resolved |
| C27 | Three merge orders | 19 uses score and key. 20 uses sort key, shard, and key. 22 uses shard and key | D8 | Resolved |
| C28 | Scope exactness vs eviction and admission | 13 says no eviction. 15 evicts. 20 admits until `max_keys` | D9 | Resolved |
| C29 | Authority pin expiry vs offline read | 19 pins expire. 15 and 23 promise offline reads | D10 | Resolved |
| C30 | Read-your-writes vs local visibility lag | 23 promises the write is visible. 10 says local visibility may lag | D10 | Resolved |
| C31 | Scalar session minimum vs version vector | 23 uses a scalar. 13 uses a vector | D10 | Resolved |
| C32 | Hydration double generation vs memory ceiling | 16 builds a new generation beside the old. 15 I7 caps resident bytes | D11 | Resolved |
| C33 | Lease epoch invalidates sealed snapshot | 16 ties snapshot validity to the lease. 15 promises byte identity | D2, D11 | Resolved |
| C34 | Replica holds no lease vs multi-tab lease | 04 I7 says no lease. 15 uses a local lease | D11 | Resolved |
| C35 | Four migration models | 05 dual-read, 06 upgrade tool, 28 offline freeze, 32 on-open migration | D12 | Resolved |
| C36 | Pointer rollback vs no downgrade | 28 says rollback is a pointer flip. 32 says no downgrade | D12 | Resolved |
| C37 | Global backup point vs no global pin | 28 wants one consistency point. 13 and 23 reject a global pin | D12 | Resolved |
| C38 | Restore loses receipts or events | 28 restores objects. 10 and 24 have retention horizons | D12 | Resolved |
| C39 | Global manifest digest vs policy canary | 07 binds one digest. 27 serves two policy versions | D13 | Resolved |
| C40 | Manifest is incomplete | 10, 12, 17, 18, 19, 24, 27, and 28 add manifest fields | D13 | Resolved |
| C41 | Control RPC vs manifest-only RPC | 25 forbids hand-written methods. 26, 27, 28, and 29 need control calls | D13 | Resolved |
| C42 | One evaluator vs two evaluators | 08 forbids a second evaluator. 12 and 29 require one for tests | D14 | Resolved |
| C43 | Grammar freeze vs AST freeze | 08 freezes grammar. 32 freezes AST | D14 | Resolved |
| C44 | Per-batch verdict vs per-row expression | 03 uses a batch verdict. 08 evaluates one row. 12 audits one verdict | D14 | Resolved |
| C45 | Error names and namespaces differ | `CROSS_SHARD_BATCH_REJECTED` vs `CrossShardMutationRejected`, and others | D16 | Resolved |
| C46 | Version axes and terms are incomplete | 32 says three axes. The system has at least twenty version names | D17 | Resolved |
| C47 | No reserved namespace | Receipts, events, audit, projection, index, catalog, lease, and manifest all need keys | D18 | Resolved |
| C48 | One cursor name for many cursors | 01, 14, 16, 19, 24, 10, 17, and 20 use different cursor fields | D18 | Resolved |
| C49 | Every verdict audited vs read scope evaluation | 29 audits every verdict. Offline replicas evaluate reads without authority | D19 | Resolved |
| C50 | External witness vs no network on commit | 12 forbids network on commit. 29 requires a witness | D19 | Resolved |
| C51 | Scheduler I1 contradicts per-key FIFO | 11 I1 allows one unresolved request per key. The queue allows many | D20 | Resolved |
| C52 | Page identity is used but not defined | 15 and 04 use page identity. 06 has no page identity field | D2 | Resolved |
| C53 | Snapshot provenance is incomplete | 06 lacks manifest, policy, map, tokenizer, statistics, and scope fields | D17 | Resolved |
| C54 | Negotiation is duplicated | 14 has a connection handshake. 25 has a pre-call negotiation. 16 has a scope descriptor | D4 | Resolved |
| C55 | Policy crosses RPC vs policy never crosses RPC | 14 subscription carries a policy expression. 25 says policy never crosses RPC | D4 | Resolved |
| C56 | Regex and fuel are outside the search and policy contracts | 29 adds regex caps and fuel. 18 and 19 have no regex. 08 uses steps | D19 | Resolved |
| C57 | One fact record vs many facts | 29 says one fact record. 17 allows a declared fact set | D19 | Resolved |
| C58 | `Queued` can live forever vs no receipt stays Queued | 17 keeps a lagging write Queued. 28 says no receipt stays Queued | D5 | Resolved |
| C59 | Event timestamp and origin key can break identity and privacy | 24 adds `committed_at` and `origin_idempotency_key` | D7 | Resolved |
| C60 | Manifest canonical JSON has no number rule | 07 uses canonical JSON. 02 forbids floats | D13 | Resolved |
| C61 | Manifest and map validation form a cycle | 07 validates prefixes against 22. 22 consumes 07 tables | D13 | Resolved |
| C62 | System sub-namespace placement is undefined | 10, 12, 17, 18, and 24 say "reserved prefix" but do not define the key layout | D1, D18 | Resolved |
| C63 | Native/wasm parity is missing from the freeze register | 02 requires byte identity. 32 lists five contracts | D21, D15 | Resolved |
| C64 | Freeze and support policy is incomplete | 32 has open questions on support window and downgrade | D23 | Resolved |
| C65 | Qualification suite starts too late | 30 says it begins in Phase A. Roadmap places it in Phase I | D22, D24 | Resolved |
| C66 | Falsifier thresholds are not tied to a gate | 31 defines thresholds. The roadmap does not make them release gates | D22 | Resolved |
| C67 | Read access logging is not separated from the write audit chain | 29 audits every verdict. 12 audits committed writes | D19 | Resolved |
| C68 | Control operations are outside the generated RPC surface | 25 forbids hand-written methods. 27 and 28 need control operations | D13 | Resolved |

## 12. Traceability matrix

| Essay | Decisions | Required edits | Vector or test |
| --- | --- | --- | --- |
| 01 KV store | D1, D2, D8, D18 | Add system marker, page identity, cursor types, exact and coverage | Ordered KV vectors |
| 02 Native and wasm | D4, D14, D17, D21 | Inject time and entropy, remove platform branches, test oracle off-path, version axes | Byte identity vectors |
| 03 Hooks | D6, D7, D14 | Rejection writes receipt and audit, post-write events, per-row evaluation | Hook vectors |
| 04 Tiering and leases | D1, D2, D11, D18 | System sub-namespaces, seal versus lease, one backend in wave 1 | Lease kill and tier identity |
| 05 Key encoding | D1, D12, D17, D18 | System marker, table grammar, key encoding annex, no split | Key vectors |
| 06 Snapshot and page | D2, D11, D17, D18 | Page id and sequence, provenance, seal split, ScopeSnapshot | Snapshot and page vectors |
| 07 Manifest | D1, D13, D17 | Table names, policy catalog, manifest sections, canonical JSON, no map validation | Manifest vectors |
| 08 Policy and AST | D14, D16, D19 | AST freeze, step budget, one evaluator plus test oracle, fact projection | Policy vectors |
| 09 Versioned writes | D1, D6, D8, D10 | Cross-shard error, receipt states, version names, audit link | Conflict vectors |
| 10 Receipts and outbox | D6, D7, D10, D16 | Local cancel, no fifth state, retention, error names | Receipt state vectors |
| 11 Scheduler | D6, D20 | Add `in_flight`, per-key FIFO, no policy, no terminal writes | Scheduler vectors |
| 12 Enforcement and audit | D5, D6, D16, D19 | Version bound, strict versus queued, audit chain, read access log | Revocation vectors |
| 13 Scope | D1, D2, D8, D9 | Members only, ScopeSnapshot, coverage mode, admission limits | Scope vectors |
| 14 Sync wire | D4, D8, D10, D18 | Custom sync frames, `SessionHello`, cursor types, policy opaque | Wire vectors |
| 15 Client replica | D2, D3, D10, D11 | Scope-projected pages, build index, local pin, tab lease, peak budget | Replica vectors |
| 16 Hydration | D2, D11, D17 | ScopeSnapshot, peak budget, seal versus lease, provenance | Crash matrix |
| 17 Projection | D5, D13, D16 | Version distance, fact catalog, canary epoch, error names | Projection vectors |
| 18 Inverted index | D3, D18 | Replica build, document-local scoring, tokenizer annex | Tokenizer vectors |
| 19 Search | D3, D8, D10 | Document-local scoring, exact and coverage, comparator, pin types | Search vectors |
| 20 Cost model | D3, D8, D9 | No global stats, coverage descriptor, `max_keys` admission, comparator | Explain vectors |
| 21 Replication seam | D7, D11, D12 | Lease as driver, sealed snapshots, event order, term | Leader-kill test |
| 22 Shard map | D1, D13, D17 | No split, table assignment, map digest, no manifest validation | Map vectors |
| 23 Bounded staleness | D8, D10 | Version vector session, pin types, exact and coverage | SLO report |
| 24 Event stream | D7, D16, D17 | Business versus operational, per-key ack, envelope fields, injected time | Event vectors |
| 25 Generated RPC | D4, D13, D18 | Connect envelope, `SessionHello`, control namespace, descriptor | RPC vectors |
| 26 Effect consumer | D7, D16 | Per-key cursor, self-contained payload, DLQ, ack | Consumer matrix |
| 27 Rule lifecycle | D5, D6, D13 | Policy catalog, canary epoch, receipt detail, rollback | Lifecycle vectors |
| 28 Operations | D2, D10, D12, D19 | Backup vector, restore receipts and events, migration, audit | Backup and restore tests |
| 29 Security | D14, D16, D19 | No regex, step budget, auth, async witness, read access log | Fuzz and tamper tests |
| 30 Qualification | D15, D17, D22, D24 | All annex vectors, wave 0, browser gate | Suite report |
| 31 Falsifiers | D3, D9, D10, D22 | Scope bytes, document-local scoring, coverage, SLO | Falsifier report |
| 32 1.0 freeze | D15, D17, D23, D24 | All surfaces, support policy, freeze checklist | Release checklist |

## 13. Freeze readiness checklist

### 13.1 Governance

- [ ] The contract register has one row for every frozen surface.
- [ ] Every row names a parent, version, spec, changelog, and migration path.
- [ ] Every essay that says "freeze" names a register row.
- [ ] `32-one-zero-freeze.md` lists every register row.
- [ ] No frozen surface depends on a Rust type layout, serde default, or enum discriminant.
- [ ] The support and deprecation policy is published.
- [ ] The change-control process is approved.

### 13.2 Terms and versions

- [ ] Every public contract uses qualified version names.
- [ ] A lint rejects a bare "version" in a public contract.
- [ ] Every artifact carries the required provenance subset.
- [ ] The version axes table is complete.
- [ ] The content digest and content seal are separate.
- [ ] A reader verifies the content digest before the seal.
- [ ] A reader rejects a missing required provenance field with `format.missing_provenance`.
- [ ] A reader rejects an unknown major version with `format.unsupported_version`.

### 13.3 Contracts and annexes

- [ ] Every core contract has a vector corpus.
- [ ] Every annex has a vector corpus.
- [ ] Every vector runs on native and wasm32.
- [ ] A released vector cannot change.
- [ ] CI rejects an edit to a released vector.
- [ ] Every contract has a second in-tree decoder.
- [ ] Every seam has two in-tree implementations.
- [ ] The test-only policy oracle stays outside the production build.

### 13.4 Registries

- [ ] The error registry covers every error in the essays.
- [ ] No error code has two causes.
- [ ] Every receipt state maps to one or more codes.
- [ ] Every code has one retry class.
- [ ] The reserved system identifiers are complete.
- [ ] Every cursor type has a schema and a vector.
- [ ] The budget registry is complete.
- [ ] The retention registry is complete.
- [ ] The digest registry is complete.

### 13.5 Storage and keys

- [ ] A data shard contains exactly one user table.
- [ ] A shard contains the system sub-namespaces for its user table.
- [ ] A user key cannot contain `0xFF` after the table prefix.
- [ ] A system key uses a registered identifier.
- [ ] A user table name cannot start with `_`.
- [ ] `_catalog` is the only top-level system table in 1.0.
- [ ] A table does not split across shards.
- [ ] A map rejects a split table with `map.table_split_unsupported`.
- [ ] The map is total and disjoint.
- [ ] The map routes every key to one shard on native and wasm32.
- [ ] A cross-shard batch fails with `shard.cross_mutation_rejected`.
- [ ] The B-tree and LSM engines return the same ordered results.

### 13.6 Manifest and policy

- [ ] The manifest declares every section in D13.
- [ ] The manifest does not embed mutable rule bodies.
- [ ] The manifest digest does not include the active policy version.
- [ ] The policy catalog is separate and content-addressed.
- [ ] The policy AST is canonical and frozen.
- [ ] The source syntax is versioned.
- [ ] One production evaluator serves read scope and write validation.
- [ ] A test-only oracle is outside the production build.
- [ ] Policy evaluation is per row.
- [ ] A batch verdict is the conjunction of row verdicts.
- [ ] A request produces one receipt and one audit record with a decision list.
- [ ] Budget exhaustion maps to `policy.budget_exhausted` and `Rejected`.
- [ ] A policy with an unknown path fails with `manifest.rule_unknown_path`.
- [ ] A policy over budget fails with `manifest.rule_unbounded`.
- [ ] A client cannot write a projection.

### 13.7 Write path

- [ ] A write carries an idempotency key.
- [ ] A write carries a base version for each mutated key.
- [ ] The check order is authentication, cross-shard, version, policy, post-write, commit.
- [ ] A base mismatch returns `Conflict`.
- [ ] A base ahead returns `Rejected` with `write.stale_base_version`.
- [ ] A cross-shard write fails before scheduling.
- [ ] A `Rejected` write changes no data, index, or business event.
- [ ] A `Rejected` write writes a receipt and an audit record.
- [ ] A terminal receipt is immutable.
- [ ] One idempotency key produces at most one terminal receipt.
- [ ] The scheduler uses `queued`, `in_flight`, and `terminal`.
- [ ] At most one request per key is `in_flight`.
- [ ] Per-key FIFO holds across all interleavings.
- [ ] A blocked key does not delay a ready key.
- [ ] The scheduler does not evaluate policy.
- [ ] The scheduler does not write terminal states.
- [ ] A write, its index, receipt, audit, and business events commit in one transaction.

### 13.8 Projection and scope

- [ ] The projection bound is a version distance.
- [ ] Time does not enter a policy verdict.
- [ ] Default lag keeps the receipt `Queued`.
- [ ] The authority enforces a queue deadline.
- [ ] Strict mode returns `authorization.stale_facts`.
- [ ] Projection invalidation follows fact-set, schema, ownership, or revocation changes.
- [ ] A canary cohort uses its own projection epoch.
- [ ] A scope identity is deterministic.
- [ ] A scope version is a version vector.
- [ ] A scope read uses one version vector.
- [ ] A scope reports removals with key, version, and reason.
- [ ] A scope declares `max_members`, `max_bytes`, and `max_shards`.
- [ ] An over-limit scope fails at subscribe.
- [ ] A coverage result carries a coverage descriptor.
- [ ] An exact result requires every targeted shard.
- [ ] A replica answers its scope offline at its local pin.

### 13.9 Snapshot, hydration, and replica

- [ ] A ShardSnapshot covers one shard at one commit version.
- [ ] A ScopeSnapshot covers one scope at one version vector.
- [ ] A ScopeSnapshot contains no out-of-scope user rows.
- [ ] A page has `page_id`, `page_sequence`, and provenance.
- [ ] The directory has its own integrity value.
- [ ] The content digest covers the directory and page digests.
- [ ] The content seal covers the content digest and lease term.
- [ ] The lease term does not enter the content digest.
- [ ] A sealed snapshot survives a lease change.
- [ ] Hydration uses a new generation and one pointer swap.
- [ ] Hydration peak storage is part of the device budget.
- [ ] A crash leaves exactly one published generation.
- [ ] A duplicate frame changes no state.
- [ ] A missing sequence number forces a re-request.
- [ ] A cold replica returns `read.not_hydrated`, not an empty result.
- [ ] A replica stores scope-projected pages without re-encoding.
- [ ] A replica builds its own search index.
- [ ] A replica uses one cursor advancer and one local tab lease.
- [ ] A replica does not hold an authority write lease.

### 13.10 Wire and RPC

- [ ] The sync core is sans-IO.
- [ ] `SessionHello` negotiates the descriptor, sync version, manifest digest, map version, capabilities, and limits.
- [ ] A scope subscription carries its policy version and scope pin.
- [ ] A data call cannot precede `SessionHello`.
- [ ] One session cannot renegotiate.
- [ ] A diff is ordered and gap-free.
- [ ] A diff states when a key leaves the scope.
- [ ] The cursor is opaque, bounded, tamper-evident, and valid after restart.
- [ ] The replica detects a gap before applying a later diff.
- [ ] Snapshot fallback sends a page manifest, pages, snapshot version, and tail cursor.
- [ ] Flow control is credit based.
- [ ] A slow consumer returns `consumer.too_slow` without stalling a shard.
- [ ] Connect carries sync frames as opaque bytes.
- [ ] RPC does not re-encode a diff.
- [ ] The descriptor is deterministic from the manifest.
- [ ] Control operations use the `control` namespace.
- [ ] Every receipt and error variant maps to one generated code.

### 13.11 Search and cost

- [ ] Index maintenance is inside the write transaction.
- [ ] The data page, index page, and commit record are one atomic unit.
- [ ] A delete tombstones every posting in the same commit.
- [ ] The tokenizer is frozen by identifier and version.
- [ ] The replica index build uses the frozen tokenizer and scorer.
- [ ] Ranking uses document-local features only.
- [ ] Ranking uses integer arithmetic.
- [ ] Search uses one version vector.
- [ ] A search and a get at one pin agree.
- [ ] Scope filtering runs before top-k.
- [ ] Pagination uses keyset cursors.
- [ ] The search comparator is `(score desc, canonical key asc)`.
- [ ] The general query comparator does not use shard id as a semantic tie-break.
- [ ] A fan-out above the bound fails with `search.fanout_exceeded`.
- [ ] Explain is byte-identical on native and wasm32.
- [ ] `max_keys` is an admission limit, not a runtime truncation.

### 13.12 Replication, map, and staleness

- [ ] One driver interface exposes acquire, renew, release, append, commit index, read at index, member set, and transfer.
- [ ] One fence value crosses the interface.
- [ ] The fence is monotonic.
- [ ] The storage boundary compares the write fence with the owner fence.
- [ ] A lower fence returns `replication.stale_epoch`.
- [ ] The log lives on low-latency durable storage.
- [ ] The log does not live on an object store.
- [ ] The leader-kill test runs in CI.
- [ ] Both drivers pass one conformance suite.
- [ ] The lease driver is a degenerate case of the same interface.
- [ ] A shard has one ownership mechanism.
- [ ] The map is static and versioned.
- [ ] A map mismatch is refused at handshake.
- [ ] A cross-shard read uses a version vector.
- [ ] A read over its bound returns `read.stale`.
- [ ] A session token carries a version vector minimum.
- [ ] A read-your-writes read blocks or returns `read.behind_commit`.

### 13.13 Events and consumer

- [ ] The business event stream is authority-only.
- [ ] A business event commits with its write.
- [ ] A rejected or conflict write emits no business event.
- [ ] The event envelope is canonical and versioned.
- [ ] The event id is deterministic.
- [ ] `committed_at` is an injected parameter.
- [ ] The envelope carries `origin_request_id`, not the raw idempotency key.
- [ ] Delivery is at least once.
- [ ] Order and ack are per key.
- [ ] A poison event blocks only its key.
- [ ] The operational feed is separate from the business stream.
- [ ] The consumer uses a completion record and a provider idempotency key.
- [ ] The consumer does not write the authority.
- [ ] A dead-letter record commits with its cursor advance.
- [ ] Event retention has a time bound, a byte bound, or both.

### 13.14 Rule lifecycle, operations, and security

- [ ] A policy version is immutable and content-addressed.
- [ ] A rule change is a write with a receipt and an audit record.
- [ ] A dry run uses a pinned sample.
- [ ] A canary serves at most two policy versions.
- [ ] A rollback activates a prior version without content edit.
- [ ] A read-widen change requires a security approver.
- [ ] A multi-table change produces one receipt per table.
- [ ] The catalog stays writable while data-plane policy denies every row.
- [ ] Backup uses a version vector and a retention pin.
- [ ] Restore preserves receipts and event cursors, or marks them expired.
- [ ] Restore does not re-execute an effect.
- [ ] A migration quiesces writes, drains the scheduler, rewrites, verifies, and flips.
- [ ] Rollback is allowed only before the first new-format write.
- [ ] No downgrade is allowed after the first new-format write.
- [ ] Authentication precedes authorization.
- [ ] An unauthenticated request receives no receipt.
- [ ] An audit record has the required fields.
- [ ] The audit chain is hash-linked and checkpointed.
- [ ] The external witness is asynchronous.
- [ ] A failed audit append rejects the write with `audit.unavailable`.
- [ ] A failed audit append leaves no receipt and changes no state.
- [ ] Read scope evaluation writes to the access log, not the audit chain.
- [ ] Policy uses a step budget, not fuel.
- [ ] Search has no regex in 1.0.
- [ ] Secrets do not enter policy, audit, error text, traces, or events.

### 13.15 Qualification and falsifiers

- [ ] The suite starts in wave 0.
- [ ] The matrix runs native, browser, and native again.
- [ ] No scenario has a target-specific branch.
- [ ] Every core contract and annex has a vector corpus.
- [ ] Every decoder has a fuzz target.
- [ ] The policy evaluator has a fuzz target.
- [ ] The suite injects every named fault.
- [ ] The suite uses a logical clock and a fixed seed.
- [ ] The suite produces one report and one narration.
- [ ] The browser tier gates the release.
- [ ] The falsifier workload spec has a hash.
- [ ] All five falsifier families run from one spec.
- [ ] The gate fails on a crossed threshold in two of three runs.
- [ ] A non-zero correctness counter fails the gate.
- [ ] A committed baseline exists for the 1.0 spec set.
- [ ] The report names hardware class, toolchain version, and commit.
- [ ] The wasm target reports the same operation hash as native.

### 13.16 Support and migration

- [ ] A reader accepts the current format and one back.
- [ ] The wire keeps one extra read version for the major.
- [ ] A format is removed only at a major, after 12 months of notice, and with an automated migration.
- [ ] The 1.x line receives fixes for 24 months after the next major.
- [ ] The last minor receives security fixes for 12 months.
- [ ] Two minors stay supported.
- [ ] The deprecation policy is published.
- [ ] The migration policy is published.
- [ ] The no-downgrade rule is published.
- [ ] A v1 snapshot opens on the release build and the previous minor.
- [ ] A 1.0 wire client resumes after seven days offline without a full resync.

## 14. Open items and deferred work

### 14.1 Items resolved by this specification

These items are no longer open. The listed decision resolves them.

| Item | Decision |
| --- | --- |
| Table split in 1.0 | D1: not supported |
| System key placement | D1 and D18: `0xFF` marker and system IDs |
| Snapshot unit | D2: ShardSnapshot and ScopeSnapshot |
| Replica index | D3: build from scope members |
| Global ranking statistics | D3: not in 1.0 ranking |
| Wire versus RPC | D4: custom sync frames over Connect |
| Projection bound | D5: version distance |
| Client cancel | D6: local outbox disposition |
| Per-key event ack | D7: required |
| Partial results | D8: exact by default, coverage opt-in |
| Scope eviction | D9: residency only |
| Pin lifetimes | D10: separate authority and replica pins |
| Hydration peak | D11: part of device budget |
| Migration model | D12: one framework |
| Backup consistency | D12: per-shard vector |
| Manifest digest and policy version | D13: separate |
| Policy AST freeze | D14: AST, not grammar |
| Frozen surface count | D15: five core contracts plus annexes |
| Error naming | D16: one registry |
| Version axes | D17: qualified axes |
| Cursor types | D18: one type per role |
| Audit read verdicts | D19: separate access log |
| Scheduler states | D20: queued, in_flight, terminal |
| Native/wasm parity | D21: one core and adapters |
| Qualification start | D22: wave 0 |
| Freeze and support | D23: complete register and support window |
| Phase order | D24: contract waves |

### 14.2 Numeric values to calibrate before the freeze

These values do not change the contract shape. They must be measured and published before wave 7.

- Authority lease duration and guard time.
- Queue deadline for `Queued` receipts.
- Receipt retention window.
- Event retention time and byte bounds.
- Audit retention floor.
- Projection staleness bound per fact source.
- Read pin retention.
- Sync cursor retention.
- Search pin retention.
- Scope device budgets per device class.
- Scope hydration overhead per device class.
- Replica memory ceiling.
- Policy step budget per rule.
- Policy depth budget.
- Policy memory arena size.
- Query cost budget.
- Query fan-out bound.
- Event payload bound.
- Index overhead budget.
- Key, value, batch, and page size limits.
- Cold-start budget per scope class.
- Read p99 budget.
- SLO availability and staleness targets.
- `max_outbox_age`, `max_ack_delay`, and restore window.
- Support window start date.

### 14.3 Deferred to post-1.0

- Multiple user tables in one data shard.
- Table split across shards.
- A global read version.
- Global corpus ranking statistics.
- Regular expression search.
- Cross-shard two-phase commit.
- A placement control plane.
- A global backup consistency service.
- An operator-run external audit witness service.
- WASM-based untrusted policy extensions.
- Per-field versions for wide records.
- Scope composition.
- A fifth receipt state.
- A local optimistic writer in the replica.
- A native fast path that skips a check.
- SHA-256 as an alternative content digest.
- Encryption and compression policy beyond the reserved header fields.
- A browser storage primitive selection beyond the two in-tree backends.
- A WASM threads requirement.
- A single async facade instead of step functions.
- A normative WASI target.
- A binary codec selection beyond negotiation.
- A WebSocket adapter beyond the Connect transport.
- A query language beyond the declared policy grammar.
- A rule that rewrites the batch.
- A rule that reads the network.
- A rule that reads a clock.
- A receipt table for operator queries.
- A control plane that writes outside a receipt.
- A per-shard cost weight for cold tier reads.

### 14.4 Open questions that need a product decision

These questions do not block the contract shape. They block a product promise.

| Question | Recommendation |
| --- | --- |
| Is the browser tier a 1.0 release gate or a nightly gate? | Release gate for the full matrix. Nightly for the full cross product. |
| Is a 1 GiB scope supported in a browser? | Only for a declared device class with a measured budget. |
| Does a scope survive a policy version change offline? | Yes, at its pinned version. New policy applies on reconnect. |
| Does a read-widen change need a quarantine stage? | Yes, before auto-promotion. |
| Is the external audit witness mandatory? | Optional for 1.0. Required for regulated deployments. |
| Does a tenant administrator author rules? | Only with separation of duties and a second approver. |
| Is a multi-table release atomic? | No. One receipt per table. |
| Is a global backup point required? | Only with a global write quiesce. |
| Is a cross-shard read allowed to return coverage? | Yes, only with an explicit request. |
| Is a scope larger than the device supported? | Only under a separate degraded contract. |

## 15. Change control

- A change to a core contract or an annex MUST update section 6.
- A change to a frozen surface MUST create a new contract version and a migration note.
- A change to an error code MUST update section 8.3 and the vectors.
- A change to a system identifier MUST update section 8.4 and the key vectors.
- A change to a cursor type MUST update section 8.5 and the wire vectors.
- A change to a budget or retention rule MUST update section 8.6 or 8.7.
- A change to a digest or checksum MUST update section 8.8 and the artifact vectors.
- A change to a decision MUST update the conflict register and the traceability matrix.
- A change to a decision MUST name the old decision and the reason for the change.
- A change to this document MUST increment the revision.
- A change to a frozen contract MUST be approved by the contract owner.
- A change to a security rule MUST be approved by the security owner.
- A change to a public promise MUST be approved by the product owner.
- A reviewer MUST use the checklist in section 13 before approval.
- A reviewer MUST reject an essay that conflicts with this document.
- A reviewer MUST reject a vector edit to a released contract.

## 16. Appendices

### Appendix A. Decision summary

| Decision | Summary |
| --- | --- |
| D1 | Data shard contains one user table plus system sub-namespaces. No split. |
| D2 | ShardSnapshot and ScopeSnapshot are separate artifacts. Page identity is explicit. Seal is separate from lease. |
| D3 | Replica stores scope-projected pages and builds its own index. Document-local scoring. |
| D4 | Custom sync frames over Connect. One `SessionHello`. Policy payload is opaque. |
| D5 | Projection bound is a version distance. Default Queued with deadline. Strict Rejected. |
| D6 | Four receipt states. Client cancel is local. Terminal verdicts consume the idempotency key. |
| D7 | Business events and operational signals are separate. Per-key ack. Self-contained effect payloads. |
| D8 | Exact mode by default. Coverage mode opt-in. One comparator per query class. |
| D9 | Scope membership is exact. Residency is separate. A 1.0 offline scope must fit the device budget. |
| D10 | Separate authority pin, replica pin, resume cursor, and receipt retention. Session is a vector. |
| D11 | Hydration peak is in the budget. Seal is separate from lease. Replica has no authority lease. |
| D12 | One migration framework. Backup uses a version vector. Restore preserves receipts and events. |
| D13 | Manifest digest excludes the active policy version. Policy catalog is separate. Control is generated. |
| D14 | Freeze the policy AST and evaluation. One production evaluator. Per-row evaluation. |
| D15 | Five core contracts plus annexes. All freeze at 1.0. |
| D16 | One error registry. Lowercase dot names. One cause per code. |
| D17 | Qualified version axes. Provenance per artifact. Content digest and seal are separate. |
| D18 | `0xFF` system marker and registered system IDs. One cursor type per role. |
| D19 | Write audit chain plus read access log. Async witness. Step budget. No regex. |
| D20 | Scheduler uses queued, in_flight, and terminal. At most one in-flight per key. |
| D21 | One core and thin adapters. Native and wasm byte identity. |
| D22 | Qualification starts in wave 0. Falsifiers gate the freeze. |
| D23 | Freeze all surfaces. Reader current plus one back. Published support window. |
| D24 | Contract waves and implementation waves. No circular gate. |

### Appendix B. Manifest outline

| Section | Required | Key fields |
| --- | --- | --- |
| `schema` | Yes | `schema_version`, `revision`, `digest` |
| `tables` | Yes | `name`, `key_fields`, `value_fields`, `limits` |
| `indexes` | Yes | `fields`, `tokenizer`, `positions`, `searchable`, `filterable`, `overhead_budget`, `scorer` |
| `rules` | Yes | `id`, `selector`, `effect`, `scope`, `cost_bound`, `strict`, `retention_class`, `audit_class`, `facts` |
| `facts` | Yes if rules read facts | `id`, `type`, `owner_shard`, `fan_in_cap`, `staleness_bound` |
| `operations` | Yes | `name`, `table`, `request`, `response`, `streaming`, `errors`, `auth_class` |
| `events` | Yes if rules emit events | `kind`, `payload_schema`, `required_fields`, `retention_class` |
| `receipts` | Yes | `retention`, `reason_codes`, `retry_classes` |
| `audit` | Yes | `visibility_class`, `retention_class`, `export_class` |
| `projection` | Yes if rules read facts | `epoch_rule`, `watermark_rule`, `invalidation_rule` |
| `codecs` | Yes | `format_versions`, `tokenizer_versions`, `digest_algorithms` |
| `control` | Yes | `backup`, `restore`, `migrate`, `rule_activate`, `rule_rollback`, `dlq_redrive`, `quota`, `audit_export` |

### Appendix C. Receipt state machine

```text
Authority receipt:
  Queued -> Committed
  Queued -> Rejected
  Queued -> Conflict
  Committed -> terminal
  Rejected -> terminal
  Conflict -> terminal

Local outbox:
  Pending -> CancelRequested
  CancelRequested -> Abandoned
  CancelRequested -> Committed   (late authority verdict wins)
  Pending -> Committed
  Pending -> Rejected
  Pending -> Conflict
  Abandoned -> terminal
```

Rules:

- A local state is not a receipt.
- A late authority verdict wins for the data.
- A terminal receipt never changes state.
- One idempotency key produces one terminal receipt.
- A `Rejected` verdict consumes the key for the same body.
- A corrected body uses a new key.

### Appendix D. Event envelope

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `envelope_version` | string | Yes | `oxkv/event/v1` |
| `event_id` | 32-byte hash | Yes | Deterministic from commit fields |
| `shard_id` | bytes | Yes | Opaque and stable |
| `table` | string | Yes | User table name |
| `key` | bytes | Yes | Canonical key |
| `key_sequence` | integer | Yes | Per-key order |
| `commit_version` | integer | Yes | Shard commit |
| `committed_at` | integer | Yes | Injected commit parameter |
| `rule_id` | string | Yes | Manifest rule identity |
| `policy_version` | 32-byte hash | Yes | Pinned policy |
| `emission_index` | integer | Yes | Order inside one commit |
| `payload` | bytes or map | Yes | Bounded |
| `payload_schema` | string | Yes | Payload type |
| `origin_request_id` | bytes | Yes | Opaque request identity |

### Appendix E. Snapshot header

| Field | Required | Notes |
| --- | --- | --- |
| `format_version` | Yes | Major and minor |
| `key_encoding_version` | Yes | Annex version |
| `required_features` | Yes | Bitset |
| `shard_id` | ShardSnapshot | Opaque |
| `prefix_bounds` | ShardSnapshot | Half-open |
| `commit_version` | ShardSnapshot | Scalar |
| `version_vector` | ScopeSnapshot | Per-shard |
| `scope_identity` | ScopeSnapshot | Canonical hash |
| `manifest_digest` | Yes | Content hash |
| `map_version` | Yes | Integer |
| `map_digest` | Yes | Content hash |
| `policy_version` | ScopeSnapshot | Content hash |
| `tokenizer_versions` | Index | Per field |
| `statistics_version` | Optional | 1.0 optional |
| `projection_epoch` | Optional | Policy-filtered scope |
| `projection_watermarks` | Optional | Per fact source |
| `page_size` | Yes | Bytes |
| `page_count` | Yes | Integer |
| `directory_offset` | Yes | Bytes |
| `directory_length` | Yes | Bytes |
| `content_digest` | Yes | BLAKE3 |
| `content_seal` | Yes | Content digest plus lease term plus signature |

### Appendix F. Wire message list

| Message | Direction | Purpose |
| --- | --- | --- |
| `SessionHello` | Client to authority | Negotiate descriptor, sync version, manifest, map, capabilities, limits |
| `SessionReady` | Authority to client | Chosen versions, limits, session identity |
| `Subscribe` | Client to authority | Scope declaration, policy version, scope pin, cursor or snapshot request |
| `Subscribed` | Authority to client | Scope identity, effective policy, starting version, cursor |
| `Diff` | Authority to client | One ordered change |
| `DiffBatch` | Authority to client | Bounded diff batch |
| `Credit` | Client to authority | Byte and message credit |
| `Keepalive` | Both | Liveness |
| `SnapshotOffer` | Authority to client | Snapshot identity, version, page count |
| `SnapshotPage` | Authority to client | One verified page |
| `SnapshotTail` | Authority to client | Tail cursor and start version |
| `SnapshotComplete` | Authority to client | Snapshot verified and published |
| `WriteSubmit` | Client to authority | Request with idempotency key and base version |
| `Receipt` | Authority to client | Terminal or queued outcome |
| `ReceiptResume` | Client to authority | Resume receipt replay from a cursor |
| `Error` | Both | Named error with retry class |

### Appendix G. Review checklist for an essay update

- [ ] The essay uses the terms in section 4.
- [ ] The essay uses qualified version names.
- [ ] The essay names the correct contract or annex row.
- [ ] The essay uses the `0xFF` system marker for system keys.
- [ ] The essay uses `ShardSnapshot` or `ScopeSnapshot` correctly.
- [ ] The essay uses the correct cursor type.
- [ ] The essay uses the error registry names.
- [ ] The essay uses the budget and retention registry.
- [ ] The essay does not claim a global version for a cross-shard read.
- [ ] The essay does not claim exactness for a coverage result.
- [ ] The essay does not promise offline data that does not fit the device budget.
- [ ] The essay does not add a fifth receipt state.
- [ ] The essay does not add a second production evaluator.
- [ ] The essay does not add a network call to the commit path.
- [ ] The essay does not add a regex to 1.0 search.
- [ ] The essay does not use fuel in a public contract.
- [ ] The essay names the wave and the gate.
- [ ] The essay has a vector or a test for every MUST.
- [ ] The essay has no open question that changes a frozen byte.
- [ ] The reviewer has checked section 13.




