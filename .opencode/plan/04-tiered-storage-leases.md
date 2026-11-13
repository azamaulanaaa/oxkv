# 4. Tierable storage with leases

**Thesis.** "Data does not need to move fast, it only needs to be there" is not a cache slogan. It is a statement about where a byte may live and who may change it. A shard must be one logical store that spans RAM, local disk, and object storage. A placement policy must decide the tier of every segment. A single-writer lease with a fencing epoch must decide which process may commit. The goal needs both, because placement, ownership, and replication share one unit: the shard.

## What the greater goal demands

- The shard is the unit of placement. A decision applies to a data shard, which contains one user table and its system sub-namespaces.
- The authority owns truth. Object storage holds bytes, and the shard holds the authority lease record. Neither may return a `commit_version` that the shard did not assign.
- The replica is read-only. It holds no authority lease, and its tiering stays inside its scope.
- The business event stream is the only path to write side effects. Tier changes emit operational signals on the operational feed.
- C2 (snapshot and page format) must carry `page_id`, `page_sequence`, provenance, and a content seal. A sealed ShardSnapshot stays valid across an authority lease change.
- The authority lease is the local form of leadership that the replication seam must generalize. A local tab lease is separate and never fences a write. A seam needs a second in-tree implementation, and the protocol core stays sans-IO.

## Required capabilities

1. **A three-tier ladder inside one shard.** RAM is a page cache. Local disk holds a write-ahead region, immutable segments, and index pages. Object storage holds immutable, checksummed segment objects. Each lower tier is authoritative for the bytes that it holds.
2. **A placement policy as versioned data.** The manifest declares rules that map a segment to a tier and a durability class. The inputs are `shard_id`, `commit_version`, size class, access count bucket, and pin state. The policy reads no clock and no randomness.
3. **Read-through.** A read that misses a tier promotes the needed page from the next colder tier. Concurrent misses share one fetch.
4. **Bounded warm-up.** A read promotes no more than the declared budget. A read that needs more returns a `KeyCursor` and a named status. It must not return silently truncated data.
5. **A single-writer authority lease with a `lease_term`.** A writer must hold a live authority lease to commit. Every commit carries the `lease_term`, and the storage boundary must reject any `lease_term` below the accepted maximum.
6. **A durable authority lease record and ordered recovery.** The record holds the shard, holder, `lease_term`, lease duration, and last `commit_version`. A new writer must write a strictly higher `lease_term`. A restarted process must re-acquire and replay to the last `commit_version` before it serves a write.

## Invariants that must hold

**I1. One `lease_term`, one writer.** At most one process commits for a shard in one `lease_term`. An acknowledged `lease_term` is a floor. The storage boundary must reject a stale `lease_term`, even when the stale process is alive.

**I2. No uncommitted visibility.** A read must never observe bytes from a write without a `commit_version`.

**I3. Byte identity across tiers.** For one `commit_version` and one page, every tier must return identical bytes or a checksum error. Tiering must not change a result, a `KeyCursor`, or an error.

**I4. No only-copy eviction.** An eviction must not remove the last verified copy of a committed `commit_version`. A segment stays pinned until a colder tier confirms a checksum.

**I5. Deterministic placement.** Replay of the same metadata must produce the same plan. A plan must not depend on time, process identity, or arrival order.

**I6. Bounded promotion.** One read must not promote more than its budget, under concurrent readers and under retry.

**I7. Replica silence.** A replica must hold no authority lease and must issue no commit. Its tiering must not promote data outside its scope. A local tab lease may exist for multi-tab coordination only.

**I8. Policy isolation.** Placement must not call the policy evaluator, and the evaluator must not read placement state.

**I9. Durable acknowledgement.** A writer must not acknowledge a commit until the log record and the authority lease fence are durable.

## Decisions and technical choices

**D1. Safety comes from the `lease_term`, liveness comes from the clock.** Clock skew may stop a writer early or delay a handover. It must never allow two commits in one interval. The storage boundary compares `lease_term` values, not times. The skew assumption bounds availability, not correctness.

**D2. The authority lease record is a compare-and-set object in the shard system sub-namespace.** A candidate reads the record and writes a higher `lease_term` with a conditional write. The backend serializes it. A loser returns `replication.lease_held`.

