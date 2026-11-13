# 5. Key encoding v1

Key encoding v1 is annex A1 to C2 (snapshot and page format) and the first frozen byte contract of oxkv 1.0. It gives each logical key one canonical byte string, a total order shared by the authority, native, and wasm nodes, and exact half-open bounds for each table. After 1.0, those bytes and that order cannot change. A later version may add an encoding but cannot reinterpret v1 bytes.

## What the greater goal demands

The goal makes a table name a prefix and a data shard the range of one user table plus its system sub-namespaces. Every key under a prefix must form one contiguous lexicographic interval. If keys scatter, a shard cannot own a table, a scope cannot be one range, and a replica cannot prove that it holds every key for a query. Hash sharding distributes load, but it destroys prefix contiguity and range resume. V1 uses order-preserving range sharding.

A range must be resumable. A replica may go offline for a day and reconnect with a cursor that names a position after the last key received. A resume must not repeat or skip a key at a batch boundary. Both nodes need one after-key rule and identical bounds.

Two nodes must agree on order. The authority, a native replica, and a wasm replica must compare the same two keys the same way. The encoding cannot depend on host endianness, locale, hash seed, or comparison-time normalization.

Policy facts project into shards, so fact keys, data keys, and scope bounds must use the same encoding and table prefixes.

## Required capabilities

1. A canonical codec that maps a typed tuple to one byte string and back.
2. A total order in which lexicographic byte order equals semantic tuple order.
3. Unambiguous prefix bounds for every table.
4. A version tag that a reader checks before it decodes anything else.
5. A validator that rejects every non-canonical key.
6. A resumable range and cursor contract.
7. A migration rule that never reinterprets old bytes.
8. Golden vectors that native and wasm targets must both pass.

## Invariants that must hold

- One logical value has exactly one encoding under v1.
- For a fixed version and schema, byte order equals tuple order.
- A table with prefix `P` has all keys in `[P, successor(P))` and no other table's key.
- Every table declares a fixed key arity. No valid key in a table is a strict byte prefix of another valid key in that table.
- Text equality is exact Unicode scalar equality. The codec does not normalize, case-fold, or apply a locale.
- Validation rejects a bad key and never repairs it.
- A cursor resume returns the first key strictly greater than the last returned key.
- Keys from different versions occupy different key spaces.
- Size limits bound decode memory, and a validator rejects an oversized key first.

## Decisions and technical choices

V1 uses an order-preserving typed tuple encoding:

```text
0x01 || table-name || 0x00 || component...
```

The first byte is the `key_encoding_version` tag `0x01`. The table name is 1 to 63 ASCII bytes matching `[a-z][a-z0-9_]*`. The zero byte after the name is the table delimiter. A table has at least one component. V1 limits a key to 4 KiB and 16 components, and a component to 1 KiB.

Each component starts with a one-byte type tag:

| Type | Tag | Payload |
| --- | --- | --- |
| text | `0x10` | UTF-8 bytes, `0x00` escaped as `0x00 0xFF`, terminated by `0x00 0x00` |
| bytes | `0x11` | Arbitrary bytes, same escape and terminator |
| u64 | `0x20` | 8 bytes, big-endian |
| i64 | `0x21` | 8 bytes, big-endian with the high bit flipped |
| bool | `0x30` | `0x00` or `0x01` |

The manifest fixes component number, order, and types. Type tags make each component self-identifying, and a hex dump shows the table name and text value.

A system key uses the `0xFF` marker after the table prefix, then one registered system identifier: `0x01` receipt, `0x02` event, `0x03` audit, `0x04` projection, `0x05` index, `0x06` lease, and `0x07` statistics. The marker keeps every system key inside the user table range. The user key validator rejects `0xFF` as a component tag. The system key validator accepts only a registered identifier. The catalog table `_catalog` uses the same marker and identifiers.

The table prefix is `P = 0x01 || table-name || 0x00`. The lower bound is `P`, inclusive. The upper bound is `successor(P)`, exclusive, where `successor` replaces the final `0x00` with `0x01`. Because the grammar excludes `0x00`, `successor(P)` exceeds every key with prefix `P`. A `P || 0xFF` bound is wrong because it excludes a next byte of `0xFF`.

Each table name maps to one shard, and no table is split. Shard ranges use the half-open table bounds. The map rejects overlap and inversion. A data shard contains the user table and its system sub-namespaces.

