# oxkv falsifier benchmarks

| Field | Value |
| --- | --- |
| Status | Proposed |
| Date | 2026-11-12 |
| Source | `specs.md` sections 7.8, 7.9, 7.22, 7.24, 9.11, and 13.15 |
| Owner | Architecture review |

## 1. Purpose

This document defines the five falsifiers for the 1.0 release gate. A
falsifier is a measured benchmark with a threshold that can disprove a claim.

Each falsifier names one workload, one definition, one threshold, and one
action. A breach fails the release gate. A skip fails the release gate. A
missing number delays the tag.

The 1.0 specification fixes the shape of each falsifier. It does not fix a
numeric threshold. Every numeric value in this document is a candidate. Mark
a threshold as frozen only after a committed baseline exists.

## 2. Summary

| Falsifier | Measure | Candidate threshold | Action on a breach |
| --- | --- | --- | --- |
| `sync_amplification` | Relative change of sync bytes per decade at a fixed selectivity | Above `0.05` per decade | Add per-scope materialized views at the authority. |
| `policy_cost` | p99 evaluation time divided by the median write time | Above `5` | Co-locate facts or widen the projection. |
| `conflict_rate` | Conflict divided by the terminal write count | Above `0.05` | Revisit the base-version check and the per-key FIFO guarantee. |
| `cold_start` | Scope bytes plus hydration bytes divided by the live scope bytes | Above `1.5` | Add a per-scope snapshot path. |
| `read_p99` | Relative change of read p99 per decade at a fixed scope | Above `0.1` per decade | Find the dataset walk and remove it. |

## 3. Falsifier definitions

### 3.1 Sync amplification

- **Name:** `sync_amplification`.
- **Definition:** The bytes one replica receives per committed batch divided by
  the encoded bytes of the matching rows plus the index deltas.
- **Workload:** Dataset rows `N` = 1e5, 1e6, and 1e7 at fixed selectivity `s` =
  1%, 10%, and 50%.
- **Candidate threshold:** A slope above `0.05` per decade. The slope measures
  the relative change of amplification as `N` grows.
- **Action:** Add per-scope materialized views at the authority. Change items
  13 and 14.

A slope above the candidate value means that sync cost follows the dataset. The
scope is then not the replication unit.

### 3.2 Policy cost per write

- **Name:** `policy_cost`.
- **Definition:** The p99 evaluation time and steps per write at a pinned
  policy version. The definition includes the RPC count for a single-prefix
  write.
- **Workload:** Rule count `r` = 5 to 200. Fact count `f` = 1e2 to 1e5. Run
  both a cold and a warm policy.
- **Candidate threshold:** A p99 policy cost above `5` times the median write
  time at `r` = 50 and `f` = 1e4.
- **Action:** Co-locate facts or widen the projection. Do not use 2PC.

The companion limits are candidates too. Step exhaustion above `0.1%` fails.
An RPC count above `1` per write fails.

### 3.3 Conflict rate

- **Name:** `conflict_rate`.
- **Definition:** The conflict count divided by the sum of committed, rejected,
  and conflict outcomes. Queued writes are excluded. A retry counts once.
- **Workload:** Client count `C` = 8, 64, and 256 on a Zipf distribution and on
  a scrubbed replay trace.
- **Candidate threshold:** A conflict rate above `0.05` on keys declared cold.
- **Action:** Revisit the base-version check and the per-key FIFO guarantee.
  Publish the retry contract.

A lost update fails at any rate. The companion limit is a retries-to-terminal
p99 above `3`.

### 3.4 Cold start

- **Name:** `cold_start`.
- **Definition:** The seconds and bytes from an empty replica to a readable and
  searchable scope at a pinned version. Hydration and diff attach are included.
- **Workload:** Scope size from 10 MB to 1 GB. Run on native and on wasm.
- **Candidate threshold:** Scope bytes plus hydration bytes above `1.5` times
  the live scope bytes.
- **Action:** Add a per-scope snapshot path before the freeze.

The companion limits are candidates too. A cold start above `60` seconds for a
100 MB scope fails. A wasm-to-native ratio above `3` fails.

The measurement uses the declared device budget. It does not use the actual
host memory.

### 3.5 Read p99

- **Name:** `read_p99`.
- **Definition:** The replica latency at p50, p99, and p999 for a point read,
  one scope page, and one search page.
- **Workload:** Dataset rows `N` vary by 10x at a fixed scope. Run both warm and
  cold.
- **Candidate threshold:** A p99 slope above `0.1` per decade.
- **Action:** Find the dataset walk in the plan or the index. Remove it.

The companion limits are candidates too. A p99 above two times the budget
fails. A p999-to-p50 ratio above `20` fails.

The search path uses document-local scoring. Ranking cost counts only
document-local features. The ranking cost never uses a collection-wide
statistic.

## 4. Measurement record

One record holds these fields:

| Field | Meaning |
| --- | --- |
| `slope` | The primary measured value. It is a slope for a slope falsifier and a ratio for a ratio falsifier. |
| `interval` | The closed confidence interval. |
| `verdict` | `pass` or `breach`. |
| `hardware_class` | `reference`, `ci`, `developer`, or `browser`. |
| `toolchain_version` | The Rust toolchain version. |
| `commit` | The source commit. |
| `sample_count` | The number of samples. |
| `threshold` | The candidate threshold and its unit. |

The record stores integer millionths. It stores no floating-point value. The
text form uses six decimal places. One stored value therefore has one frozen
text form.

## 5. Release gate

The gate applies these rules in order:

1. A breached threshold fails the gate.
2. A skipped falsifier fails the gate.
3. A missing falsifier number delays the tag.
4. A full pass allows the tag.

A pass entry names the measured value and the threshold. A breach entry names
the measured value, the threshold, and the action.

The gate does not infer a pass from an absent number.

## 6. Result mode

An exact result requires every targeted shard to serve the pin. A coverage
result carries a coverage descriptor. The descriptor lists the answered
shards, the missing shards, and the served versions.

A report marks a coverage result with `result_mode: coverage` and
`coverage: marked`. The report marks an exact result with `result_mode: exact`
and `coverage: none`.

## 7. Determinism

The workload model uses a fixed seed and a logical clock. It reads no wall
clock. It draws no operating-system entropy. It has no network, no I/O, and no
floating point on the semantic path.

One spec produces one spec hash. One spec produces one operation hash. A replay
on native and a replay on wasm32 produce the same operation hash.

## 8. Crate

The contract lives in `crates/oxkv-falsify`.

- `falsifier` defines the five falsifiers and the candidate thresholds.
- `workload` defines the spec, the logical clock, the fixed seed, and the
  replay.
- `measure` defines the record and the verdict.
- `gate` defines the release gate.
- `report` defines the deterministic report writer.
- `budget` defines the scope budget, the ranking cost, and the coverage record.

Run the tests with `cargo test -p oxkv-falsify`. Run the benchmark targets with
`cargo bench -p oxkv-falsify`. Run the plain report with
`cargo run -p oxkv-falsify --example falsify_runner`.
