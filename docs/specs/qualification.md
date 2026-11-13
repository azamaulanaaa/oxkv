# oxkv qualification suite

| Field | Value |
| --- | --- |
| Status | Wave 0 |
| Date | 2026-11-12 |
| Scope | Scenario matrix, faults, golden vectors, differential comparator, fuzz, report, and tiers |
| Normative source | `specs.md` sections 6, 7.21, 7.22, 9.10, 9.11, and 13.15 |
| Crate | `crates/oxkv-qualify` |

## 1. Purpose

The qualification suite proves that one logical input produces one result on
every target. It also proves that a released byte stream does not change.

The suite runs offline. It uses no network. It uses no wall clock and no
operating-system randomness. It uses no floating point and no `unsafe`.

## 2. One command

Run the suite offline with one command:

```text
cargo test -p oxkv-qualify
```

Run the same suite and print the report and the narration:

```text
cargo run -p oxkv-qualify -- --tier pull_request
```

The binary starts no service. The default tier is `pull_request`.

## 3. Register

The suite runs a vector for every row in the contract register. The register
file is `crates/oxkv-qualify/register.json`. The code reads the same rows from
`oxkv_qualify::register`.

| Id | Surface | Version | Parent | Vector file |
| --- | --- | --- | --- | --- |
| C1 | Manifest schema | `oxkv/manifest/v1` | none | `C1-manifest.json` |
| C2 | Snapshot and page format | `snapshot-page/v1` | none | `C2-snapshot.json` |
| C3 | Sync wire protocol | `sync-wire/v1` | none | `C3-sync-wire.json` |
| C4 | Policy language | `policy-ast/v1` | none | `C4-policy.json` |
| C5 | Receipt and error semantics | `receipt-error/v1` | none | `C5-receipt-error.json` |
| A1 | Key encoding | `key-encoding/v1` | C2 | `A1-key-encoding.json` |
| A2 | Tokenizer | `tokenizer/v1` | C1 | `A2-tokenizer.json` |
| A3 | Event envelope | `event/v1` | C5 | `A3-event.json` |
| A4 | Shard map | `oxkv/map/v1` | C1 | `A4-shard-map.json` |
| A5 | Scope semantics | `scope/v1` | C3 | `A5-scope.json` |
| A6 | Projection staleness | `projection/v1` | C4 | `A6-projection.json` |
| A7 | Audit envelope | `audit/v1` | C5 | `A7-audit.json` |
| A8 | Cost and explain | `cost/v1` | C1 | `A8-cost.json` |
| A9 | Cursor types | `cursor/v1` | C3 | `A9-cursor.json` |
| A10 | Descriptor | `descriptor/v1` | C1 | `A10-descriptor.json` |
| A11 | Read pin and staleness | `read-pin/v1` | C3 | `A11-read-pin.json` |
| A12 | Migration and backup | `migration/v1` | C2 | `A12-migration.json` |

A new contract adds one row. It adds one vector file. It does not edit a
released vector.

## 4. Matrix

One case definition runs on native, browser-shaped, and native again.

| Id | Scenario | Injection | Oracle |
| --- | --- | --- | --- |
| S1 | Native to browser to native | Export, load, export, load | Identical bytes and equal rows |
| S2 | Disconnect and resume | Cut link after frame N | Resume, no gap, bounded duplicates |
| S3 | Conflict | Two writers use base version 1 | `Conflict`, current key version, no overwrite |
| S4 | Rule change | Pin v1, stage v2, roll back | Pinned verdict and one audit record per change |
| S5 | Snapshot restore | Fresh prefix, one corrupt page | `format.corrupt_page`, clean restore |
| S6 | Format upgrade | Open a previous-version artifact | Recorded values, no defaults, v2 rejected |
| F1 | Crash at commit | Crash at each commit step | One published generation, no partial state |

A scenario has no target-specific branch. The target label enters the report
only. A test asserts that every case has one trace digest on all three targets.

The browser-shaped target runs the same core with the same code path. The
real browser run uses headless Chrome and headless Firefox in CI. See
sections 11 and 12.

## 5. Faults

The suite injects every named fault:

| Fault | Point | Named outcome |
| --- | --- | --- |
| Link cut | After frame N | Resume from `SyncCursor`, no full resync |
| Cursor expiry | After N frames | `sync.cursor_expired`, snapshot fallback |
| Conditional-write loss | Write N | `write.missing_base_version`, no overwrite |
| Corrupt page | Page N | `format.corrupt_page`, no partial state |
| Truncated frame | Frame N | `sync.cursor_invalid`, frame requested again |
| Crash at commit | Each of validate, apply, receipt, audit, index, publish | One intact generation |
| Duplicate delivery | Frame N | At-least-once bound holds, no state change |

The fault plan is a scenario input. The default case list covers every fault
kind. A test asserts that coverage.

## 6. Deterministic control

- The clock is a logical counter with a fixed step. It is not a wall clock.
- The seed is fixed. The default seed is `0x4f584b565155414c`.
- The generator is `SplitMix64`. It draws no operating-system randomness.
- Every map on a trace path is ordered. Every trace field is sorted.
- A trace is normalized before hashing. A line feed, a pipe, or a control byte
  in a value becomes `_`.