**D3. Version distance is measured in versions, not seconds.** A demotion rule matches the distance between the segment `commit_version` and the shard `commit_version`. A background pass may propose demotion, but the decision must replay from the metadata log.

**D4. Fencing sits at the storage boundary, not the RPC boundary.** A paused process can pass an RPC guard and resume after a handover. The commit path must carry the `lease_term` to the tier that writes bytes.

**D5. A paused writer is a first-class case.** A stopped process that resumes after a handover must fail with `replication.stale_epoch`, and the shard must keep one history.

**D6. Restart is a re-acquisition, never a resumption.** On start, a process must read the fence, acquire a higher `lease_term`, replay from the last checkpoint, and only then serve a write. Without the fence it must stay read-only.

**D7. Two drivers prove the tier seam.** A filesystem driver and an object-store driver must implement one interface. A memory driver serves wasm and tests. A driver that cannot hold the only copy must declare that limit.

**D8. Errors are named and classified.**

- `replication.lease_expired`: the holder lost the authority lease. Retry after re-acquisition.
- `replication.lease_held`: another writer holds the authority lease. Retry with a hint.
- `replication.stale_epoch`: the commit is below the fence. Terminal for that attempt.
- `replication.only_copy_eviction`: the plan would remove the last copy. Terminal. An operator must act.
- `replication.tier_unavailable`: a colder tier did not answer. A read may retry. A write must not acknowledge.
- `replication.warmup_budget_exceeded`: the read needs another page. Continue with the `KeyCursor`.
- `replication.placement_violation`: the plan contradicts the policy. Terminal.

**D9. Recovery is ordered.** Read the fence. Discard log records above the last `commit_version`. Replay and verify checksums. Acquire a higher `lease_term`. Then serve writes.

## Dependencies

- A1 (key encoding) and C2 (snapshot and page format), so a shard has one identity and a page carries `page_id`, `page_sequence`, and `commit_version` across tiers.
- The manifest, so placement is versioned with the tables that it governs.
- Write semantics with receipts, so `replication.stale_epoch` and `replication.lease_expired` join one taxonomy.
- Scope and the replication seam, so replica tiering stays inside the query and the seam consumes the authority lease as its local form.
- The ordering rule applies: wave 1 ships one storage backend. Tiering beyond one backend waits for the formats to version.

## Acceptance criteria

1. A read after a restart with an empty RAM tier and a cold disk matches the pre-restart result byte for byte.
2. A forced kill during a commit leaves either a committed `commit_version` or no version. Recovery reports one outcome.
3. A paused writer, resumed after a handover, receives `replication.stale_epoch`, and the history holds one commit.
4. Two simulated writers with clocks skewed by more than the authority lease duration cannot both commit in one interval.
5. A crash immediately after a commit acknowledgement preserves the commit.
6. Promotion from object storage respects the byte budget under 32 concurrent readers.
7. An eviction plan that would remove the only copy returns `replication.only_copy_eviction` and changes no tier.
8. A placement plan replays identically on two hosts.
9. A replica answers a scoped read offline with empty local tiers, matching the authority at the pinned `commit_version`.
10. The wasm build reads from the memory driver and declares tiering unavailable with a named error.

## What would falsify this design

- A fault injection shows two commits in one interval. The fence is then insufficient.
- Safety depends on synchronized clocks. The design fails its own premise.
- A result, `KeyCursor`, or error changes with the tier that answered. Byte identity is false.
- Cold-tier p99 exceeds the stated budget after warm-up, and no placement change recovers it.
- Recovery after an ungraceful stop loses a committed write or resurrects a stale one.
- The placement plan needs a clock, randomness, or arrival order to reproduce.
- A second driver forces a semantic difference that the first driver hid.

## Open questions

1. What authority lease duration and guard time hold the availability budget at p99?
2. Do object stores in scope offer conditional writes strong enough for the fence?
3. Should one authority lease cover a set of shards, and what does batch handover cost?
4. Which policy facts must stay pinned in RAM, so that a rule-checked write never waits on a cold tier?
5. Is encryption per tier, per segment, or per shard, and where does the key live for a cold read?
6. Who pays for warm-up: the first reader, a background pass, or the query budget?
