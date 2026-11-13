# oxkv 1.0 freeze and support policy

| Field | Value |
| --- | --- |
| Status | Published |
| Date | 2026-11-12 |
| Scope | The contract register, freeze states, reader window, support policy, and removal policy |
| Normative source | `specs.md` sections 6, 7.15, 7.17, 7.23, 7.24, 9.12, and 13 |
| Crate | `crates/oxkv-freeze` |

## 1. Purpose

1.0 is a freeze, not a feature count. It fixes the declarative surface, the
bytes, and the meaning of a write outcome. The Rust API stays free to change.

A frozen surface crosses a boundary that the release cannot rebuild. A browser
bundle ships once. A snapshot stays in object storage for years. An outbox
entry waits for a receipt. The register below is the complete list of these
surfaces.

## 2. Contract and annex register

Every frozen surface has one row. A core contract has no parent. An annex names
one core contract as parent. The version text has the `name/vN` form.

### 2.1 Core contracts

| ID | Surface | Version | Parent | Specification | Vectors | Migration |
| --- | --- | --- | --- | --- | --- | --- |
| C1 | Manifest schema | `oxkv/manifest/v1` | none | `specs.md` section 6.1 | `30-qualification-suite.md#vectors-c1` | Forward manifest migration on open. No downgrade. |
| C2 | Snapshot and page format | `snapshot-page/v1` | none | `specs.md` section 6.1 | `30-qualification-suite.md#vectors-c2` | Forward only. A v1 snapshot opens on 1.0 and the previous minor. |
| C3 | Sync wire protocol | `sync-wire/v1` | none | `specs.md` section 6.1 | `30-qualification-suite.md#vectors-c3` | One extra read version for the major. No write downgrade. |
| C4 | Policy language | `policy-ast/v1` | none | `specs.md` section 6.1 | `30-qualification-suite.md#vectors-c4` | A grammar change that keeps the AST is additive. It needs no migration. |
| C5 | Receipt and error semantics | `receipt-error/v1` | none | `specs.md` section 6.1 | `30-qualification-suite.md#vectors-c5` | A new code is additive. A removed code stays reserved. |

### 2.2 Annexes

| ID | Surface | Version | Parent | Specification | Vectors | Migration |
| --- | --- | --- | --- | --- | --- | --- |
| A1 | Key encoding | `key-encoding/v1` | C2 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a1` | A new key encoding is a new major format with a forward migration. |
| A2 | Tokenizer | `tokenizer/v1` | C1 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a2` | A tokenizer change is a new annex version and a reindex. |
| A3 | Event envelope | `event/v1` | C5 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a3` | Old envelopes stay readable. A new field is additive. |
| A4 | Shard map | `oxkv/map/v1` | C1 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a4` | The map migrates under the writer lease before the first read. |
| A5 | Scope semantics | `scope/v1` | C3 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a5` | A scope change is a new scope version. No stored scope changes in place. |
| A6 | Projection staleness | `projection/v1` | C4 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a6` | A new epoch invalidates old projections. The next open rebuilds them. |
| A7 | Audit envelope | `audit/v1` | C5 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a7` | The hash chain appends. An old record stays valid. |
| A8 | Cost and explain | `cost/v1` | C1 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a8` | A cost model change keeps the explain schema and adds a version. |
| A9 | Cursor types | `cursor/v1` | C3 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a9` | A token change is a new cursor version. An old token expires. |
| A10 | Descriptor | `descriptor/v1` | C1 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a10` | A descriptor change is additive. An old binding stays valid. |
| A11 | Read pin and staleness | `read-pin/v1` | C3 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a11` | A pin mode change is a new annex version. Old sessions keep their mode. |
| A12 | Migration and backup | `migration/v1` | C2 | `specs.md` section 6.2 | `30-qualification-suite.md#vectors-a12` | Migration is forward only. An interrupted migration resumes on the next open. |

### 2.3 Frozen and free parts

| ID | Frozen parts | Free parts |
| --- | --- | --- |
| C1 | Table declarations, key schema, fields, indexes, rule metadata, facts, operations, events, receipts, audit, projection, codecs, control | Rust builders and generated code |
| C2 | Header, provenance, page identity, page sequence, checksums, framing, encoding, key order | Codec named in the header, block size, cache |
| C3 | Framing, `SessionHello`, subscribe, diff, cursor, resume, flow control | Transport and batching |
| C4 | AST, evaluation order, determinism, step budget, explanation | Source syntax sugar and diagnostics |
| C5 | States, idempotency, retry class, error registry, receipt envelope | Scheduler, outbox storage, logs |
| A1 | Version byte, table grammar, component tags, system marker, order, bounds | Table set and schemas |
| A2 | Identifier, version, Unicode data, normalization, case folding, segmentation | Ranking parameters |
| A3 | Envelope fields, event identity, ordering, retention | Consumer library |
| A4 | Routing, table assignment, bounds, version, digest | Placement control plane |
| A5 | Identity, version vector, diffs, removal reasons | Scope composition |
| A6 | Version-distance bound, epoch, watermarks | Numeric limits |
| A7 | Record fields, hash chain, checkpoints | Export tooling |
| A8 | Explain schema, cost error names | Weights and estimates |
| A9 | Token formats, fields, expiry, errors | Server-side state |
| A10 | Method, message, field, code mapping | Language bindings |
| A11 | Pin modes, session vector, staleness errors | Numeric bounds |
| A12 | Backup manifest, restore steps, migration steps | Tooling and scheduling |

