//! Validation and routing tests for the static shard map.

use oxkv_contracts::TableName;
use oxkv_contracts::keyspace::CATALOG_TABLE;
use oxkv_contracts::version::{MapVersion, PrefixBounds, ShardId};
use oxkv_map::{MAP_MAGIC, MAP_SCHEMA, MapError, ShardEntry, ShardMap, table_bounds};

fn table(name: &str) -> TableName {
    TableName::parse_user(name).expect("the table name is valid")
}

fn valid_map() -> ShardMap {
    ShardMap::from_assignments(
        MapVersion::new(7),
        ShardId::new(vec![0]),
        [
            (ShardId::new(vec![1]), table("accounts")),
            (ShardId::new(vec![2]), table("ledger_entry")),
        ],
    )
    .expect("the map is valid")
}

fn with_catalog(entries: impl IntoIterator<Item = ShardEntry>) -> Vec<ShardEntry> {
    let mut entries: Vec<ShardEntry> = entries.into_iter().collect();
    entries.push(ShardEntry::catalog(ShardId::new(vec![0])));
    entries
}

#[test]
fn a_split_table_fails() {
    let entries = with_catalog([
        ShardEntry::data(ShardId::new(vec![1]), table("accounts")).expect("the entry is valid"),
        ShardEntry::data(ShardId::new(vec![2]), table("accounts")).expect("the entry is valid"),
    ]);
    let error = ShardMap::new(MapVersion::new(1), entries).unwrap_err();
    assert_eq!(error.code(), "map.table_split_unsupported");
    assert_eq!(
        error,
        MapError::TableSplitUnsupported {
            table: "accounts".to_owned()
        }
    );
}

#[test]
fn an_overlapping_range_fails() {
    let bounds = table_bounds(&table("accounts"));
    let mut upper = bounds.upper().to_vec();
    upper.push(0x00);
    let entries = with_catalog([ShardEntry::from_parts(
        ShardId::new(vec![1]),
        table("accounts"),
        PrefixBounds::new(bounds.lower().to_vec(), upper),
    )]);
    assert_eq!(
        ShardMap::new(MapVersion::new(1), entries)
            .unwrap_err()
            .code(),
        "map.overlap"
    );
}

#[test]
fn a_gap_fails() {
    let bounds = table_bounds(&table("accounts"));
    let entries = with_catalog([ShardEntry::from_parts(
        ShardId::new(vec![1]),
        table("accounts"),
        PrefixBounds::new(bounds.lower().to_vec(), bounds.lower().to_vec()),
    )]);
    assert_eq!(
        ShardMap::new(MapVersion::new(1), entries)
            .unwrap_err()
            .code(),
        "map.empty_range"
    );

    let bounds = table_bounds(&table("accounts"));
    let mut short_upper = bounds.lower().to_vec();
    short_upper.push(0x10);
    let entries = with_catalog([ShardEntry::from_parts(
        ShardId::new(vec![1]),
        table("accounts"),
        PrefixBounds::new(bounds.lower().to_vec(), short_upper),
    )]);
    assert_eq!(
        ShardMap::new(MapVersion::new(1), entries)
            .unwrap_err()
            .code(),
        "map.gap"
    );
}

#[test]
fn a_duplicate_shard_id_fails() {
    let entries = with_catalog([
        ShardEntry::data(ShardId::new(vec![1]), table("accounts")).expect("the entry is valid"),
        ShardEntry::data(ShardId::new(vec![1]), table("ledger_entry")).expect("the entry is valid"),
    ]);
    let error = ShardMap::new(MapVersion::new(1), entries).unwrap_err();
    assert_eq!(error.code(), "map.duplicate_id");
    assert_eq!(
        error,
        MapError::DuplicateId {
            shard_id: "01".to_owned()
        }
    );
}

#[test]
fn an_unmapped_table_fails() {
    let map = valid_map();
    let missing = table("missing");
    assert_eq!(
        map.validate_tables([&table("accounts"), &missing])
            .unwrap_err()
            .code(),
        "map.table_unmapped"
    );
    assert_eq!(
        map.shard_for_table(&missing).unwrap_err().code(),
        "map.table_unmapped"
    );
    assert_eq!(
        map.shards_for_tables([&missing]).unwrap_err().code(),
        "map.table_unmapped"
    );
}

#[test]
fn an_empty_range_fails() {
    let bounds = table_bounds(&table("accounts"));
    let entries = with_catalog([ShardEntry::from_parts(
        ShardId::new(vec![1]),
        table("accounts"),
        PrefixBounds::new(bounds.upper().to_vec(), bounds.lower().to_vec()),
    )]);
    assert_eq!(
        ShardMap::new(MapVersion::new(1), entries)
            .unwrap_err()
            .code(),
        "map.empty_range"
    );
}

