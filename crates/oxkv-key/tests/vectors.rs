//! Golden vectors and property tests for key encoding v1.
//!
//! A released vector does not change. Add a case for new behavior.

mod support;

use oxkv_key::{Component, Key, Table, TableBounds, after_key, successor};
use serde_json::Value;

fn expected_str<'a>(case: &'a Value, field: &str) -> &'a str {
    case[field]
        .as_str()
        .unwrap_or_else(|| panic!("the case has no {field} field"))
}

fn cases(file: &Value) -> &[Value] {
    assert_eq!(
        file["contract"].as_str(),
        Some("key-encoding/v1"),
        "the file names the key encoding contract"
    );
    file["cases"]
        .as_array()
        .expect("the file has a case list")
        .as_slice()
}

#[test]
fn user_key_golden_vectors_are_pinned() {
    let file = support::load(include_str!("vectors/keys.json"));
    for case in cases(&file) {
        let name = expected_str(case, "name");
        let table = support::table(expected_str(case, "table"));
        let components = support::components(&case["components"]);
        let key = Key::new_user(table, components)
            .unwrap_or_else(|error| panic!("the user key {name} is invalid: {error}"));
        let bytes = key
            .encode()
            .unwrap_or_else(|error| panic!("the user key {name} does not encode: {error}"));
        assert_eq!(
            support::hex(&bytes),
            expected_str(case, "hex"),
            "the key bytes changed for {name}"
        );
        let decoded = Key::decode(&bytes)
            .unwrap_or_else(|error| panic!("the user key {name} does not decode: {error}"));
        assert_eq!(decoded, key, "the user key {name} did not round trip");
        assert_eq!(
            decoded.encode().expect("the decoded key encodes"),
            bytes,
            "the user key {name} changed on re-encode"
        );
    }
}

#[test]
fn system_key_golden_vectors_are_pinned() {
    let file = support::load(include_str!("vectors/systems.json"));
    for case in cases(&file) {
        let name = expected_str(case, "name");
        let table = support::table(expected_str(case, "table"));
        let components = support::components(&case["components"]);
        let id = u8::try_from(
            case["system_id"]
                .as_u64()
                .expect("the case names a system id"),
        )
        .expect("the system id fits in u8");
        let key = Key::new_system(table, id, components)
            .unwrap_or_else(|error| panic!("the system key {name} is invalid: {error}"));
        assert!(key.is_system());
        assert_eq!(key.system_id(), Some(id));
        let bytes = key
            .encode()
            .unwrap_or_else(|error| panic!("the system key {name} does not encode: {error}"));
        assert_eq!(
            support::hex(&bytes),
            expected_str(case, "hex"),
            "the key bytes changed for {name}"
        );
        let decoded = Key::decode(&bytes)
            .unwrap_or_else(|error| panic!("the system key {name} does not decode: {error}"));
        assert_eq!(decoded, key, "the system key {name} did not round trip");
    }
}

#[test]
fn bounds_golden_vectors_are_pinned() {
    let file = support::load(include_str!("vectors/bounds.json"));
    for case in cases(&file) {
        let name = expected_str(case, "name");
        let table = support::table(expected_str(case, "table"));
        let bounds = TableBounds::new(&table);
        assert_eq!(
            support::hex(bounds.lower()),
            expected_str(case, "prefix_hex"),
            "the prefix changed for {name}"
        );
        assert_eq!(
            support::hex(bounds.upper()),
            expected_str(case, "upper_hex"),
            "the upper bound changed for {name}"
        );
        assert_eq!(
            successor(bounds.lower()).as_deref(),
            Some(bounds.upper()),
            "the successor is wrong for {name}"
        );
        assert!(bounds.contains(bounds.lower()));
        assert!(!bounds.contains(bounds.upper()));
        let key = Key::new_user(table, vec![Component::bool(false)])
            .expect("the sample key is valid")
            .encode()
            .expect("the sample key encodes");
        assert!(bounds.contains(&key), "the table range rejects its key");
    }
    let after_cases = file["after_key_cases"]
        .as_array()
        .expect("the file has after-key cases");
    for case in after_cases {
        let name = expected_str(case, "name");
        let last = support::unhex(expected_str(case, "last_hex"));
        let expected = support::unhex(expected_str(case, "after_hex"));
        let actual = after_key(&last);
        assert_eq!(actual, expected, "the after-key changed for {name}");
        assert!(actual.as_slice() > last.as_slice());
    }
}

