#![allow(missing_docs)]
#![allow(dead_code)]

//! Shared support for the snapshot golden vectors.

use std::ops::Range;

use oxkv_contracts::version::{
    CommitVersion, FactSourceId, FactVersion, LeaseTerm, ManifestDigest, MapDigest, MapVersion,
    PolicyVersion, PrefixBounds, ProjectionEpoch, ProjectionWatermarks, ScopeIdentity,
    ScopeVersionVector, ShardId, VersionVector,
};
use oxkv_snapshot::directory::Directory;
use oxkv_snapshot::header::Header;
use oxkv_snapshot::{
    Record, ScopeProjectionBuilder, ScopeSnapshotParams, ShardSnapshot, ShardSnapshotParams,
};
use serde_json::Value;

/// One golden file.
pub(crate) struct GoldenFile {
    pub(crate) contract: String,
    pub(crate) cases: Vec<GoldenCase>,
}

/// One golden case.
pub(crate) struct GoldenCase {
    pub(crate) name: String,
    pub(crate) input: Value,
    pub(crate) expected: Value,
}

/// Loads one golden file.
pub(crate) fn load(text: &str) -> GoldenFile {
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    let contract = value["contract"]
        .as_str()
        .expect("the golden file names a contract")
        .to_owned();
    let cases = value["cases"]
        .as_array()
        .expect("the golden file has a case list")
        .iter()
        .map(|case| GoldenCase {
            name: case["name"]
                .as_str()
                .expect("the case names a vector")
                .to_owned(),
            input: case["input"].clone(),
            expected: case["expected"].clone(),
        })
        .collect();
    GoldenFile { contract, cases }
}

/// Returns the lowercase hexadecimal form of a byte slice.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// Parses a lowercase hexadecimal text.
pub(crate) fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "the hex text is even");
    let raw = text.as_bytes();
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let mut index = 0;
    while index < raw.len() {
        bytes.push((digit(raw[index]) << 4) | digit(raw[index + 1]));
        index += 2;
    }
    bytes
}

fn digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => panic!("the hex text uses a non-hex character"),
    }
}

/// Returns the value byte range of one header field.
pub(crate) fn field_value_range(bytes: &[u8], field: &str) -> Range<usize> {
    let marker = format!("{field}=");
    let mut line_start = 0;
    while line_start < bytes.len() {
        let line_end = bytes[line_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| line_start + position + 1);
        let line = &bytes[line_start..line_end];
        if line.starts_with(marker.as_bytes()) {
            let start = line_start + marker.len();
            return start..line_end - 1;
        }
        line_start = line_end;
    }
    panic!("the header has no {field} field");
}

/// Replaces one header field value. The new value must keep the length.
pub(crate) fn replace_field(bytes: &[u8], field: &str, value: &str) -> Vec<u8> {
    let range = field_value_range(bytes, field);
    assert_eq!(
        range.len(),
        value.len(),
        "the replacement value for {field} must keep the length"
    );
    let mut out = bytes.to_vec();
    out[range].copy_from_slice(value.as_bytes());
    out
}

/// Replaces one header field name. The new name must keep the length.
pub(crate) fn rename_field(bytes: &[u8], field: &str, name: &str) -> Vec<u8> {
    let range = field_value_range(bytes, field);
    let start = range.start - field.len() - 1;
    assert_eq!(
        field.len(),
        name.len(),
        "the new field name must keep the length"
    );
    let mut out = bytes.to_vec();
    out[start..start + field.len()].copy_from_slice(name.as_bytes());
    out
}

/// Replaces the first line of the header.
pub(crate) fn replace_first_line(bytes: &[u8], line: &str) -> Vec<u8> {
    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .expect("the header has a first line");
    assert_eq!(end, line.len(), "the new first line must keep the length");
    let mut out = bytes.to_vec();
    out[..end].copy_from_slice(line.as_bytes());
    out
}

/// Returns the sample records in key order.
pub(crate) fn sample_records() -> Vec<Record> {
    vec![
        Record::new(b"a1".to_vec(), b"one".to_vec()),
        Record::new(b"a2".to_vec(), b"two".to_vec()),
        Record::new(b"a3".to_vec(), b"three".to_vec()),
    ]
}

/// Returns the sample shard snapshot parameters.
pub(crate) fn sample_params() -> ShardSnapshotParams {
    ShardSnapshotParams::new(
        ShardId::new(vec![0x01, 0x02]),
        PrefixBounds::new(b"a".to_vec(), b"z".to_vec()),
        CommitVersion::new(9),
        ManifestDigest::from_bytes([0x11; 32]),
        MapVersion::new(4),
        MapDigest::from_bytes([0x22; 32]),
        LeaseTerm::new(3),
    )
}