- No oracle reads a memory address, a map iteration order, or a wall clock.

## 7. Golden vectors

Each vector file holds cases. A case names a deterministic input and one
expected output. The output is canonical bytes or one named error.

The manifest is `crates/oxkv-qualify/vectors/manifest.json`. It lists the
`BLAKE3` digest of every vector file and marks every entry as released.

The test `every_released_vector_keeps_its_digest` reads the manifest and the
files. It computes each digest. It does not regenerate a vector. The test
`the_released_manifest_is_unchanged` also pins the digest of the manifest.

An edit to a released vector fails the test. A break needs a new contract
version and a new vector. Never edit a released vector.

## 8. Differential comparator

The comparator checks three surfaces:

- Expected bytes or one named error for every vector.
- The same observation on native, browser-shaped, and native again.
- A stable vector across contract versions. A major change must return one
  named error.

The report names every difference. One failing target fails the suite.

## 9. Fuzz

The fuzz entry points cover every decoder and the policy evaluator:

- All eight cursor token types.
- The wire frame decoder.
- The artifact decoder.
- The error-code lookup.
- The table-name parser.
- The receipt tag decoder.
- The index artifact decoder, the segment decoder, and the posting-key decoder.
- The policy parser and evaluator.

The index probes call the real `oxkv-index` decoder on a valid fixture. The
corpus then corrupts the index digest, the segment header, and the posting
list. A corrupt input must return a named error. It must not panic.

The oracle checks five properties:

- Totality. Every input returns a value or a named error.
- Determinism. Two runs return the same result.
- Step monotonicity. A longer `and` chain uses at least as many steps.
- Memory bound. The arena stays inside the budget.
- Native and wasm parity. The corpus has one pinned digest.

The corpus starts from fixed bytes. It runs in the test process. It adds no
network service.

## 10. Report and narration

`Report::from_runs` builds the report from one normalized trace per case.
`Report::narration` builds the narration from the same traces. The report and
the narration carry the same trace digest. A test asserts this equality.

The report lists every case, every target, the result, the trace digest, and
every failure. The narration tells the same story in short sentences.

## 11. Tiers

The tiers are policy data in `crates/oxkv-qualify/policy/tiers.json`.

| Tier | Browser targets | Fuzz | Old artifacts | Gates release |
| --- | --- | --- | --- | --- |
| `pull_request` | One | No | No | No |
| `nightly` | Full matrix | Yes | Yes | No |
| `release_candidate` | Full matrix | Yes | Yes | Yes |

The release candidate includes the full logical browser matrix. The checked-in
CI runs two real headless browser targets in the `wasm-browser` job of
`ci.yml`: Chrome and Firefox. The job is a matrix with one leg per browser.
The release workflow calls `ci.yml`, so a failed Chrome leg or a failed
Firefox leg blocks the tag. The qualification suite itself runs the
browser-shaped target on the native host. The real browser job proves the wasm
binding and OPFS paths on both engines. It does not run the qualification
vectors.

The `pull_request` row names the minimum browser work for the tier. The
checked-in CI runs both browser legs on a pull request.

## 12. Continuous integration

`.github/workflows/qualification.yml` runs three jobs on a pull request and on
a push to `main`:

- `native` runs the native suite, clippy, and the format check.
- `vectors` runs the vector immutability test.
- `freeze` runs the freeze register gate, the receipt registry gate, and the
  register agreement gate.

`.github/workflows/ci.yml` runs the `qualification` job with
`cargo run -p oxkv-qualify --locked -- --tier release_candidate`. It also runs
the `wasm-browser` job as a matrix over headless Chrome and headless Firefox.
The Chrome leg installs `chromium` and `chromium-driver`. The Firefox leg
installs `firefox-esr` and a pinned `geckodriver` release. Each leg runs
`wasm-pack test --headless --chrome` or `wasm-pack test --headless --firefox`
for the core, the OPFS crate, and the wasm bindings. `.github/workflows/release.yml`
calls `ci.yml` and gates both publish jobs on `checks`. The browser tier and
the qualification matrix therefore gate the release.

`.github/scripts/check-release-gates.sh` fails when a release gate is absent
from the workflows. It requires both browser legs, the Firefox and geckodriver
installation, and the qualification matrix.

The workflow uses locked dependencies and pinned action revisions. It starts
no service.

## 13. Growth rules

- The suite starts in wave 0. It grows with every contract.
- A new core contract or annex adds a register row and a vector file.
- A new scenario adds a case. A case keeps one definition for every target.
- A new decoder adds one fuzz entry point and one corpus probe.
- A released vector is immutable.
- A changed behaviour creates a new contract version and a new vector.

## 14. Deviations and open questions

- The browser-shaped target runs the shared core on the native host. The real
  headless browser run belongs to the browser tier. The workflow now runs
  Chrome and Firefox. This branch did not execute the browser workflow
  locally. The workflow change stays unverified until CI runs it.
- The wave-0 corpus pins the registries and the sample decoders. A later
  contract adds behavioural vectors for its full frozen surface.
- Open question: who approves a vector corpus for release?
- Open question: how many released artifact versions stay in the corpus?
- Open question: what canonicalization rule applies to a JSON number?