#[test]
fn every_named_error_has_a_golden_vector() {
    let file = support::load(include_str!("vectors/errors.json"));
    let mut seen = Vec::new();
    for case in cases(&file) {
        let name = expected_str(case, "name");
        let bytes = support::unhex(expected_str(case, "hex"));
        let result = if let Some(schema) = case.get("schema") {
            let table = support::table(expected_str(case, "table"));
            Key::decode_with_schema(&table, &support::schema(schema), &bytes)
        } else if let Some(table) = case.get("table") {
            let table = support::table(table.as_str().expect("the table is text"));
            Key::decode_in_table(&table, &bytes)
        } else {
            Key::decode(&bytes)
        };
        let error = result
            .err()
            .unwrap_or_else(|| panic!("the case {name} was accepted but must be rejected"));
        assert_eq!(
            error.code(),
            expected_str(case, "code"),
            "the error code changed for {name}"
        );
        seen.push(error.code());
    }
    for code in [
        "key.unknown_version",
        "key.malformed",
        "key.non_canonical",
        "key.outside_table",
        "key.invalid_utf8",
        "key.too_large",
        "key.reserved_system_marker",
        "key.unknown_system_id",
    ] {
        assert!(
            seen.contains(&code),
            "the vectors do not cover the error {code}"
        );
    }
}

#[test]
fn generated_keys_round_trip() {
    let mut rng = support::Rng::new(0x05_4b_45_59_01);
    for index in 0..2000 {
        let key = support::random_key(&mut rng);
        let bytes = key.encode().expect("the generated key encodes");
        let decoded = Key::decode(&bytes).expect("the generated key decodes");
        assert_eq!(decoded, key, "the generated key {index} did not round trip");
        assert_eq!(
            decoded.encode().expect("the decoded key encodes"),
            bytes,
            "the generated key {index} changed on re-encode"
        );
    }
}

#[test]
fn byte_order_equals_tuple_order() {
    let mut rng = support::Rng::new(0x05_4b_45_59_02);
    for index in 0..2000 {
        let (left, right) = if index % 2 == 0 {
            let table = support::random_table(&mut rng);
            (
                support::random_key_for(&table, &mut rng),
                support::random_key_for(&table, &mut rng),
            )
        } else {
            (support::random_key(&mut rng), support::random_key(&mut rng))
        };
        let left_bytes = left.encode().expect("the generated key encodes");
        let right_bytes = right.encode().expect("the generated key encodes");
        assert_eq!(
            left.cmp(&right),
            left_bytes.cmp(&right_bytes),
            "the tuple order and byte order differ for case {index}"
        );
    }
}

#[test]
fn a_table_range_contains_only_its_own_keys() {
    let tables: Vec<Table> = [
        "a",
        "b",
        "accounts",
        "ledger_entry",
        "z9",
        "a_b",
        "table_1",
        "xyz",
    ]
    .iter()
    .map(|name| Table::parse_user(name).expect("the table name is valid"))
    .collect();
    let mut rng = support::Rng::new(0x05_4b_45_59_03);
    let mut keys = Vec::new();
    for table in &tables {
        for _ in 0..20 {
            keys.push(support::random_key_for(table, &mut rng));
        }
    }
    for table in &tables {
        let bounds = TableBounds::new(table);
        for key in &keys {
            let bytes = key.encode().expect("the generated key encodes");
            let expected = key.table() == table;
            assert_eq!(
                bounds.contains(&bytes),
                expected,
                "the range for {table} has the wrong membership"
            );
        }
    }
}

#[test]
fn a_resume_after_every_key_returns_the_next_key() {
    let table = Table::parse_user("accounts").expect("the table name is valid");
    let mut rng = support::Rng::new(0x05_4b_45_59_04);
    let mut keys: Vec<Key> = (0..500)
        .map(|_| support::random_key_for(&table, &mut rng))
        .collect();
    keys.sort();
    keys.dedup();
    assert!(keys.len() > 2, "the corpus has enough unique keys");
    let bounds = TableBounds::new(&table);
    for window in keys.windows(2) {
        let last = window[0].encode().expect("the generated key encodes");
        let next = window[1].encode().expect("the generated key encodes");
        let resume = after_key(&last);
        assert!(
            resume.as_slice() > last.as_slice(),
            "the resume repeats a key"
        );
        assert!(
            resume.as_slice() <= next.as_slice(),
            "the resume skips a key"
        );
        assert_eq!(
            bounds.resume_after(&last),
            Some(resume),
            "the table resume bound is wrong"
        );
    }
}