Length-prefixed components are rejected. If length precedes a variable-length value, order sorts by length before content, so `b` sorts before `aa`. If length follows the value, a delimiter is still necessary. Fixed-width lengths do not repair this.

A bare separator in variable-length data is rejected. A zero byte without escaping is ambiguous when the value contains a zero byte. The chosen leaf escapes `0x00` and ends with a terminator. This is separator escaping inside a typed tuple, not a whole-key separator scheme.

UTF-8 text and arbitrary bytes are separate types. Text must be valid UTF-8 and compares as UTF-8 bytes, which preserves Unicode scalar order. No normalization is applied, so precomposed and decomposed forms are different keys. Applications that need equivalent matching declare and index a normalized field. This keeps a Unicode table version out of A1. Bytes cover opaque identifiers and protocol tokens. Floating point is excluded because NaN, negative zero, and multiple representations defeat a canonical order.

The `key_encoding_version` tag is the migration mechanism. A v2 encoding must use a new first byte and a complete new key space, and must not reuse `0x01` with different rules. Key encoding migration is part of the one D12 migration framework. It quiesces writes, drains the scheduler, rewrites under a new epoch, verifies counts and checksums, and flips the pointer. It does not run a permanent dual-read. A rollback is allowed only before the first v2 write. An unknown `key_encoding_version` returns `key.unknown_version` and is never compared as raw bytes.

Validation runs at write ingress, ShardSnapshot load, sync apply, and index build. It rejects an unknown `key_encoding_version`, table, or type tag, a manifest arity mismatch, a key outside its table prefix, a malformed escape, a bad bool, invalid text UTF-8, trailing bytes, and an oversized key. Stable errors are `key.unknown_version`, `key.malformed`, `key.non_canonical`, `key.outside_table`, `key.invalid_utf8`, `key.too_large`, `key.reserved_system_marker`, and `key.unknown_system_id`. Error text names the rule and byte offset and does not echo an unbounded key.

## Dependencies

The manifest declares table names, key types, arity, and index keys. C2 stores raw keys and the `key_encoding_version` without a fixed key length. C3 carries the `key_encoding_version`, bounds, and a `KeyCursor`. The policy language uses table names for scope and fact keys. The shard map uses derived bounds. The KV engine iterates in bytewise order. The qualification suite owns the vectors.

## Acceptance criteria

- Golden vectors cover every type, extreme values, empty and embedded-NUL text, multibyte UTF-8, the maximum size, and every named error.
- A property test proves `decode(encode(value)) = value`, byte order equals tuple order, and a table range contains only its own keys.
- Native and wasm targets produce identical bytes and identical order for the full vector set.
- The shard map routes every valid key to exactly one shard and rejects a split table.
- A scope over one table compiles to one half-open range.
- A cursor resume after every generated key returns no duplicate and no skipped key.
- A v1 reader rejects a v2 key with `key.unknown_version`.
- A migration rehearsal proves that the v1 and v2 key spaces do not overlap.
- Every validation error is named and stable.

## What would falsify this design

- A required query is not a contiguous table-prefix interval.
- A required type has no canonical order-preserving encoding within the limit.
- A valid key has two byte encodings.
- Two targets disagree on key order.
- A cursor resume skips or repeats a key.
- A table range includes a key from another table.
- A migration must read v1 bytes as v2.
- A common key is unreadable in a hex dump.
- Range sharding cannot meet the hot-shard falsifier, and no key design fixes it.

## Open questions

1. Exact Unicode scalar equality or NFC normalization? Recommendation: exact equality plus a normalized field for matching.
2. Which extra types does 1.0 need: UUID, decimal, timestamp, or none? Data timestamps are allowed. Policy has no clock.
3. Is ASCII table-name grammar sufficient, or do names need Unicode and a longer limit?
4. Should optional key components be forbidden? Recommendation: forbid them and use a separate table.
5. How should secondary index keys be namespaced against user tables in one shard?
6. Must the sync protocol authenticate cursors that expose the last key, or is opacity sufficient?

## Frozen annex A1

The `key_encoding_version` byte `0x01`, the table-name grammar and delimiter, the component tags and payloads, the `0xFF` system marker and system identifiers, the escape and terminator rules, integer byte order, lexicographic comparison, the prefix successor rule, half-open ranges, the validation rules, and the size limits cannot change. The set of tables, their schemas, their shard assignment, the `KeyCursor` token format, and the Rust API may change.
