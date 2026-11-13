//! The snapshot content digest and content seal.
//!
//! The content digest covers the directory integrity value and every page
//! integrity value. It never covers the authority lease term.
//!
//! The content seal covers the content digest and the lease term. A reader
//! verifies the content digest before it verifies the seal. A sealed snapshot
//! stays readable after a lease term change.

use oxkv_contracts::version::{ContentDigest, ContentSeal, LeaseTerm};

use crate::directory::Directory;
use crate::error::SnapshotError;

/// The `BLAKE3` domain tag for the content digest.
pub const CONTENT_DIGEST_TAG: &str = "oxkv/snapshot-content/v1";

/// Computes the content digest of a directory and its page digests.
#[must_use]
pub fn content_digest_for(directory: &Directory) -> ContentDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(CONTENT_DIGEST_TAG.as_bytes());
    hasher.update(&[0]);
    hasher.update(directory.integrity.as_bytes());
    for entry in &directory.entries {
        hasher.update(&entry.integrity.to_be_bytes());
    }
    ContentDigest::from_bytes(*hasher.finalize().as_bytes())
}

/// Verifies the content digest.
///
/// # Errors
///
/// Returns [`SnapshotError::CorruptPage`] when the expected digest does not
/// match the computed digest.
pub fn verify_content_digest(
    expected: ContentDigest,
    computed: ContentDigest,
    offset: u64,
) -> Result<(), SnapshotError> {
    if expected != computed {
        return Err(SnapshotError::CorruptPage {
            page_id: oxkv_contracts::version::PageId::from_bytes([0; 32]),
            offset,
            reason: "the content digest does not match",
        });
    }
    Ok(())
}

/// Verifies the content seal after the content digest.
///
/// # Errors
///
/// Returns [`SnapshotError::StaleSeal`] when the seal does not cover the
/// content digest or when the seal lease term is not the header lease term.
pub fn verify_seal(
    seal: ContentSeal,
    computed: ContentDigest,
    lease_term: LeaseTerm,
    offset: u64,
) -> Result<(), SnapshotError> {
    if !seal.covers(computed) || seal.lease_term() != lease_term {
        return Err(SnapshotError::StaleSeal { offset });
    }
    Ok(())
}
