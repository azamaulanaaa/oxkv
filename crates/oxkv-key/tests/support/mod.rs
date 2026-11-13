#![allow(missing_docs, dead_code)]

//! Shared support for the key encoding vectors and property tests.

use oxkv_key::{Component, ComponentType, Key, KeySchema, Table};
use serde_json::Value;

/// Loads one golden file.
pub(crate) fn load(text: &str) -> Value {
    serde_json::from_str(text).expect("the golden file is valid JSON")
}

/// Returns the lowercase hexadecimal form of a byte slice.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// Parses a lowercase hexadecimal string.
pub(crate) fn unhex(text: &str) -> Vec<u8> {
    let digits = text.as_bytes();
    let (pairs, remainder) = digits.as_chunks::<2>();
    assert!(remainder.is_empty(), "the hexadecimal text has pairs");
    let mut bytes = Vec::with_capacity(pairs.len());
    for &[high, low] in pairs {
        bytes.push((hex_digit(high) << 4) | hex_digit(low));
    }
    bytes
}

fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("the hexadecimal digit is invalid: {byte:#04x}"),
    }
}

/// Parses one table name.
pub(crate) fn table(name: &str) -> Table {
    Table::parse(name).unwrap_or_else(|error| panic!("the table {name} is invalid: {error}"))
}

/// Parses one component from JSON.
pub(crate) fn component(value: &Value) -> Component {
    let kind = value["type"].as_str().expect("the component names a type");
    match kind {
        "text" => Component::text(
            value["value"]
                .as_str()
                .expect("the text component has a value"),
        ),
        "bytes" => Component::bytes(unhex(
            value["hex"].as_str().expect("the bytes component has hex"),
        )),
        "u64" => Component::u64(
            value["value"]
                .as_str()
                .expect("the u64 component has a value")
                .parse()
                .expect("the u64 value is valid"),
        ),
        "i64" => Component::i64(
            value["value"]
                .as_str()
                .expect("the i64 component has a value")
                .parse()
                .expect("the i64 value is valid"),
        ),
        "bool" => Component::bool(
            value["value"]
                .as_bool()
                .expect("the bool component has a value"),
        ),
        _ => panic!("the component type {kind} is unknown"),
    }
}

/// Parses a component list from JSON.
pub(crate) fn components(value: &Value) -> Vec<Component> {
    value
        .as_array()
        .expect("the components field is a list")
        .iter()
        .map(component)
        .collect()
}

/// Parses a key schema from JSON.
pub(crate) fn schema(value: &Value) -> KeySchema {
    let types: Vec<ComponentType> = value
        .as_array()
        .expect("the schema field is a list")
        .iter()
        .map(|item| {
            let name = item.as_str().expect("the schema type is text");
            match name {
                "text" => ComponentType::Text,
                "bytes" => ComponentType::Bytes,
                "u64" => ComponentType::U64,
                "i64" => ComponentType::I64,
                "bool" => ComponentType::Bool,
                _ => panic!("the schema type {name} is unknown"),
            }
        })
        .collect();
    KeySchema::new(types).expect("the schema is valid")
}

/// A deterministic generator for the property corpus.
pub(crate) struct Rng(u64);

impl Rng {
    /// Creates a generator from a fixed seed.
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Returns the next 64-bit value.
    pub(crate) fn next_u64(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    /// Returns the next boolean value.
    pub(crate) fn next_bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    /// Returns the next signed 64-bit value.
    pub(crate) fn next_i64(&mut self) -> i64 {
        i64::from_ne_bytes(self.next_u64().to_ne_bytes())
    }

    /// Returns a value below one positive bound.
    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "the bound is positive");
        self.next_u64() % bound
    }

    /// Returns an index below one positive bound.
    pub(crate) fn below_usize(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).expect("the bound fits in u64");
        usize::try_from(self.below(bound)).expect("the value fits in usize")
    }
}

/// Returns a random user table name.
pub(crate) fn random_table(rng: &mut Rng) -> Table {
    let length = 1 + rng.below_usize(63);
    let mut name = String::with_capacity(length);
    name.push(char::from(
        b'a' + u8::try_from(rng.below(26)).expect("the value fits in u8"),
    ));
    for _ in 1..length {
        let choice = rng.below(37);
        let byte = if choice < 26 {
            b'a' + u8::try_from(choice).expect("the value fits in u8")
        } else if choice < 36 {
            b'0' + u8::try_from(choice - 26).expect("the value fits in u8")
        } else {
            b'_'
        };
        name.push(char::from(byte));
    }
    Table::parse_user(&name).expect("the generated table name is valid")
}

/// Returns a random text value.
pub(crate) fn random_text(rng: &mut Rng) -> String {
    let length = rng.below_usize(8);
    let mut text = String::new();
    for _ in 0..length {
        match rng.below(6) {
            0 => text.push('\u{0000}'),
            1 => text.push('\u{00e9}'),
            2 => text.push('\u{4e2d}'),
            3 => text.push('\u{1f980}'),
            4 => text.push(char::from(
                b'a' + u8::try_from(rng.below(26)).expect("the value fits in u8"),
            )),
            _ => text.push(char::from(
                b'0' + u8::try_from(rng.below(10)).expect("the value fits in u8"),
            )),
        }
    }
    text
}

/// Returns a random bytes value.
pub(crate) fn random_bytes(rng: &mut Rng) -> Vec<u8> {
    let length = rng.below_usize(16);
    let mut bytes = Vec::with_capacity(length);
    for _ in 0..length {
        let [byte, ..] = rng.next_u64().to_le_bytes();
        bytes.push(byte);
    }
    bytes
}

/// Returns a random component.
pub(crate) fn random_component(rng: &mut Rng) -> Component {
    match rng.below(5) {
        0 => Component::text(random_text(rng)),
        1 => Component::bytes(random_bytes(rng)),
        2 => Component::u64(rng.next_u64()),
        3 => Component::i64(rng.next_i64()),
        _ => Component::bool(rng.next_bool()),
    }
}

/// Returns a random key for one table.
pub(crate) fn random_key_for(table: &Table, rng: &mut Rng) -> Key {
    let count = 1 + rng.below_usize(16);
    let components: Vec<Component> = (0..count).map(|_| random_component(rng)).collect();
    if rng.next_bool() {
        let id = u8::try_from(1 + rng.below(7)).expect("the value fits in u8");
        Key::new_system(table.clone(), id, components).expect("the generated system key is valid")
    } else {
        Key::new_user(table.clone(), components).expect("the generated user key is valid")
    }
}

/// Returns a random key for a random table.
pub(crate) fn random_key(rng: &mut Rng) -> Key {
    let table = random_table(rng);
    random_key_for(&table, rng)
}
