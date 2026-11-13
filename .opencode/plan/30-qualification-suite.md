# 30. Qualification suite

The qualification suite is the proof that 1.0 is a product, not a version number. It joins a matrix that carries one story across native, browser, and native again, an append-only corpus of golden vectors for C1-C5 and A1-A12, and a differential report across targets and versions. It is the regression net, the ABI proof, and the customer demo. Passing means the corpus is unchanged and green.

## What the greater goal demands

A query is the unit of ownership. A client declares a policy-filtered scope. Matching data materializes locally, stays searchable offline, and syncs incrementally. The promise holds only if one scope, one policy, and one byte stream produce one behaviour on a server and in a browser. That claim needs a test on both sides.

C1-C5 and A1-A12 freeze at 1.0. A freeze of that register is credible only when an outside party can check it without reading internals.

Three audiences pull the suite apart. Maintainers need a fast regression net. Integrators need proof that bytes and semantics survive a target change and a format upgrade. Customers need a legible story: disconnect, keep reading, reconnect, resume. Scenarios stay legible. Vectors stay exhaustive. One report joins both, and the demo reads the trace the gate asserts.

## Required capabilities

- One matrix. A scenario runs on native, browser, and native again. No target gets its own expectation.
- Fault injection at named points: link cut, cursor expiry, conditional-write loss, corrupt page, truncated frame.
- Deterministic control: logical clock, fixed seed, canonical ordering, normalized traces.
- An append-only golden corpus for every core contract C1-C5 and every annex A1-A12, with a differential comparator across targets and versions.
- A report, plus a narration from the same trace.
- A corpus of old manifests, snapshots, pages, frames, and receipts.
- A fuzz harness for every decoder and the policy evaluator.

| Id | Scenario | Injection | Oracle |
| --- | --- | --- | --- |
| S1 | Native to browser to native | Export, load, export, load | Identical bytes, equal traces |
| S2 | Disconnect and resume | Cut link after frame N | Resume, no gap, bounded duplicates |
| S3 | Conflict | Two writers use base version v | `Conflict`, current version, no overwrite |
| S4 | Rule change | Pin v1, stage v2, roll back | Pinned check, one audit record |
| S5 | Snapshot restore | Fresh prefix, one corrupt page | Named checksum error, clean restore |
| S6 | Format upgrade | Open a previous-version artifact | Recorded values, no defaults |

Every row runs on all three targets. S6 also uses checked-in artifacts, because a self-written old file proves nothing. In S2 a cursor can expire, and the replica must fall back to a snapshot.

## Invariants that must hold

- A scenario definition runs on every target without a target-specific branch.
- A released vector never changes. CI rejects an edit.
- One logical input produces one normalized trace on every target.
- A receipt has exactly one terminal state. An unknown state fails decode.
- Read scope and write validation return one verdict for one expression and fact set.
- A resume from a valid cursor loses no committed change. Duplicates stay within the at-least-once bound.
- A snapshot restore never exposes a partial page or a torn diff handoff.
- An older artifact opens and reads its recorded values. A newer artifact fails with `format.unsupported_version`.
- Policy evaluation terminates within its budget. Budget exhaustion is a named outcome.
- No oracle depends on wall-clock time, memory address, or map iteration order.
- A failing target fails the suite. A skip needs a named reason in the report.

**Passing a contract.** A contract passes when four conditions hold. The corpus covers every C1-C5 and A1-A12 surface. Every vector matches exactly on every target. The corpus is unchanged since release. No vector is skipped. A red result is a failure of the implementation or of the contract version. Never edit a released vector. Changed behaviour needs a new contract version and a migration note.

## Decisions and technical choices

**One portable harness versus one per target.** A separate JavaScript harness is faster and uses platform idioms, but creates two definitions of passing. One harness compiled to every target forces the vocabulary and trace format to be portable. The cost is discipline: no threads, no clock, no filesystem. I recommend one harness with thin runners.

