#![allow(missing_docs, unreachable_pub)]

//! Shared fixtures for the replica tests.

use std::collections::BTreeMap;

use oxkv_contracts::version::{
    CommitVersion, Hash32, ManifestDigest, MapDigest, MapVersion, PageId, PageSequence,
    PolicyVersion, PrefixBounds, ScopeIdentity, ScopeVersionVector, ShardId, TokenizerVersion,
    VersionVector,
};
use oxkv_replica::{
    DeviceBudget, MirroredRecord, PageDescriptor, ReplicaConfig, ReplicaIdentity,
    SUPPORTED_PAGE_VERSION, Scope, ScopePin, ScopeSnapshot, ScopeSnapshotPage, Scorer,
};
use serde_json::Value;

pub const MANIFEST: [u8; 32] = [1; 32];
pub const POLICY: [u8; 32] = [2; 32];
pub const SCOPE: [u8; 32] = [5; 32];

pub fn scope() -> Scope {
    Scope::new(
        ScopePin::new(
            ManifestDigest::from_bytes(MANIFEST),
            PolicyVersion::from_bytes(POLICY),
            MapVersion::new(3),
            MapDigest::from_bytes([4; 32]),
            ScopeIdentity::from_bytes(SCOPE),
        ),
        TokenizerVersion::new(1),
        [
            oxkv_contracts::TableName::parse_user("accounts").expect("the table name is valid"),
            oxkv_contracts::TableName::parse_user("ledger").expect("the table name is valid"),
        ],
        [oxkv_contracts::version::FactSourceId::new("roles")],
    )
}

pub fn config(max_resident_bytes: u64, max_hydration_overhead_bytes: u64) -> ReplicaConfig {
    ReplicaConfig::new(
        ReplicaIdentity::new(b"origin".to_vec()),
        DeviceBudget::new(max_resident_bytes, max_hydration_overhead_bytes),
    )
    .with_scorer(Scorer::DEFAULT)
}

pub fn shard() -> ShardId {
    ShardId::new(vec![1])
}

pub fn vector(version: u64) -> ScopeVersionVector {
    ScopeVersionVector::from_vector(VersionVector::from_entries([(
        shard(),
        CommitVersion::new(version),
    )]))
}

pub fn key(name: &str) -> Vec<u8> {
    let mut key = b"\x01accounts\x00\x10".to_vec();
    key.extend_from_slice(name.as_bytes());
    key
}

pub fn row(name: &str, value: &str, key_version: u64) -> MirroredRecord {
    MirroredRecord::row(
        key(name),
        value.as_bytes().to_vec(),
        oxkv_contracts::version::KeyVersion::new(key_version),
    )
}

pub fn page(sequence: u64, commit_version: u64, records: Vec<MirroredRecord>) -> ScopeSnapshotPage {
    let label = format!("page-{sequence}-{commit_version}");
    ScopeSnapshotPage::new(
        PageDescriptor {
            format_version: SUPPORTED_PAGE_VERSION,
            page_id: PageId::from_bytes(*Hash32::of(label.as_bytes()).as_bytes()),
            page_sequence: PageSequence::new(sequence),
            shard_id: shard(),
            commit_version: CommitVersion::new(commit_version),
            bounds: PrefixBounds::new(vec![0x01], vec![0x02]),
            scope_identity: ScopeIdentity::from_bytes(SCOPE),
            manifest_digest: ManifestDigest::from_bytes(MANIFEST),
            policy_version: PolicyVersion::from_bytes(POLICY),
        },
        records,
    )
}

pub fn snapshot(commit_version: u64, pages: Vec<ScopeSnapshotPage>) -> ScopeSnapshot {
    ScopeSnapshot::new(
        ScopeIdentity::from_bytes(SCOPE),
        vector(commit_version),
        vector(commit_version),
        pages,
    )
}

pub fn two_row_snapshot(commit_version: u64) -> ScopeSnapshot {
    snapshot(
        commit_version,
        vec![page(
            1,
            commit_version,
            vec![row("alice", "red green", 1), row("bob", "green blue", 1)],
        )],
    )
}

pub fn two_page_snapshot(commit_version: u64) -> ScopeSnapshot {
    snapshot(
        commit_version,
        vec![
            page(1, commit_version, vec![row("alice", "red green", 1)]),
            page(2, commit_version, vec![row("bob", "green blue", 1)]),
        ],
    )
}

pub struct GoldenFile {
    pub contract: String,
    pub cases: Vec<GoldenCase>,
}

pub struct GoldenCase {
    pub name: String,
    pub expected: Value,
}

pub fn load(text: &str) -> GoldenFile {
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    GoldenFile {
        contract: value["contract"]
            .as_str()
            .expect("the golden file names a contract")
            .to_owned(),
        cases: value["cases"]
            .as_array()
            .expect("the golden file has a case list")
            .iter()
            .map(|case| GoldenCase {
                name: case["name"]
                    .as_str()
                    .expect("the case names a vector")
                    .to_owned(),
                expected: case["expected"].clone(),
            })
            .collect(),
    }
}

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

pub fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).expect("a hex digit");
            let low = (pair[1] as char).to_digit(16).expect("a hex digit");
            u8::try_from((high << 4) | low).expect("one byte")
        })
        .collect()
}

pub fn golden_rows() -> BTreeMap<Vec<u8>, oxkv_replica::RowEntry> {
    let mut rows = BTreeMap::new();
    for (name, value) in [
        ("alice", "red green"),
        ("bob", "green blue"),
        ("carol", "green green"),
    ] {
        rows.insert(
            key(name),
            oxkv_replica::RowEntry {
                key_version: oxkv_contracts::version::KeyVersion::new(1),
                commit_version: CommitVersion::new(5),
                value: Some(value.as_bytes().to_vec()),
                page_id: None,
            },
        );
    }
    rows
}
