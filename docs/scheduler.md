# Write scheduler

The write scheduler chooses the next batch of write requests for the commit
path. It is a pure core. It does not commit data, write a receipt state, read a
clock, draw randomness, or perform I/O.

## Request states

A request has exactly one of three states:

- `queued`: the request waits for its key and its explicit dependencies.
- `in_flight`: the commit path holds the request.
- `terminal`: the commit path resolved the request.

A key has at most one request in flight. A key may have many queued requests.
Per-key FIFO orders all non-terminal requests. Commit order on one key equals
arrival order after terminal removals.

The scheduler writes only its own request state. The commit path owns every
terminal receipt state. The scheduler `queued` state is not the receipt
`Queued` state. A scheduler transition writes no receipt.

## Admission

Admission appends one request to a shard arrival sequence. Each edge must name
an earlier request in the same shard. The scheduler never infers an edge.

Admission rejects four edge faults:

| Fault | Error code |
| --- | --- |
| Cycle, including a self-edge | `outbox.dependency_cycle` |
| Forward reference | `outbox.forward_reference` |
| Cross-shard edge | `outbox.cross_shard_edge` |
| Fan-in overflow | `outbox.fan_in_exceeded` |

Admission appends a request after every existing request. A self-edge is
therefore the only cycle that a caller can declare. The cycle check also walks
the existing edges.

A duplicate request identity uses `outbox.duplicate_request`. An exhausted shard
arrival sequence uses `internal.resource_exhausted`.

The scheduler collapses duplicate dependency edges. It stores edges in arrival
order. It does not reorder requests.

## Selection and budget

One call inspects at most `B` ready positions and `D` dependency edges. The
call returns a partial decision when a limit stops the scan. The decision sets
`more_work` when a ready request remains.

The scheduler walks shards in a stable rotation. It reserves one selection per
shard per round. A shard with more ready work receives the rest in later
rounds.

A blocked shard consumes no position. A blocked key stays absent from the ready
set. Neither fault delays a ready request on another key or shard.

Every non-terminal request appears in exactly one place. A selected request
appears in `selections`. Every other non-terminal request appears in `reasons`.
The reason precedence is:

1. `in_flight`
2. `shard_blocked`
3. `key_blocked`
4. `dependency_blocked`
5. `outside_budget`

The budget bounds the selection scan. The explain output has one row for each
non-terminal request, so its size grows with the pending set.

## Starvation bound

For a finite ready set, an adequate edge budget, and no new arrivals, a ready
request is selected within `ceil(ready_count / B)` calls. Use
`starvation_bound` to compute the bound. Use `SchedulerBudget::is_adequate_for`
to test the edge budget.

## Determinism and replay

The state uses ordered maps and ordered sets. Every tie-break uses an arrival
number or a stable identity. Equal state and equal budget produce an equal
decision. `SchedulerState::digest` covers the configuration, the epoch, the
shard flags, the arrival counters, and every request record.

A replay rebuilds the batch sequence from the admission order, the block
changes, the budget, and the retire order. The log records the state digest,
the budget, the batch, and the reasons.

## Use by the outbox and versioned writes

Essay 10 (outbox) supplies queued intents and explicit edges. It maps a
selected request identity back to its outbox entry. It applies the decision
before submission and retires the request after the authority verdict.

Essay 09 (versioned writes) supplies base versions and the commit verdict. The
scheduler orders work. It does not compare versions and does not evaluate
policy.

The caller must supply the canonical key bytes and the shard from the static
shard map. The scheduler does not decode a key and does not guess a shard.
