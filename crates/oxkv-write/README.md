# oxkv-write

Versioned write requests and a durable authority for oxkv.

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

## Durable state

- An authority can save its state to a data directory.
- The state covers the rows and their key versions, the receipt ledger
  entries, the commit version, the audit records, the audit checkpoints, and
  the chain tip.
- One canonical format holds the state. The format has a magic value, a format
  version, and a `BLAKE3` integrity value.
- `save_state` writes a temporary file and renames it over the state file.
- `load_state` rejects a missing, truncated, corrupt, or unknown-version file
  with `AuthorityStateError`.
- A save followed by a load round-trips the state exactly.
- The state file is `authority.state` in the data directory.
- State persistence is native only. The commit path stays pure.

## Modes and checkpoint signing

- A production authority takes a real `CheckpointSigner`.
- A production configuration that uses `StructuralSigner` fails at startup with
  `authority.structural_signer_in_production`.
- A local authority uses `StructuralSigner`. The structural signer is for
  tests and local runs. It is not a production signature scheme.
- The signer key identifier must match the configured checkpoint key.
- Production still requires a real `CheckpointSigner`. This tree has no
  production signer.

## Native binary

- The `oxkv-authority` binary opens one data directory and loads the saved state
  when it exists.
- The binary commits one write, saves the state, and prints the receipt in a
  stable text form.
- The binary uses `FileWitnessOutbox` for the checkpoints.
- The binary runs in local mode with `StructuralSigner`.
- The binary refuses production mode with a clear error. This tree has no real
  `CheckpointSigner`.

## Witness outbox

- The commit path stores each signed checkpoint in an asynchronous outbox.
- The commit path does not wait for witness delivery.
- `FileWitnessOutbox` is a native-only, file-backed outbox.
- A restart reads the pending checkpoint files and resumes delivery.
- `WitnessQueue` is the in-memory outbox for tests and local runs.

## Check order

1. Receipt lookup for the idempotency key.
2. Cross-shard check.
3. Base-version check.
4. Policy check.
5. Commit.

Authentication is outside this crate. Policy is reject-only. A commit hook
derives index or event effects only at the commit step.

## Boundaries

This crate does not implement the scheduler, the client outbox, the external
audit witness, the search index, or the event stream. It exposes pure policy
and commit hook seams for those crates. The audit chain is hash linked per
shard. Each checkpoint carries a signature. The crate stores no key material.

The commit path is portable and pure. It uses no clock, randomness, I/O,
floating point, or `unsafe`. Native state persistence and the native witness
outbox use file I/O.

## Example

```rust
use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
use oxkv_contracts::version::{
    KeyVersion, ManifestDigest, MapVersion, PolicyVersion, ProjectionEpoch, ProjectionWatermarks,
    ShardId,
};
use oxkv_security::secret::KeyIdentifier;
use oxkv_write::{AllowAll, Authority, AuthorityConfig, ClientId, Key, Mutation, WriteRequest};

let mut authority = Authority::local(AuthorityConfig::new(
    ShardId::new(vec![1]),
    PolicyVersion::from_bytes([4; 32]),
    ManifestDigest::from_bytes([5; 32]),
    MapVersion::new(6),
    ProjectionEpoch::new(7),
    ProjectionWatermarks::new(),
    KeyIdentifier::new("checkpoint-key-1"),
))
.expect("the local authority starts");
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