### 2.4 Register rules

- Every frozen surface has one row.
- An annex names one parent core contract.
- Every row has a version, a specification, a changelog, a vector corpus, and
  a migration path.
- No frozen surface depends on a Rust type layout, a serde default, or an enum
  discriminant.
- A break to a frozen surface creates a new major contract version and a
  migration note.
- An additive change to a minor contract changes no old bytes.
- No essay uses the word "freeze" without a register row.

## 3. Freeze states

| State | Meaning | A break needs |
| --- | --- | --- |
| `draft` | The surface may break. | Nothing. |
| `candidate` | The surface has a version. | A new version and a migration note. |
| `frozen` | The surface must not break inside 1.x. | A new major contract version and a migration note. |

Phase B and Phase C items become candidates. Version 1.0 freezes every core
contract and every annex. The transition check accepts these moves:

- `draft` to `candidate`
- `draft` to `frozen`
- `candidate` to `frozen`
- one state to the same state

The transition check rejects every move out of `frozen`. It rejects
`candidate` to `draft`.

## 4. Reader window

A 1.x reader accepts the current format and one back. The 1.0 reader accepts
format `1.0` and the previous minor, `0.12`.

| Request | Result |
| --- | --- |
| Current format `1.0` | Accepted |
| Previous minor `0.12` | Accepted |
| Older minor `0.11` | `format.unsupported_version` |
| Later minor `1.1` | `format.unsupported_version` |
| Unknown major `2.0` | `format.unsupported_version` |

An unknown major returns `format.unsupported_version` before any record
applies.

A 1.x wire peer accepts the current protocol and one extra read version for the
major. The 1.0 peer writes wire version 1. It reads wire versions 1 and 2. An
unknown wire version returns `sync.unsupported_version`. The read window is
wider than the write window because a tab cannot be force-upgraded.

## 5. Support policy

| Rule | Value |
| --- | --- |
| Fixes after the next major | 24 months |
| Security fixes for the last minor | 12 months |
| Supported minors | Two minors stay supported. |
| Removal notice | 12 months of notice |
| Removal migration | An automated migration is required. |
| Downgrade | No downgrade after the first write under a new format. |

The 1.x line receives fixes for 24 months after the next major. The last minor
receives security fixes for 12 months. Two minors stay supported.

The policy is published before the tag. The crate constant
`SupportPolicy::PUBLISHED_IN` names this document.

## 6. Deprecation and removal policy

A format is removed only at a major release. The removal needs 12 months of
notice and an automated migration. A minor release cannot remove a format.

The removal check returns one of these results:

| Condition | Result |
| --- | --- |
| Removal outside a major | `RemovalNotAtMajor` |
| Notice shorter than 12 months | `NoticeTooShort` |
| No automated migration | `MigrationRequired` |
| Major, 12 months of notice, automated migration | Accepted |

A removed error code stays reserved. A new error code is additive.

## 7. Migration and no-downgrade rule

Migration runs forward on open, under the writer lease, before the first read.
A migration verifies checksums, switches the manifest in one CAS, resumes after
interruption, and never runs backward.

A store cannot downgrade after the first write under a new format. The guard
records the newest format that the store wrote. An open request for an older
format fails with `ops.downgrade_not_allowed`. The rule prevents data loss from
a newer write that the older format cannot read.

Opening a newer format after a write is an upgrade. The guard accepts it.

## 8. Qualification requirement

`30-qualification-suite.md` carries one vector corpus for every core contract
and every annex. The corpus names each register row.

The crate exposes `VectorCorpus::check`. The check fails when a supplied corpus
omits one register row. The test corpus in
`crates/oxkv-freeze/tests/vectors/corpus.json` covers all 17 rows.

## 9. Build gate

The freeze statement gate fails when an essay says "freeze" or "frozen" and
names no register row. The gate also fails when a claim names an unknown row.

The checked-in list at
`crates/oxkv-freeze/tests/vectors/essay-freeze-index.json` records each essay
freeze claim and the rows that the claim names. The test does not read the plan
directory at test time. The index is the durable copy of the claims.

## 10. Changelog

| Date | Change |
| --- | --- |
| 2026-11-12 | Add the 1.0 freeze register with C1 to C5 and A1 to A12. |
| 2026-11-12 | Add the `draft`, `candidate`, and `frozen` states and the transition check. |
| 2026-11-12 | Add the format reader window and the wire read window. |
| 2026-11-12 | Publish the support window, the removal policy, and the no-downgrade rule. |
| 2026-11-12 | Add the corpus coverage check and the freeze statement gate. |
