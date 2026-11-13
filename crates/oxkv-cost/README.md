# oxkv-cost

The query cost model, explain record, bounded fan-out, and deterministic
scatter-gather merge for oxkv.

The crate is portable. It compiles for native and `wasm32-unknown-unknown`. It
uses integer arithmetic only. It does not read a clock, draw randomness, use a
regular expression, or perform I/O.

## Surfaces

| Module | Purpose |
| --- | --- |
| `plan` | Builds a rule-based query plan and computes its cost. |
| `budget` | Checks the query cost, fan-out, and token limits. |
| `explain` | Renders the `cost/v1` explain record as canonical JSON or text. |
| `merge` | Merges ordered shard streams under one total order. |
| `coverage` | Implements exact and coverage result modes. |
| `cursor` | Adds cost accounting and expiry to `CostCursor`. |

## Cost unit

One shard costs 1000 milliunits. One examined key costs 1 milliunit.

```text
cu_milli = 1000 * shards + keys_examined
```

A term probe counts as one examined key. A query with `t` terms over `s` shards
therefore has at least `s * t` term probes.

## Result modes

Exact mode is the default. Every targeted shard must serve the pin. If one
shard cannot, the call returns a named error and no row.

Coverage mode is opt-in. A coverage page carries a `CoverageDescriptor` with
the answered shards, the missing shards, and the served version of each
answered shard. A coverage page is always partial. It never returns an exact
cursor.

## Merge order

| Query class | Comparator |
| --- | --- |
| Search | `(score descending, canonical key ascending)` |
| General | `(declared sort key ascending, canonical key ascending)` |

The shard identifier is provenance only. It is not a tie-break. Arrival order,
retry order, and shard speed do not change the output.

## Named errors

| Code | Cause |
| --- | --- |
| `query.budget_exceeded` | The query cost is above the budget. |
| `search.fanout_exceeded` | The query fan-out is above the limit. |
| `search.pin_expired` | A shard cannot serve the pin. |
| `search.token_budget_exceeded` | The token count is above the budget. |
| `scope.shard_unreachable` | An exact query has an unavailable shard. |
| `internal.invariant_violation` | A plan or merge invariant failed. |

## Golden vectors

The `tests/vectors` directory pins the explain bytes, the merge order, and the
coverage descriptor. The crate compiles for native and `wasm32`.
