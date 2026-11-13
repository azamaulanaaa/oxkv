# 31. Falsifier benchmarks

Five measurements decide whether this design ships. If a policy-filtered query
is the unit of ownership, cost must track the scope and not the dataset. Sync
bytes follow matching rows. Policy cost per write follows rules and projected
facts at one shard. Conflicts stay rare and legible. Cold start costs the
scope. Read p99 stays flat as the dataset grows behind a fixed scope. This
essay defines each measurement, its workload, its failure threshold, and the
action a breach forces.

## What the greater goal demands

The goal is a local-first, policy-governed, query-materialized data layer. A
client declares a policy-filtered query. Matching data materializes locally,
stays searchable offline, and syncs incrementally. Authority owns truth and
serializes writes. Replica is read-only with a scope, an index, and an outbox.

The suite must be adversarial. Set selectivity low, dataset size high,
concurrency high, and policy large, because the claim breaks there. A
benchmark that cannot fail is a demo.

The suite must be a gate. Item 31 precedes item 32 so a failing slope can
still change a contract before 1.0. C1-C5 and A1-A12 are the frozen contracts.

A macro result without a micro explanation is a number, not evidence.

## Required capabilities

**One workload spec.** A declarative file states the dataset generator,
scope predicate, policy version, shard map, concurrency schedule, trace, axes,
and metrics. Its hash keys every baseline, and all tiers consume it.

**Synthetic generators.** Controlled axes: dataset rows `N`, selectivity `s`,
rules `r`, facts `f`, and clients `C`. Generate skew, because uniform keys
hide conflict.

**Replay.** A scrubbed trace replayed through the same driver. It supplies
shape: bursts, hot keys, retry storms. It pins one point, so it cannot
establish a slope.

**Three drivers.** Micro measures one stage. Macro runs a real in-process
cluster over generated RPC, with real shards, replicas, and wire encoding.
Replay feeds macro. Only macro may pass or fail a falsifier.

**Accounting.** The harness counts bytes per replica per commit, policy steps
and time, receipt transitions, hydration bytes, retries, and latency
histograms. A number unattributable to one write is not acceptable.

**Determinism.** Shard map, scope, policy version, and write order derive from
a seed. Timing varies. The operation sequence must not.

**Reporting.** Each run writes a machine-readable report with the slope, its
interval, the verdict, hardware class, rustc version, and git sha.

## Invariants that must hold

Each metric has one definition, one workload, and one trouble threshold.

| Metric | Definition | Workload | Threshold |
| --- | --- | --- | --- |
| Sync amplification `A` | Bytes one replica receives per committed batch, divided by encoded bytes of matching rows plus index deltas. | `N` = 1e5, 1e6, 1e7 at fixed `s` = 1%, 10%, 50% | `A` rises with `N` at fixed `s` (slope > 0.05 per decade), or `A` > 4 at `s` = 1% |
| Policy cost `P` | p99 evaluation time and steps per write at a pinned policy version, plus RPCs per single-prefix write. | `r` = 5 to 200, `f` = 1e2 to 1e5, cold and warm policy | p99 `P` > 5x median write at `r` = 50, `f` = 1e4, steps exhausted > 0.1%, or RPCs > 1 |
| Conflict rate `K` | Conflict / (Committed + Rejected + Conflict). Queued is excluded. A retry counts once. | `C` = 8, 64, 256 on Zipf and replay | `K` > 5% on keys declared cold, retries-to-terminal p99 > 3, or any lost update |
| Cold start `S` | Seconds and bytes from an empty replica to a readable, searchable scope at a pinned scope version vector, including hydration and diff attach. | Scope 10 MB to 1 GB, native and wasm | `S` bytes > 1.5x live scope bytes, `S` > 60 s for 100 MB, or wasm/native > 3x |
| Read p99 `L` | Replica latency at p50, p99, p999 for a point read, one scope page, and one search page. | `N` varies 10x at fixed scope, warm and cold | p99 rises with `N` at fixed scope (slope > 0.1 per decade), p99 > 2x budget, or p999/p50 > 20 |

Four invariants are absolute. A lost update fails at any rate. A cross-shard
mutation fails unless it returns `shard.cross_mutation_rejected`. A receipt reaches one
terminal state. An idempotent replay must not duplicate an effect.

The `L` workload must include a coverage-mode page with a coverage descriptor. Exact mode is the default, and a coverage page must not advance an exact pagination cursor. Ranking must use document-local features only, so a global corpus statistic cannot explain a slope. The scope byte measurement must compare live scope bytes and hydration peak against the declared device budget. The SLO report from item 23 supplies the availability and staleness targets.

