#![allow(missing_docs)]

//! Shared support for the contract golden vectors.

use std::collections::BTreeMap;

use oxkv_contracts::cursor::{
    CostCursor, CostFrontier, CursorType, EventCursor, EventPosition, EventSequence,
    HydrationCursor, KeyCursor, ProjectionCursor, QueryFingerprint, ReceiptCursor, ReceiptSequence,
    ScopeGeneration, SearchCursor, SortTuple, SortValue, SyncCursor,
};
use oxkv_contracts::receipt::ReceiptId;
use oxkv_contracts::version::{
    CommitVersion, ContentDigest, FactSourceId, FactVersion, Hash32, PageSequence, ProjectionEpoch,
    ProjectionWatermarks, ScopeVersionVector, ShardId, SnapshotIdentity, SnapshotVersion,
    VersionVector,
};
use oxkv_contracts::{CursorDirection, CursorToken, TableName};
use serde_json::Value;

/// One golden file.
pub(crate) struct GoldenFile {
    pub(crate) contract: String,
    pub(crate) cases: Vec<GoldenCase>,
}

/// One golden case.
pub(crate) struct GoldenCase {
    pub(crate) name: String,
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
            expected: case["expected"].clone(),
        })
        .collect();
    GoldenFile { contract, cases }
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

pub(crate) fn scope_vector(entries: &[(&[u8], u64)]) -> ScopeVersionVector {
    ScopeVersionVector::from_vector(VersionVector::from_entries(
        entries
            .iter()
            .map(|(shard, version)| (ShardId::new(shard.to_vec()), CommitVersion::new(*version))),
    ))
}

pub(crate) fn key_cursor() -> KeyCursor {
    KeyCursor::new(
        TableName::parse_user("accounts").expect("the table name is valid"),
        SnapshotVersion::new(18),
        vec![b'k', 1],
        CursorDirection::Ascending,
    )
}

pub(crate) fn sync_cursor() -> SyncCursor {
    SyncCursor::new(
        ScopeGeneration::new(3),
        scope_vector(&[(&[1], 18), (&[2], 9)]),
        scope_vector(&[(&[1], 4)]),
        Hash32::from_bytes([7; 32]),
    )
}

pub(crate) fn hydration_cursor() -> HydrationCursor {
    HydrationCursor::new(
        SnapshotIdentity::new(
            ShardId::new(vec![1]),
            CommitVersion::new(18),
            ContentDigest::from_bytes([8; 32]),
        ),
        PageSequence::new(2),
        ContentDigest::from_bytes([9; 32]),
    )
}

pub(crate) fn receipt_cursor() -> ReceiptCursor {
    ReceiptCursor::new(ReceiptId::from_bytes([10; 32]), ReceiptSequence::new(5))
}

pub(crate) fn event_cursor() -> EventCursor {
    let mut entries = BTreeMap::new();
    entries.insert(vec![b'a'], EventSequence::new(4));
    entries.insert(vec![b'b'], EventSequence::new(7));
    EventCursor::new(EventPosition::PerKey(entries))
}

pub(crate) fn search_cursor() -> SearchCursor {
    SearchCursor::new(
        scope_vector(&[(&[1], 18)]),
        QueryFingerprint::from_bytes([11; 32]),
        SortTuple::new(vec![
            SortValue::Integer(42),
            SortValue::Text("alpha".to_owned()),
            SortValue::Bytes(vec![1, 2, 3]),
        ]),
    )
}

pub(crate) fn projection_cursor() -> ProjectionCursor {
    let mut watermarks = ProjectionWatermarks::new();
    watermarks.set(FactSourceId::new("accounts"), FactVersion::new(12));
    ProjectionCursor::new(watermarks, ProjectionEpoch::new(41))
}

pub(crate) fn cost_cursor() -> CostCursor {
    let mut frontiers = BTreeMap::new();
    frontiers.insert(ShardId::new(vec![1]), CostFrontier::new(10));
    frontiers.insert(ShardId::new(vec![2]), CostFrontier::new(20));
    CostCursor::new(frontiers, scope_vector(&[(&[1], 18), (&[2], 9)]), 5)
}

/// Returns every sample cursor type and token.
pub(crate) fn cursor_tokens() -> [(CursorType, Vec<u8>); 8] {
    [
        (CursorType::Key, key_cursor().to_token()),
        (CursorType::Sync, sync_cursor().to_token()),
        (CursorType::Hydration, hydration_cursor().to_token()),
        (CursorType::Receipt, receipt_cursor().to_token()),
        (CursorType::Event, event_cursor().to_token()),
        (CursorType::Search, search_cursor().to_token()),
        (CursorType::Projection, projection_cursor().to_token()),
        (CursorType::Cost, cost_cursor().to_token()),
    ]
}

/// Decodes one token as the given cursor type.
pub(crate) fn decode_for(
    cursor_type: CursorType,
    token: &[u8],
) -> Result<(), oxkv_contracts::CursorError> {
    match cursor_type {
        CursorType::Key => KeyCursor::from_token(token).map(|_| ()),
        CursorType::Sync => SyncCursor::from_token(token).map(|_| ()),
        CursorType::Hydration => HydrationCursor::from_token(token).map(|_| ()),
        CursorType::Receipt => ReceiptCursor::from_token(token).map(|_| ()),
        CursorType::Event => EventCursor::from_token(token).map(|_| ()),
        CursorType::Search => SearchCursor::from_token(token).map(|_| ()),
        CursorType::Projection => ProjectionCursor::from_token(token).map(|_| ()),
        CursorType::Cost => CostCursor::from_token(token).map(|_| ()),
    }
}
