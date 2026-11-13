//! Golden vectors for the static shard map.

mod support;

use oxkv_contracts::version::{MapVersion, ShardId};
use oxkv_map::ShardMap;
use serde_json::Value;

/// Builds the golden map from one vector file.
fn build(file: &Value) -> ShardMap {
    let map_version = MapVersion::new(support::field_u64(file, "map_version"));
    let catalog = ShardId::new(support::bytes(support::field_str(
        file,
        "catalog_shard_id_hex",
    )));
    let assignments = file["assignments"]
        .as_array()
        .expect("the vector has an assignment list")
        .iter()
        .map(|assignment| {
            (
                ShardId::new(support::bytes(support::field_str(
                    assignment,
                    "shard_id_hex",
                ))),
                support::table(support::field_str(assignment, "table")),
            )
        });
    ShardMap::from_assignments(map_version, catalog, assignments).expect("the golden map is valid")
}

#[test]
fn the_map_golden_vector_is_pinned() {
    let file = support::load(include_str!("vectors/map.json"));
    assert_eq!(support::field_str(&file, "contract"), "oxkv/map/v1");
    let map = build(&file);
    assert_eq!(
        map.map_version(),
        MapVersion::new(support::field_u64(&file, "map_version"))
    );
    assert_eq!(
        support::hex(&map.canonical_bytes()),
        support::field_str(&file, "canonical_hex")
    );
    assert_eq!(
        map.digest().to_hex(),
        support::field_str(&file, "digest_hex")
    );
}

#[test]
fn the_map_entry_bounds_are_pinned() {
    let file = support::load(include_str!("vectors/map.json"));
    let map = build(&file);
    let expected = file["entries"]
        .as_array()
        .expect("the vector has an entry list");
    assert_eq!(map.entries().len(), expected.len());
    for (entry, expected) in map.entries().iter().zip(expected) {
        assert_eq!(
            entry.table().as_str(),
            support::field_str(expected, "table")
        );
        assert_eq!(
            support::hex(entry.shard_id().as_bytes()),
            support::field_str(expected, "shard_id_hex")
        );
        assert_eq!(
            support::hex(entry.bounds().lower()),
            support::field_str(expected, "lower_hex")
        );
        assert_eq!(
            support::hex(entry.bounds().upper()),
            support::field_str(expected, "upper_hex")
        );
    }
}

#[test]
fn the_map_golden_routes_are_pinned() {
    let file = support::load(include_str!("vectors/map.json"));
    let map = build(&file);
    for expected in file["routes"]
        .as_array()
        .expect("the vector has a route list")
    {
        let key = support::bytes(support::field_str(expected, "key_hex"));
        let shard = map.route_shard(&key).expect("the golden key is mapped");
        assert_eq!(
            support::hex(shard.as_bytes()),
            support::field_str(expected, "shard_id_hex"),
            "the route changed for {}",
            support::field_str(expected, "name")
        );
    }
}

#[test]
fn the_route_is_identical_for_every_target() {
    let file = support::load(include_str!("vectors/map.json"));
    let map = build(&file);
    for expected in file["routes"]
        .as_array()
        .expect("the vector has a route list")
    {
        let key = support::bytes(support::field_str(expected, "key_hex"));
        let entry = map.route(&key).expect("the golden key is mapped");
        assert_eq!(
            support::hex(entry.shard_id().as_bytes()),
            support::field_str(expected, "shard_id_hex")
        );
        assert_eq!(entry, map.route(&key).expect("the route is stable"));
    }
}

#[test]
fn the_digest_is_stable_across_entry_order() {
    let file = support::load(include_str!("vectors/map.json"));
    let forward = build(&file);
    let mut assignments: Vec<(ShardId, oxkv_contracts::TableName)> = file["assignments"]
        .as_array()
        .expect("the vector has an assignment list")
        .iter()
        .map(|assignment| {
            (
                ShardId::new(support::bytes(support::field_str(
                    assignment,
                    "shard_id_hex",
                ))),
                support::table(support::field_str(assignment, "table")),
            )
        })
        .collect();
    assignments.reverse();
    let reversed = ShardMap::from_assignments(
        MapVersion::new(support::field_u64(&file, "map_version")),
        ShardId::new(support::bytes(support::field_str(
            &file,
            "catalog_shard_id_hex",
        ))),
        assignments,
    )
    .expect("the reversed map is valid");
    assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
    assert_eq!(forward.digest(), reversed.digest());
}

#[test]
fn the_canonical_bytes_round_trip() {
    let file = support::load(include_str!("vectors/map.json"));
    let map = build(&file);
    let bytes = map.canonical_bytes();
    let decoded = ShardMap::from_canonical_bytes(&bytes).expect("the golden map decodes");
    assert_eq!(decoded, map);
    assert_eq!(decoded.digest(), map.digest());
}
