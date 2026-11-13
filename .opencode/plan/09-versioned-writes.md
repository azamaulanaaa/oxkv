# 9. Versioned writes

A client cannot write. The authority must decide whether the record still looks
the way the client believed, so a write carries a value, an idempotency key, and a
**base version**: the state the client read before the change. Without that base,
the system is last-write-wins with extra steps, and the loser never learns.
Versioned writes make the base mandatory. A mismatch becomes a terminal
`Conflict`. A retry stays safe under duplication. The recommendation is optimistic
per-key versions with exact-match compare-and-set at commit.

## What the greater goal demands

The authority owns truth and serializes every write. A replica is read-only: it
holds a scope, an index, and a durable outbox. Every guarantee about concurrent
intent must come from the request.

A client may be offline for a day and submit later. No reservation may precede the
edit, and the client cannot know about other pending writes.

Policy has no clock, randomness, or network, and conflict resolution must obey the
same discipline. Wall-clock time must not decide outcomes. The base must be
derivable from what the replica materialized, so a client names only a version it
has seen.

## Required capabilities

1. A request must carry `idempotency_key` and, per mutated key, `base_version`.
   Without either field it is malformed.
2. The authority must assign versions. A client must not construct or guess one.
3. The shard must compare-and-set the per-key version atomically with the data
   write and the receipt.
4. One durable receipt per idempotency key must live in that transaction.
5. A receipt must name one terminal state: `Committed`, `Rejected`, or
   `Conflict`. `Queued` is the only non-terminal state.
6. A `Conflict` must carry the current version and enough policy-filtered state
   to re-read.
7. The outbox must expose conflict as terminal. Rebase must re-read, take the new
   base, and issue a new key.
8. A cross-shard mutation must fail with a named error before any version check.
   Generated RPC must require both request fields.

## Invariants that must hold

- **The base is mandatory.** No path commits without comparing the named `base_version` to
  the current `key_version` of that key.
- **Exact match, never a range.** A base ahead of the current `key_version` is never
  accepted, so a client cannot pin the future. The authority never applies a
  request at another `commit_version` and never merges.
- **Conflict is terminal.** One idempotency key produces at most one terminal
  receipt, and it never later becomes `Committed`.
- **At most one application.** One key and one body apply at most once. The same
  key with a different body is a protocol error, and nothing applies.
- **Monotonic per key.** A commit at base `key_version` V produces a `key_version` greater than V.
  A delete consumes a `key_version` and leaves a tombstone, so delete and re-create
  cannot alias an old `key_version`.
- **One transaction.** The `key_version` check, policy check, data write, `commit_version`
  assignment, receipt, audit record, and business events commit together.

## Decisions and technical choices

### Which version space a request names

- **Shard `commit_version`** is too coarse: any write in the shard would conflict.
- **Scope version vector** is wrong: it moves when a query result moves, not when one key
  changes, so two scopes name incomparable versions for one record.
- **`key_version`**, the document version, is precise: one record, one comparison.
- **Batch `commit_version`** serves multi-record requests but is not sufficient alone,
  because a shard commit serializes the whole shard.

A request names one `base_version` per mutated key. A receipt names the resulting
shard `commit_version`: the first is the precondition, the second the observation.
Conflating them is the likeliest error, because both are called a version. A
multi-record request in one shard carries `(key, base_version)` pairs. If any pair
mismatches, the whole request is `Conflict` and no key changes, because partial
application would leave a receipt the client cannot reason about.

### Why two offline renames defeat any dependency graph

Two clients hold one record at version 7. Both go offline. A sets `alpha`, B sets
`beta`, and neither sees the other.

A dependency graph records edges between operations that know each other. It
cannot hold an edge between A and B, because neither request can name the other.
Both name version 7. The graph is a fork with two children and no edge between
them, and that shape is what a graph should represent, so it reports nothing
wrong.

The conflict is not a cycle or a missing edge. It is mutual exclusion: at most one
child of 7 may become 8. Mutual exclusion is not a graph property but a property
of a serialization point. Only the authority, holding both requests and the
current version, can.

