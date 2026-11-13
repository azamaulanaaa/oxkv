# oxkv-rpc

`oxkv-rpc` holds the generated RPC surface for oxkv. The manifest is the only
source. The crate compiles one neutral descriptor and generates one method
surface from it.

## Layers

- `descriptor` defines `descriptor/v1`. It lists methods, message types,
  fields, field cardinality, wire types, error codes, auth classes, and
  streaming shapes. It emits canonical bytes, canonical text, and a `BLAKE3`
  digest. No Rust type layout, serde default, or enum discriminant is
  contractual.
- `manifest` and `generator` compile the manifest and generate method metadata.
- `connect` defines the Connect RPC envelope. One server stream carries opaque
  bytes. A sync diff frame and a policy payload pass without a decode.
- `session` exposes the descriptor identity and the `SessionHello`
  compatibility check. The sync wire owns the handshake.
- `protocol` is the sans-IO core.
- `transport` has the in-process reference transport and a framed wrapper.

## Control namespace

Control operations use the `control` namespace. They use the same generator,
error mapping, and authentication as data operations. The surface has eight
control operations:

- `control/backup`
- `control/restore`
- `control/migrate`
- `control/rule_activate`
- `control/rule_rollback`
- `control/dlq_redrive`
- `control/quota`
- `control/audit_export`

## Errors

The RPC layer adds `rpc.unknown_method`, `rpc.descriptor_mismatch`, and
`rpc.codec_unsupported`. The sync errors pass through with the same meaning.

A data call before negotiation returns `sync.negotiation_required`. A
descriptor digest mismatch returns `rpc.descriptor_mismatch`. A transport
failure maps to a named protocol error. An unknown remote code maps to a named
client error and keeps the raw code.

## Boundary

This crate does not implement the sync state machine. Essay 14 owns the sync
frames, the cursor, and the resume rules. This crate carries those frames as
opaque bytes.

The crate uses no network, clock, randomness, file I/O, floating point, or
unsafe code.

## Verification

```text
cargo test -p oxkv-rpc
cargo clippy -p oxkv-rpc --all-targets -- -D warnings
cargo check -p oxkv-rpc --target wasm32-unknown-unknown
cargo fmt --check -p oxkv-rpc
```
