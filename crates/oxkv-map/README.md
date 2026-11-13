# oxkv-map

Static shard map v1 for oxkv.

The map assigns every user table to one data shard. It assigns `_catalog` to
the catalog shard. A table never splits across shards. Each shard entry owns
the half-open range `[P, successor(P))` of its table prefix.

The map is a static configuration. It is not a placement control plane. It
does not read stored data, a clock, a seed, or the environment.

## Bounds

The key encoding v1 table prefix is `P = 0x01 || table-name || 0x00`. The
successor rule replaces the final `0x00` with `0x01`. The shard range is
`[P, successor(P))`. The range contains every user key and every system key
for the table.

The key encoding crate is not in this worktree. The `bounds` module implements
the frozen successor rule. Replace that module with the key encoding crate
when the crate lands. The bytes and the order do not change.

Table ranges are not adjacent. A gap between two table ranges is normal. A
`map.gap` error means that one shard range does not cover the full prefix of
its own table. A `map.table_split_unsupported` error means that two entries
name one table.

## Canonical bytes

The canonical form is versioned. All integers are big-endian.

| Field | Encoding |
| --- | --- |
| Magic | `OXKVMAP` |
| Canonical format version | `u16`, value `1` |
| Map schema | `u32` length plus `oxkv/map/v1` |
| Map version | `u64` |
| Entry count | `u32` |
| Entry shard identifier | `u32` length plus opaque bytes |
| Entry role | `u8`, `0` data and `1` catalog |
| Entry table name | `u32` length plus UTF-8 bytes |
| Entry lower bound | `u32` length plus bytes |
| Entry upper bound | `u32` length plus bytes |

Entries are in range order. The map digest is the raw BLAKE3 hash of the
canonical map bytes.

## Validation

| Error code | Cause |
| --- | --- |
| `map.unknown_version` | The map schema or canonical format is unknown. |
| `map.overlap` | A range overlaps another range or leaves its table prefix. |
| `map.gap` | A range does not cover the full prefix of its table. |
| `map.duplicate_id` | Two entries use one shard identifier. |
| `map.empty_range` | A lower bound is not less than its upper bound. |
| `map.table_unmapped` | The map does not contain a declared table. |
| `map.table_split_unsupported` | One table occurs in more than one entry. |

## API use

| Consumer | API |
| --- | --- |
| Scope (essay 13) | `shards_for_tables` returns one sorted shard entry per shard. |
| Wire (essay 14) | `route` maps one encoded key to one shard. |
| Bounded staleness (essay 23) | `map_version` and `digest` identify the map pin. |

## Example

```rust
use oxkv_contracts::version::{MapVersion, ShardId};
use oxkv_contracts::TableName;
use oxkv_map::ShardMap;

let accounts = TableName::parse_user("accounts").expect("the name is valid");
let map = ShardMap::from_assignments(
    MapVersion::new(1),
    ShardId::new(vec![0]),
    [(ShardId::new(vec![1]), accounts)],
)
.expect("the map is valid");
assert_eq!(map.entries().len(), 2);
```