A threshold sits where the claim stops being true, not where hardware sits. A
4x amplification still supports "sync tracks the scope". A 10x does not.

## Decisions and technical choices

**Micro tier, in-tree.** Cheap, low variance, good for attribution and
pull-request gating. It cannot see fan-out, hydration, or wire cost, and
assumes stationary load while sync is bursty. Tier 1 only.

**External multi-process driver.** Highest fidelity: real sockets and fsync
contention. Noisy, hardware-bound, and not reproducible off the release
machine. Nightly confirmation only.

**Simulation.** Deterministic and cheap at `N` = 1e9. It validates the model,
not the software. Use it only beyond the largest measured `N`.

**Recommendation.** Three tiers. Tier 1 micro. Tier 2 in-process cluster over
generated RPC, which owns the verdict. Tier 3 replay. The transports must
agree within 5% on point reads, a falsifier of the transport abstraction.

**Synthetic versus replay.** Synthetic owns slopes, because only it sweeps an
axis. Replay owns `K` and `L` at the gate, because only it carries realistic
skew.

**Absolute versus relative.** Falsifiers use absolute thresholds. Regressions
use 10% against baseline, confirmed in two of three runs. Absolute values
drift with hardware. Slopes do not.

## Dependencies

- Items 5 to 7 supply the key, snapshot, and manifest formats that specs and
  baselines use.
- Items 9 to 11 supply versioned writes, receipts, and the scheduler that `K`
  and `P` measure.
- Items 13 to 17 supply scope, the wire protocol, the replica, hydration, and
  projection. Tier 2 cannot start before 17.
- Items 18 to 20 supply the index and cost model for `L`.
- Items 22, 23, and 28 supply the shard map, bounded-staleness reads, and
  metrics.
- Item 30 supplies the scenario matrix and golden vectors that tier 2 reuses.

Tier 1 starts earlier: policy cost after item 8, fan-out after 13, hydration
after 16.

## Acceptance criteria

1. One spec runs all five falsifier families across tiers 1 to 3.
2. Each run writes a report with the slope, its interval, and a verdict per
   metric.
3. The gate fails on a crossed slope in two of three runs, or a non-zero
   correctness counter.
4. Each tier 2 result names the tier 1 group that explains it, or it is
   invalid.
5. The in-process and TCP transports agree within 5% on point-read p99.
6. A committed baseline is required for the 1.0 spec set, with hardware and
   git sha.
7. The wasm target reports the same operation hash as native.
8. Tier 1 finishes within 30 minutes in CI. Tier 3 finishes within 4 hours
   nightly.
9. The report declares a breach, the metric, and the action. A silent miss
   fails the gate.

## What would falsify this design

`A` rising with `N` at fixed `s` is the decisive failure. Sync is then
dataset-proportional and the scope is not the replication unit. The response
is per-scope materialized views at the authority, which changes items 13 and
14. If that cost is unacceptable, the thesis narrows to "a scope reduces sync"
and must stop claiming a cap.

RPCs per write greater than one falsifies single-shard validation. Either
facts are not projected, or validation needs a second shard. The fix is
co-location or wider projection, not 2PC.

A high `K` on cold keys falsifies the conflict model. Revisit the base-version
check and the per-key FIFO guarantee, and publish the retry contract.

`S` proportional to `N` falsifies hydration. The scope is not the ScopeSnapshot
unit. The response is a per-scope ScopeSnapshot path before 1.0.

`L` rising with `N` at fixed scope falsifies the read path. Something walks the
dataset. Find it in the plan or the index and remove it.

A suite that stays green under adversarial axes falsifies the suite. The
release stops until the harness can fail.

## Open questions

- What is "scope size"? Rows, bytes, or index terms. Use bytes for `S`, rows
  for `A`, and pick one before 1.0.
- Is `K` a property of the authority or of the authority plus a pinned retry
  policy? Pinning measures the pair. Freedom makes runs incomparable.
- Can a replay trace be shipped? Privacy and size may force a generator plus a
  hash, weakening tier 3.
- Which absolute thresholds survive a hardware change, and which become
  ratios?
- Does wasm read p99 include browser scheduling? If so, the threshold differs
  per target.
- At what calibration error does simulation stop being admissible evidence for
  `N` = 1e8?
- Who owns the baseline machine, and what happens when it is down?
