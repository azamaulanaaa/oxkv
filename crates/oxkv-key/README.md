# oxkv-key

Canonical key encoding v1 for oxkv.

One logical key has one byte string. Lexicographic byte order equals the
semantic tuple order. A table with prefix `P` owns the half-open interval
`[P, successor(P))`.

This crate is portable. It compiles for native and `wasm32-unknown-unknown`. It
uses no clock, randomness, I/O, floating point, or `unsafe`.

## Wire format

```text
0x01 || table-name || 0x00 || component...
0x01 || table-name || 0x00 || 0xFF || system-id || component...
```

A user table name matches `[a-z][a-z0-9_]*`. The name has 1 to 63 ASCII bytes.
A user table name does not start with `_`. The catalog table `_catalog` is the
only system table name in 1.0.

| Type | Tag | Payload |
| --- | --- | --- |
| text | `0x10` | UTF-8 bytes. `0x00` becomes `0x00 0xFF`. The value ends with `0x00 0x00`. |
| bytes | `0x11` | Arbitrary bytes. The escape and terminator match text. |
| u64 | `0x20` | 8 bytes, big-endian. |
| i64 | `0x21` | 8 bytes, big-endian with the high bit flipped. |
| bool | `0x30` | `0x00` or `0x01`. |

A system key uses the `0xFF` marker and a registered system identifier. A user
component rejects `0xFF`.

## Limits

| Limit | Value |
| --- | --- |
| Key size | 4096 bytes |
| Components per key | 16 |
| Component payload | 1024 bytes |
| Table name | 63 bytes |

The decoder checks the key size first. It rejects an invalid key. It never
repairs a key.

## Errors

Each error returns a stable code.

| Code | Cause |
| --- | --- |
| `key.unknown_version` | The first byte is not `0x01`. |
| `key.malformed` | The key structure is invalid. |
| `key.non_canonical` | The components do not match the table schema. |
| `key.outside_table` | The key belongs to another table. |
| `key.invalid_utf8` | A text component is not valid UTF-8. |
| `key.too_large` | A size limit is exceeded. |
| `key.reserved_system_marker` | A user component uses `0xFF`. |
| `key.unknown_system_id` | The system identifier is not registered. |

## Check

```bash
cargo test -p oxkv-key
cargo clippy -p oxkv-key --all-targets -- -D warnings
cargo check -p oxkv-key --target wasm32-unknown-unknown
cargo fmt --check -p oxkv-key
```