Detection by shipping dependency metadata between clients therefore cannot work
offline. It would require B to know A before submitting, which is the
connectivity local-first removes. Detection belongs at commit. Scheduler edges
order writes the authority knows, but they do not replace the `key_version` check.

### Conflict, rejection, and retry

A `Conflict` is not an error: the request was well formed and the state moved. A
`Rejected` says policy refused the body. The `key_version` check must run first: a
policy verdict on a body that would overwrite a record the client never saw is
meaningless, and the right next step is to re-read, not to revise the body.

Retry is safe because the idempotency key, not the attempt, identifies the write.
After a timeout the client resends the same key and base. If the first attempt
committed, the stored receipt returns. If not, the compare-and-set runs.

A conflicted request must not be replayed with the same key at a new base,
because that would let the authority decide intent. Rebase issues a new key, and
the old conflict stays visible as history. A duplicate with a different body must
fail closed, or idempotency is false.

### Optimistic versions, locks, or last-write-wins

**Last-write-wins** needs a total order: arrival order, which discards intent
silently, or client time, which is untrusted and non-monotonic. It also gives the
loser `Committed`, which breaks the visible-conflict milestone. Reject it.

**Locks** need a reservation. A client offline for a day cannot hold one, and if
it does, every other writer is blocked. Locks convert contention into
unavailability, and an expired lease mid-edit restores the lost update. Reject
them as the default. Per-key FIFO is not a lock: it orders writes that arrived and
reserves nothing.

**Optimistic per-key versions** need no reservation, work offline, cost one
comparison, and make the loser's outcome explicit. They fail only toward false
conflicts when two clients touch different fields of one record. That cost is
bounded and measurable.

## Dependencies

- **A1 (key encoding)** defines a per-key `key_version` slot and a canonical absence
  value.
- **C1 (manifest schema)** declares versioned-write operations and requires both fields.
- **C4 (policy language)** runs at commit, after the `key_version` check, in one transaction.
- **C5 (receipt and error semantics)** and the outbox carry the durable terminal state. **Scheduler** gives
  per-key FIFO with no conflict detection.
- **Replica**, **C2 (snapshot and page format)**, and **A4 (shard map)** expose the `key_version`,
  preserve versions and tombstones across restore, and name the cross-shard
  rejection.

## Acceptance criteria

- Missing `base_version` fails with `write.missing_base_version` and changes no data.
- Two clients rename one record from one base. One gets `Committed`, one gets
  `Conflict`, and the record holds the winner.
- The loser rebases and commits at the next `commit_version`. Retrying the same key still
  returns `Conflict`.
- Three deliveries of one request produce one `commit_version` and three identical
  receipts. A reused key with a different body fails with `write.idempotency_key_reuse`.
- A base ahead of the current `key_version` fails with `write.stale_base_version` and does not
  advance.
- A create from absence commits, a second conflicts, and delete then create does
  not reuse the `key_version`.
- A two-shard request fails with `shard.cross_mutation_rejected`.
- A stale base with a policy-violating body returns `Conflict`, not `Rejected`.
- A `Rejected` verdict writes a receipt and an audit record, and consumes the key for the same body.
- A terminal receipt names the audit record that justified it.
- No injected fault commits without a matching base.

## What would falsify this design

- A false-conflict rate on hot keys high enough that clients rebase more than they
  work, which would make per-key versions too coarse.
- A common pattern of offline edits to disjoint fields of one record, where users
  expect both to survive.
- Any implementation that accepts a base ahead of the current version, or rebases
  on the server.
- A duplicate delivery that creates a second version, a restore that loses
  tombstones, or a conflict rate that grows with offline duration until long
  offline use is impractical.

## Open questions

1. Per-field versions for wide records at 1.0, or whole-key exact match? Must the
   absence base differ from version zero?
2. How long are idempotency receipts retained? Unbounded retention is exact but
   costly, and a bounded window weakens the guarantee.
3. Does a `Conflict` carry the current value, a hash, or only the `key_version`? A value
   helps the client but may leak fields its scope excludes.
4. Does a client's own commit always enter its scope? If policy filters it out,
   only the receipt shows it.