#[test]
fn an_unknown_map_version_fails() {
    let map = valid_map();
    let mut bytes = map.canonical_bytes();
    let schema_offset = MAP_MAGIC.len() + 2 + 4 + MAP_SCHEMA.len() - 1;
    bytes[schema_offset] = b'2';
    let error = ShardMap::from_canonical_bytes(&bytes).unwrap_err();
    assert_eq!(error.code(), "map.unknown_version");
    assert_eq!(
        error,
        MapError::UnknownVersion {
            found: "oxkv/map/v2".to_owned()
        }
    );
}

#[test]
fn a_map_without_a_catalog_fails() {
    let entries =
        [ShardEntry::data(ShardId::new(vec![1]), table("accounts")).expect("the entry is valid")];
    assert_eq!(
        ShardMap::new(MapVersion::new(1), entries)
            .unwrap_err()
            .code(),
        "map.table_unmapped"
    );
    assert_eq!(
        ShardMap::from_assignments(
            MapVersion::new(1),
            ShardId::new(vec![0]),
            [(ShardId::new(vec![1]), TableName::catalog())],
        )
        .unwrap_err()
        .code(),
        "map.table_split_unsupported"
    );
    assert_eq!(CATALOG_TABLE, "_catalog");
}

#[test]
fn the_error_codes_are_stable() {
    let map = valid_map();
    let mut schema = map.canonical_bytes();
    schema[MAP_MAGIC.len() + 2 + 4 + MAP_SCHEMA.len() - 1] = b'9';

    let bounds = table_bounds(&table("accounts"));
    let empty = with_catalog([ShardEntry::from_parts(
        ShardId::new(vec![1]),
        table("accounts"),
        PrefixBounds::new(bounds.upper().to_vec(), bounds.lower().to_vec()),
    )]);
    let split = with_catalog([
        ShardEntry::data(ShardId::new(vec![1]), table("accounts")).expect("the entry is valid"),
        ShardEntry::data(ShardId::new(vec![2]), table("accounts")).expect("the entry is valid"),
    ]);
    let duplicate = with_catalog([
        ShardEntry::data(ShardId::new(vec![1]), table("accounts")).expect("the entry is valid"),
        ShardEntry::data(ShardId::new(vec![1]), table("ledger_entry")).expect("the entry is valid"),
    ]);

    let cases = [
        (
            ShardMap::from_canonical_bytes(&schema).unwrap_err().code(),
            "map.unknown_version",
        ),
        (
            ShardMap::new(MapVersion::new(1), empty).unwrap_err().code(),
            "map.empty_range",
        ),
        (
            ShardMap::new(MapVersion::new(1), split).unwrap_err().code(),
            "map.table_split_unsupported",
        ),
        (
            ShardMap::new(MapVersion::new(1), duplicate)
                .unwrap_err()
                .code(),
            "map.duplicate_id",
        ),
    ];
    for (actual, expected) in cases {
        assert_eq!(actual, expected);
    }
}

#[test]
fn the_map_routes_every_valid_key_to_one_shard() {
    let map = valid_map();
    let suffixes: [&[u8]; 6] = [
        b"\x10a\x00\x00",
        b"\x11\x00\xff\x00\x00",
        b"\x20\x00\x00\x00\x00\x00\x00\x00\x01",
        b"\x21\x7f\xff\xff\xff\xff\xff\xff\xff",
        b"\x30\x01",
        b"\xff\x01\x00",
    ];
    for entry in map.entries() {
        for suffix in suffixes {
            let mut key = entry.bounds().lower().to_vec();
            key.extend_from_slice(suffix);
            assert_eq!(
                map.route(&key).expect("the key is mapped").shard_id(),
                entry.shard_id()
            );
        }
        for value in 0_u64..16 {
            let mut key = entry.bounds().lower().to_vec();
            key.push(0x20);
            key.extend_from_slice(&value.to_be_bytes());
            assert_eq!(
                map.route(&key).expect("the key is mapped").shard_id(),
                entry.shard_id()
            );
        }
        for system_id in 0x01_u8..=0x07 {
            let mut key = entry.bounds().lower().to_vec();
            key.extend_from_slice(&[0xff, system_id, 0x00]);
            assert_eq!(
                map.route(&key).expect("the key is mapped").shard_id(),
                entry.shard_id()
            );
        }
    }
}

#[test]
fn the_route_is_the_same_on_native_and_wasm32() {
    let map = valid_map();
    let key = b"\x01accounts\x00\x10alice\x00\x00";
    let expected = ShardId::new(vec![1]);
    assert_eq!(map.route_shard(key).expect("the key is mapped"), &expected);
    assert_eq!(map.route_shard(key).expect("the key is mapped"), &expected);
}
