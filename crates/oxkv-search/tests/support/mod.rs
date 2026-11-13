#![allow(missing_docs)]
#![allow(dead_code)]

//! Shared support for the search tests and golden vectors.

use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId, VersionVector};
use oxkv_search::{QueryFingerprint, SearchHit, SearchPin};

/// Builds a scope version vector from shard byte and version pairs.
pub(crate) fn vector(entries: &[(u8, u64)]) -> ScopeVersionVector {
    ScopeVersionVector::from_vector(VersionVector::from_entries(
        entries
            .iter()
            .map(|(shard, version)| (ShardId::new(vec![*shard]), CommitVersion::new(*version))),
    ))
}

/// Builds a shard identifier from one byte.
pub(crate) fn shard(byte: u8) -> ShardId {
    ShardId::new(vec![byte])
}

/// Builds a search pin with one fingerprint byte.
pub(crate) fn pin(entries: &[(u8, u64)], fingerprint: u8) -> SearchPin {
    SearchPin::new(
        vector(entries),
        QueryFingerprint::from_bytes([fingerprint; 32]),
    )
}

/// Builds a relevance hit.
pub(crate) fn hit(shard_byte: u8, key: &[u8], score: i64, commit_version: u64) -> SearchHit {
    SearchHit::new(
        key.to_vec(),
        score,
        shard(shard_byte),
        CommitVersion::new(commit_version),
    )
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
