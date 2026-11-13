//! Content seals and artifact verification.
//!
//! A content digest covers artifact bytes only. A content seal covers the
//! content digest and the authority lease term. The lease term never enters the
//! content digest.
//!
//! A reader verifies the content digest before the seal. A failed checksum
//! never serves the artifact. A sealed artifact stays readable across a lease
//! change.
//!
//! A write path checks the seal term against the writer term after the content
//! check. A valid content digest with a stale seal fails with
//! [`TierError::StaleEpoch`].

use oxkv_contracts::version::{ContentDigest, ContentSeal, LeaseTerm};

use crate::error::TierError;

/// An artifact and the seal that covers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedContent {
    bytes: Vec<u8>,
    digest: ContentDigest,
    seal: ContentSeal,
}

impl SealedContent {
    /// Seals artifact bytes under one authority lease term.
    #[must_use]
    pub fn seal(bytes: Vec<u8>, lease_term: LeaseTerm) -> Self {
        let digest = ContentDigest::of_bytes(&bytes);
        let seal = ContentSeal::new(digest, lease_term, None);
        Self {
            bytes,
            digest,
            seal,
        }
    }

    /// Creates a sealed artifact from decoded parts.
    ///
    /// Use this function after a reader decodes a header. The verifier then
    /// checks the bytes against the claimed digest and seal.
    #[must_use]
    pub const fn from_parts(bytes: Vec<u8>, digest: ContentDigest, seal: ContentSeal) -> Self {
        Self {
            bytes,
            digest,
            seal,
        }
    }

    /// Returns the artifact bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the content digest.
    #[must_use]
    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }

    /// Returns the content seal.
    #[must_use]
    pub const fn content_seal(&self) -> ContentSeal {
        self.seal
    }

    /// Returns the sealed lease term.
    #[must_use]
    pub const fn lease_term(&self) -> LeaseTerm {
        self.seal.lease_term()
    }

    /// Computes the content digest from the current bytes.
    #[must_use]
    pub fn compute_digest(&self) -> ContentDigest {
        ContentDigest::of_bytes(&self.bytes)
    }

    /// Verifies the content digest before any seal check.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::ChecksumMismatch`] when the bytes do not match the
    /// claimed digest.
    pub fn verify_content(&self) -> Result<(), TierError> {
        if self.compute_digest() == self.digest {
            Ok(())
        } else {
            Err(TierError::ChecksumMismatch)
        }
    }

    /// Verifies the content digest and the seal for a read.
    ///
    /// A seal from an older lease term stays valid. The function does not
    /// compare the seal term with the current authority term.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::ChecksumMismatch`] when the content digest fails or
    /// the seal does not cover the digest.
    pub fn verify_read(&self) -> Result<(), TierError> {
        self.verify_content()?;
        if self.seal.covers(self.digest) {
            Ok(())
        } else {
            Err(TierError::ChecksumMismatch)
        }
    }

    /// Verifies an artifact for a write under one lease term.
    ///
    /// The content check runs before the seal check. A valid digest with a
    /// stale seal returns [`TierError::StaleEpoch`].
    ///
    /// # Errors
    ///
    /// Returns [`TierError::ChecksumMismatch`] when the content digest fails.
    /// Returns [`TierError::StaleEpoch`] when the seal term differs from the
    /// writer term.
    pub fn verify_for_write(&self, writer_term: LeaseTerm) -> Result<(), TierError> {
        self.verify_read()?;
        if self.seal.lease_term() == writer_term {
            Ok(())
        } else {
            Err(TierError::StaleEpoch)
        }
    }
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{ContentDigest, ContentSeal, LeaseTerm};

    use super::{SealedContent, TierError};

    #[test]
    fn the_content_digest_ignores_the_lease_term() {
        let first = SealedContent::seal(b"page-bytes".to_vec(), LeaseTerm::new(1));
        let second = SealedContent::seal(b"page-bytes".to_vec(), LeaseTerm::new(2));
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.bytes(), second.bytes());
        assert_ne!(first.lease_term(), second.lease_term());
    }

    #[test]
    fn a_sealed_artifact_survives_a_lease_change() {
        let sealed = SealedContent::seal(b"page-bytes".to_vec(), LeaseTerm::new(1));
        assert_eq!(sealed.verify_read(), Ok(()));
        let new_leader_term = LeaseTerm::new(2);
        assert_ne!(sealed.lease_term(), new_leader_term);
        assert_eq!(sealed.verify_read(), Ok(()));
        assert_eq!(sealed.bytes(), b"page-bytes");
    }

    #[test]
    fn a_stale_seal_fails_after_a_valid_digest() {
        let sealed = SealedContent::seal(b"page-bytes".to_vec(), LeaseTerm::new(1));
        assert_eq!(sealed.verify_content(), Ok(()));
        assert_eq!(
            sealed.verify_for_write(LeaseTerm::new(2)),
            Err(TierError::StaleEpoch)
        );
    }

    #[test]
    fn a_failed_checksum_does_not_serve_the_artifact() {
        let digest = ContentDigest::of_bytes(b"expected");
        let seal = ContentSeal::new(digest, LeaseTerm::new(1), None);
        let sealed = SealedContent::from_parts(b"corrupt".to_vec(), digest, seal);
        assert_eq!(sealed.verify_read(), Err(TierError::ChecksumMismatch));
        assert_eq!(
            sealed.verify_for_write(LeaseTerm::new(2)),
            Err(TierError::ChecksumMismatch)
        );
    }

    #[test]
    fn a_seal_that_covers_another_digest_fails() {
        let digest = ContentDigest::of_bytes(b"page-bytes");
        let other = ContentDigest::of_bytes(b"other-bytes");
        let seal = ContentSeal::new(other, LeaseTerm::new(1), None);
        let sealed = SealedContent::from_parts(b"page-bytes".to_vec(), digest, seal);
        assert_eq!(sealed.verify_read(), Err(TierError::ChecksumMismatch));
    }
}
