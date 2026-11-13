#![allow(dead_code, missing_docs)]

//! Shared helpers for the ordered key-value tests.

use oxkv_contracts::version::{CommitVersion, ShardId};
use oxkv_kv::{BTreeEngine, Batch, Limits, LsmEngine, Operation, OrderedMap};

pub(crate) const SHARD: &[u8] = b"shard-1";

pub(crate) fn shard() -> ShardId {
    ShardId::new(SHARD.to_vec())
}

pub(crate) fn other_shard() -> ShardId {
    ShardId::new(b"shard-2".to_vec())
}

pub(crate) fn put(key: &[u8], value: &[u8]) -> Operation {
    Operation::put(key.to_vec(), value.to_vec())
}

pub(crate) fn delete(key: &[u8]) -> Operation {
    Operation::delete(key.to_vec())
}

pub(crate) fn batch(version: u64, operations: Vec<Operation>) -> Batch {
    Batch::new(shard(), CommitVersion::new(version), operations)
}

pub(crate) fn btree(limits: Limits) -> BTreeEngine {
    BTreeEngine::new(shard(), limits)
}

pub(crate) fn lsm(limits: Limits) -> LsmEngine {
    LsmEngine::new(shard(), limits)
}

/// Runs one check against both engines.
pub(crate) fn for_both_engines(check: impl Fn(&mut dyn OrderedMap)) {
    let mut btree = btree(Limits::default());
    check(&mut btree);
    let mut lsm = lsm(Limits::default());
    check(&mut lsm);
}

pub(crate) fn commit(engine: &mut dyn OrderedMap, version: u64, operations: Vec<Operation>) {
    engine
        .commit(batch(version, operations))
        .expect("the batch commits");
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

pub(crate) fn unhex(text: &str) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2),
        "the hexadecimal text has an even length"
    );
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_digit(bytes[index]);
        let low = hex_digit(bytes[index + 1]);
        out.push((high << 4) | low);
        index += 2;
    }
    out
}

fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => panic!("the hexadecimal digit is invalid"),
    }
}
