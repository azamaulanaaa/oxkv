//! The released vector corpus must not change.
//!
//! The test reads the checked-in manifest. It computes the `BLAKE3` digest of
//! every vector file. It never regenerates a vector.

use std::path::PathBuf;

use oxkv_qualify::register::REGISTER;
use oxkv_qualify::vectors::parse_manifest;
use oxkv_qualify::vectors::parse_vector_file;

/// The digest of the manifest bytes at release.
const RELEASED_MANIFEST_DIGEST: &str =
    "14ec80681f598feab948b63e076e8508d844e3364f81beb46a810bca41240e79";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_released_manifest_is_unchanged() {
    let bytes =
        std::fs::read(root().join("vectors/manifest.json")).expect("the manifest is readable");
    let digest = oxkv_qualify::canonical::hex(blake3::hash(&bytes).as_bytes());
    assert_eq!(
        digest, RELEASED_MANIFEST_DIGEST,
        "the vector manifest changed. A released vector cannot change."
    );
}

#[test]
fn every_released_vector_keeps_its_digest() {
    let text = std::fs::read_to_string(root().join("vectors/manifest.json"))
        .expect("the manifest is readable");
    let manifest = parse_manifest(&text).expect("the manifest parses");
    assert_eq!(manifest.vector_version, "qualification-vectors/v1");
    assert_eq!(manifest.entries.len(), REGISTER.len());
    for entry in &manifest.entries {
        assert!(entry.released, "the vector {} is not released", entry.id);
        let bytes = std::fs::read(root().join(&entry.path))
            .unwrap_or_else(|_| panic!("the vector file {} is missing", entry.path));
        let digest = oxkv_qualify::canonical::hex(blake3::hash(&bytes).as_bytes());
        assert_eq!(
            digest, entry.digest,
            "the released vector {} changed. A break needs a new contract version.",
            entry.id
        );
    }
}

#[test]
fn every_register_row_has_one_released_file() {
    let text = std::fs::read_to_string(root().join("vectors/manifest.json"))
        .expect("the manifest is readable");
    let manifest = parse_manifest(&text).expect("the manifest parses");
    for row in &REGISTER {
        let entry = manifest
            .entry(row.vector_id)
            .unwrap_or_else(|| panic!("the row {} has no vector entry", row.id));
        assert_eq!(entry.contract, row.id);
        assert!(entry.path.ends_with(&format!("{}.json", row.vector_id)));
        let file_text =
            std::fs::read_to_string(root().join(&entry.path)).expect("the vector file is readable");
        let file = parse_vector_file(&file_text).expect("the vector file parses");
        assert_eq!(file.contract, row.id);
        assert_eq!(file.version, row.version);
        assert!(!file.cases.is_empty(), "the row {} has no case", row.id);
        for case in &file.cases {
            assert_ne!(case.id, "");
        }
    }
}

#[test]
fn the_manifest_has_no_duplicate_vector() {
    let text = std::fs::read_to_string(root().join("vectors/manifest.json"))
        .expect("the manifest is readable");
    let manifest = parse_manifest(&text).expect("the manifest parses");
    for (index, left) in manifest.entries.iter().enumerate() {
        for right in &manifest.entries[index + 1..] {
            assert_ne!(left.id, right.id);
            assert_ne!(left.path, right.path);
        }
    }
}