**Byte oracles versus semantic oracles.** Semantic assertions survive legal variation but cannot see a format change, which C2, C3, and C5 freeze. Byte oracles catch drift but punish harmless variation without canonicalization. I recommend matching the oracle to the contract. Use raw bytes for ShardSnapshot, page, wire, and receipts. Use canonical JSON for the manifest. Use verdict tables for policy.

**Append-only corpus versus regenerated snapshots.** A bless command makes new tests cheap. It also lets a bug become the oracle, with a green build as evidence. Append-only makes every contract change a visible diff. I recommend append-only, and CI must reject an edit to a released vector.

**In-repo suite versus external service.** An external service is independent, but it is a second system, and a customer cannot run it offline. I recommend an in-repo suite published as a command and library, so one invocation serves both.

Unit tests answer whether a function matches intent. Scenarios answer whether composition tells the right story. Differential tests answer whether targets agree. None replaces another. Differential alone is weakest: two targets can agree on the wrong bytes.

Fuzzing targets decoders and the policy evaluator. Properties: no panic, bounded time and memory, deterministic output, target parity. The corpus starts from the vectors, and every crash becomes one. It runs nightly. A finding can disprove a contract, not prove one.

The CI matrix multiplies targets, features, backends, and artifact versions. The browser tier is the expensive row: it needs a container, a driver, and a package step. Tiering controls cost. A pull request runs native vectors and one browser target. Nightly adds the full browser matrix, old artifacts, and fuzzing. A release candidate runs the full cross product, with 15-minute pull-request and 90-minute nightly budgets. The browser tier must gate the release. The pull request tier may run one browser target, and the nightly tier must run the full browser matrix.

## Dependencies

- Items 5 through 8: manifest, key encoding, snapshot and page format, policy language.
- Items 9 through 12: versioned writes, receipts, outbox, audit.
- Items 13 through 17: scope, wire core, replica, hydration, projection.
- Items 18 through 23: search, fencing, shard map, staleness.
- Items 24 through 29: events, RPC, rule lifecycle, restore, fuzzing.
- Toolchain: wasm-pack, Node, headless browser, logical clock, fault-injection storage.

The suite begins in wave 0, not wave 7. Each wave adds its scenario and vectors. Wave 7 freezes C1-C5 and A1-A12 and publishes what earlier waves proved.

## Acceptance criteria

- A native to browser to native round trip produces identical export bytes.
- CI rejects an edit to a released vector.
- Each core contract C1-C5 and each annex A1-A12 has a versioned vector corpus.
- An older artifact opens and reads its recorded values.
- A newer artifact fails with `format.unsupported_version`.
- A conflicting write ends `Conflict` with the current `key_version`, and no overwrite occurs.
- A rule change is checked against the pinned `policy_version` and writes one audit record.
- A disconnect at any injected frame resumes without a full resync.
- A customer runs the suite offline in one command.

## What would falsify this design

A per-target branch would falsify the claim that one core serves every target. A different but valid byte stream for one input would falsify C1-C5 and A1-A12, or expose incomplete canonicalization.

Two targets that agree while a vector fails would show that differential testing is not enough. A suite that passes both targets and every vector can still miss a shared bug. Corpus coverage is the limit.

A corpus that needs frequent edits during 1.x would show that the contracts were underspecified at 1.0. An old artifact that needs an old toolchain would show that the format is not self-describing.

A passing scenario with a timing tolerance would show that the oracle is nondeterministic. A restore that passes while exposing partial state would show that the oracle is too weak.

A CI cost that forces the browser tier out of the release gate would leave the ABI claim unproven.

## Open questions

1. Who approves a vector corpus for release: a contract owner or a test owner?
2. Does the browser tier gate 1.0, or is one wasm target sufficient?
3. How many released artifact versions stay in the corpus, and for how long?
4. What canonicalization rules apply to JSON numbers, floats, and map ordering?
5. Is a platform difference ever legitimate, and how is it classified?
6. Is `format.unsupported_version` part of C5 (receipt and error semantics), and what retry guidance does it carry?