/// Builds the sample shard snapshot.
pub(crate) fn sample_snapshot() -> Vec<u8> {
    ShardSnapshot::build(&sample_params(), &sample_records()).expect("the sample builds")
}

/// Returns the first page entry of a snapshot.
pub(crate) fn first_page(bytes: &[u8]) -> (oxkv_contracts::version::PageId, u64, u64) {
    let header = Header::parse(bytes).expect("the header parses");
    let start = usize::try_from(header.directory_offset).expect("the offset fits");
    let length = usize::try_from(header.directory_length).expect("the length fits");
    let directory = Directory::decode(&bytes[start..start + length], header.directory_offset)
        .expect("the directory decodes");
    let entry = directory.entries.first().expect("the sample has one page");
    (entry.page_id, entry.offset, entry.length)
}

/// Flips one page payload byte and returns the page id and page offset.
pub(crate) fn corrupt_page(
    bytes: &[u8],
    page_index: usize,
    byte_from_end: usize,
) -> (oxkv_contracts::version::PageId, u64, Vec<u8>) {
    let header = Header::parse(bytes).expect("the header parses");
    let start = usize::try_from(header.directory_offset).expect("the offset fits");
    let length = usize::try_from(header.directory_length).expect("the length fits");
    let directory = Directory::decode(&bytes[start..start + length], header.directory_offset)
        .expect("the directory decodes");
    let entry = &directory.entries[page_index];
    let mut out = bytes.to_vec();
    let offset = usize::try_from(entry.offset).expect("the offset fits");
    let page_length = usize::try_from(entry.length).expect("the length fits");
    let position = offset + page_length - byte_from_end;
    out[position] ^= 0x01;
    (entry.page_id, entry.offset, out)
}

/// Returns the sample scope version vector.
pub(crate) fn sample_scope_vector() -> ScopeVersionVector {
    ScopeVersionVector::from_vector(VersionVector::from_entries([
        (ShardId::new(vec![0x01]), CommitVersion::new(9)),
        (ShardId::new(vec![0x02]), CommitVersion::new(7)),
    ]))
}

/// Returns the sample scope snapshot parameters.
pub(crate) fn sample_scope_params() -> ScopeSnapshotParams {
    let mut watermarks = ProjectionWatermarks::new();
    watermarks.set(FactSourceId::new("accounts"), FactVersion::new(3));
    ScopeSnapshotParams::with_defaults(
        ScopeIdentity::from_bytes([0x33; 32]),
        PolicyVersion::from_bytes([0x44; 32]),
        sample_scope_vector(),
        ProjectionEpoch::new(5),
        watermarks,
        ManifestDigest::from_bytes([0x11; 32]),
        MapVersion::new(4),
        MapDigest::from_bytes([0x22; 32]),
        LeaseTerm::new(3),
    )
}

/// Returns one real v1 user key.
pub(crate) fn user_key(table: &str, text: &str) -> Vec<u8> {
    let mut key = vec![0x01];
    key.extend_from_slice(table.as_bytes());
    key.push(0x00);
    key.push(0x10);
    key.extend_from_slice(text.as_bytes());
    key.extend_from_slice(&[0x00, 0x00]);
    key
}

/// Returns one real v1 system key.
pub(crate) fn system_key(table: &str, id: u8, text: &str) -> Vec<u8> {
    let mut key = vec![0x01];
    key.extend_from_slice(table.as_bytes());
    key.push(0x00);
    key.push(0xff);
    key.push(id);
    key.push(0x10);
    key.extend_from_slice(text.as_bytes());
    key.extend_from_slice(&[0x00, 0x00]);
    key
}

/// Admits every record. Use a narrower predicate to test a scope boundary.
pub(crate) fn admit_all(_key: &[u8], _value: &[u8]) -> bool {
    true
}

/// Builds the sample scope snapshot.
pub(crate) fn sample_scope_snapshot() -> Vec<u8> {
    let mut builder = ScopeProjectionBuilder::new(sample_scope_params(), admit_all);
    builder
        .insert(
            ShardId::new(vec![0x01]),
            CommitVersion::new(9),
            Record::new(user_key("accounts", "alice"), b"one".to_vec()),
        )
        .expect("the row is admitted");
    builder
        .insert_projection(
            ShardId::new(vec![0x01]),
            CommitVersion::new(9),
            Record::new(system_key("accounts", 0x04, "fact"), b"fact".to_vec()),
        )
        .expect("the fact is admitted");
    builder
        .insert(
            ShardId::new(vec![0x02]),
            CommitVersion::new(7),
            Record::new(user_key("ledger_entry", "entry"), b"two".to_vec()),
        )
        .expect("the row is admitted");
    builder.build().expect("the scope builds")
}
