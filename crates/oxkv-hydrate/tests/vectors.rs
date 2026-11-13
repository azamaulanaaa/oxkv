//! Golden vectors for the page and snapshot formats.
//!
//! The vectors pin the canonical bytes, the page identity, the page digest, and
//! the snapshot content digest. A format change must update the vector on
//! purpose.

use oxkv_contracts::version::{
    CommitVersion, LeaseTerm, PrefixBounds, ScopeVersionVector, ShardId, VersionVector,
};
use oxkv_hydrate::{SealedSnapshot, SnapshotDraft, SnapshotPage};

const GOLDEN: &str = include_str!("vectors/golden.json");

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

fn sample() -> SealedSnapshot {
    let shard = ShardId::new(vec![1]);
    let bounds = PrefixBounds::new(vec![b'a'], vec![b'z']);
    let first = SnapshotPage::record_page(
        0,
        shard.clone(),
        CommitVersion::new(7),
        PrefixBounds::new(vec![b'a'], vec![b'm']),
        &[
            (b"alpha".to_vec(), b"one".to_vec()),
            (b"bravo".to_vec(), b"two".to_vec()),
        ],
    );
    let second = SnapshotPage::record_page(
        1,
        shard.clone(),
        CommitVersion::new(7),
        PrefixBounds::new(vec![b'm'], vec![b'z']),
        &[(b"xray".to_vec(), b"three".to_vec())],
    );
    let mut draft = SnapshotDraft::sample(bounds, LeaseTerm::new(3));
    draft.commit_version = CommitVersion::new(7);
    draft.version_vector = ScopeVersionVector::from_vector(VersionVector::from_entries([(
        shard,
        CommitVersion::new(7),
    )]));
    draft
        .seal(vec![first, second])
        .expect("the sample snapshot seals")
}

#[test]
fn the_page_vector_matches() {
    let golden: serde_json::Value =
        serde_json::from_str(GOLDEN).expect("the golden vector is valid JSON");
    let snapshot = sample();
    let page = &snapshot.pages[0];
    assert_eq!(hex(&page.encode()), golden["page"].as_str().expect("page"));
    assert_eq!(
        hex(page.page_id.as_bytes()),
        golden["page_id"].as_str().expect("page id")
    );
    assert_eq!(
        hex(page.digest().as_bytes()),
        golden["page_digest"].as_str().expect("page digest")
    );
    let decoded = SnapshotPage::decode(&page.encode()).expect("the page vector decodes");
    assert_eq!(decoded, *page);
}

#[test]
#[ignore = "regenerates the golden vector file"]
fn generate_vectors() {
    assert_eq!(
        std::env::var("OXKV_UPDATE_GOLDEN").as_deref(),
        Ok("1"),
        "set OXKV_UPDATE_GOLDEN=1 to rewrite the golden vector"
    );
    let snapshot = sample();
    let page = &snapshot.pages[0];
    let page_digests = snapshot
        .directory
        .iter()
        .flat_map(|entry| entry.page_digest.as_bytes().to_vec())
        .collect::<Vec<_>>();
    let value = serde_json::json!({
        "page": hex(&page.encode()),
        "page_id": hex(page.page_id.as_bytes()),
        "page_digest": hex(page.digest().as_bytes()),
        "snapshot": hex(&snapshot.to_bytes()),
        "content_digest": hex(snapshot.header.content_digest.as_bytes()),
        "page_digests": hex(&page_digests),
    });
    let text = serde_json::to_string_pretty(&value).expect("the vector serializes");
    let path = format!("{}/tests/vectors/golden.json", env!("CARGO_MANIFEST_DIR"));
    std::fs::write(&path, format!("{text}\n"))
        .unwrap_or_else(|error| panic!("cannot write the golden file {path}: {error}"));
}

#[test]
fn the_snapshot_vector_matches() {
    let golden: serde_json::Value =
        serde_json::from_str(GOLDEN).expect("the golden vector is valid JSON");
    let snapshot = sample();
    assert_eq!(
        hex(&snapshot.to_bytes()),
        golden["snapshot"].as_str().expect("snapshot")
    );
    assert_eq!(
        hex(snapshot.header.content_digest.as_bytes()),
        golden["content_digest"].as_str().expect("content digest")
    );
    let page_digests = snapshot
        .directory
        .iter()
        .flat_map(|entry| entry.page_digest.as_bytes().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(
        hex(&page_digests),
        golden["page_digests"].as_str().expect("page digests")
    );
    let decoded =
        SealedSnapshot::decode(&snapshot.to_bytes()).expect("the snapshot vector decodes");
    assert_eq!(decoded.to_bytes(), snapshot.to_bytes());
    assert_eq!(decoded.pages, snapshot.pages);
}
