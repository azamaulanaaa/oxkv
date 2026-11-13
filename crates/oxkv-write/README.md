# oxkv-write

Versioned write requests and an in-memory authority commit simulator for oxkv.

## What it does

- A request carries one client identity, one idempotency key, and one base
  version for each mutated key.
- The authority owns the per-key version sequence and the shard commit sequence.
- The authority compares each base version at commit. The comparison is exact.
- A mismatch returns `Conflict` with `write.version_conflict`.
- A base ahead of the current key version returns `Rejected` with `write.stale_base_version`.
- A delete consumes a key version and leaves a tombstone.
- One idempotency key has one receipt. A retry with the same body returns the same receipt.
- A retry with a different body fails with `write.idempotency_key_reuse`.
- A cross-shard mutation fails with `shard.cross_mutation_rejected` before the version check.

## Check order

1. Receipt lookup for the idempotency key.
2. Cross-shard check.
3. Base-version check.
4. Policy check.
5. Commit.

Authentication is outside this crate. Policy is reject-only. A commit hook
derives index or event effects only at the commit step.

## Boundaries

This crate does not implement the scheduler, the outbox, the external audit
witness, the search index, or the event stream. It exposes pure policy and
commit hook seams for those crates. The audit chain is in memory and hash
linked per shard. It carries no checkpoint signature and no key material.

The crate is portable and pure. It uses no clock, randomness, I/O, floating
point, or `unsafe`.

## Example

```rust
use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
use oxkv_contracts::version::{
    KeyVersion, ManifestDigest, MapVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
    ShardId,
};
use oxkv_write::{AllowAll, Authority, AuthorityConfig, ClientId, Key, Mutation, WriteRequest};

let mut authority = Authority::new(AuthorityConfig::new(
    ShardId::new(vec![1]),
    PolicyVersion::from_bytes([4; 32]),
    ManifestDigest::from_bytes([5; 32]),
    MapVersion::new(6),
    ProjectionEpoch::new(7),
    ProjectionWatermarks::new(),
));
let request = WriteRequest::new(
    RequestId::from_bytes([2; 32]),
    ClientId::new(vec![7]),
    IdempotencyKey::new(vec![1]),
    [Mutation::put(
        Key::new(b"alpha".to_vec()),
        ShardId::new(vec![1]),
        KeyVersion::new(0),
        b"one".to_vec(),
    )],
);

let outcome = authority
    .submit(&request, &AllowAll, &[])
    .expect("the create commits");
assert_eq!(outcome.state().name(), "Committed");
```
