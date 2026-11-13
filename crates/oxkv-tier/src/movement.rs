//! Tier movement and eviction.
//!
//! A move copies the exact bytes to the target tier, verifies the target
//! checksum, and only then removes the source copy. The artifact identity and
//! the byte order stay the same.
//!
//! An eviction that would remove the only verified copy returns
//! [`TierError::OnlyCopyEviction`] and changes no tier.

use oxkv_contracts::version::ContentDigest;

use crate::error::TierError;
use crate::tier::{ArtifactId, Tier, TierStack};

/// The result of one artifact move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Movement {
    artifact: ArtifactId,
    source: Tier,
    target: Tier,
    content_digest: ContentDigest,
    byte_len: usize,
}

impl Movement {
    /// Creates a movement receipt.
    #[must_use]
    pub const fn new(
        artifact: ArtifactId,
        source: Tier,
        target: Tier,
        content_digest: ContentDigest,
        byte_len: usize,
    ) -> Self {
        Self {
            artifact,
            source,
            target,
            content_digest,
            byte_len,
        }
    }

    /// Returns the artifact identity.
    #[must_use]
    pub const fn artifact(&self) -> &ArtifactId {
        &self.artifact
    }

    /// Returns the source tier.
    #[must_use]
    pub const fn source(&self) -> Tier {
        self.source
    }

    /// Returns the target tier.
    #[must_use]
    pub const fn target(&self) -> Tier {
        self.target
    }

    /// Returns the verified content digest.
    #[must_use]
    pub const fn content_digest(&self) -> ContentDigest {
        self.content_digest
    }

    /// Returns the artifact length in bytes.
    #[must_use]
    pub const fn byte_len(&self) -> usize {
        self.byte_len
    }
}

impl TierStack {
    /// Moves one artifact to a target tier.
    ///
    /// The copy sequence is read, verify, write, verify, and remove. The source
    /// copy stays in place when the target does not confirm the checksum.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::TierUnavailable`] when the target does not answer
    /// or no available tier holds the artifact.
    /// Returns [`TierError::ChecksumMismatch`] when a source or target digest
    /// does not match.
    pub fn move_artifact(
        &mut self,
        artifact: &ArtifactId,
        digest: ContentDigest,
        target: Tier,
    ) -> Result<Movement, TierError> {
        if !self.status(target).is_available() {
            return Err(TierError::TierUnavailable);
        }
        let source = self.find_copy(artifact)?;
        let bytes = self.read_from(artifact, source)?;
        if ContentDigest::of_bytes(&bytes) != digest {
            return Err(TierError::ChecksumMismatch);
        }
        let byte_len = bytes.len();
        if source == target {
            return Ok(Movement::new(
                artifact.clone(),
                source,
                target,
                digest,
                byte_len,
            ));
        }
        let Some(driver) = self.driver_mut(target) else {
            return Err(TierError::TierUnavailable);
        };
        driver.put(artifact.clone(), bytes);
        let target_bytes = self
            .driver(target)
            .and_then(|driver| driver.get(artifact))
            .ok_or(TierError::TierUnavailable)?;
        if ContentDigest::of_bytes(&target_bytes) != digest {
            if let Some(driver) = self.driver_mut(target) {
                driver.remove(artifact);
            }
            return Err(TierError::ChecksumMismatch);
        }
        if let Some(driver) = self.driver_mut(source) {
            driver.remove(artifact);
        }
        Ok(Movement::new(
            artifact.clone(),
            source,
            target,
            digest,
            byte_len,
        ))
    }

    /// Evicts one artifact from a tier.
    ///
    /// The function refuses the eviction when no colder available tier holds a
    /// verified copy. A hot copy alone is not enough to release a pin.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::TierUnavailable`] when the tier does not answer.
    /// Returns [`TierError::ArtifactMissing`] when the tier has no artifact.
    /// Returns [`TierError::ChecksumMismatch`] when the copy is corrupt.
    /// Returns [`TierError::OnlyCopyEviction`] when no colder verified copy
    /// exists.
    pub fn evict(
        &mut self,
        artifact: &ArtifactId,
        digest: ContentDigest,
        tier: Tier,
    ) -> Result<(), TierError> {
        let bytes = self.read_from(artifact, tier)?;
        if ContentDigest::of_bytes(&bytes) != digest {
            return Err(TierError::ChecksumMismatch);
        }
        let has_colder_copy = Tier::ALL.iter().copied().any(|other| {
            other.is_colder_than(tier) && self.has_verified_copy(artifact, other, digest)
        });
        if !has_colder_copy {
            return Err(TierError::OnlyCopyEviction);
        }
        if let Some(driver) = self.driver_mut(tier) {
            driver.remove(artifact);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{ContentDigest, PageSequence, ShardId};

    use crate::error::TierError;
    use crate::tier::{ArtifactId, Tier, TierStack};

    fn artifact() -> ArtifactId {
        ArtifactId::new(ShardId::new(vec![1]), 4, PageSequence::new(1))
    }

    fn page(sequence: u64) -> ArtifactId {
        ArtifactId::new(ShardId::new(vec![1]), 4, PageSequence::new(sequence))
    }

    fn stack_with(tiers: &[Tier], bytes: &[u8]) -> TierStack {
        let mut stack = TierStack::memory();
        for tier in tiers {
            let driver = stack
                .driver_mut(*tier)
                .expect("the memory stack has every tier");
            driver.put(artifact(), bytes.to_vec());
        }
        stack
    }

    #[test]
    fn a_move_preserves_identity_and_bytes() {
        let bytes = b"page-bytes";
        let digest = ContentDigest::of_bytes(bytes);
        let mut stack = stack_with(&[Tier::Disk], bytes);
        let movement = stack
            .move_artifact(&artifact(), digest, Tier::Object)
            .expect("the move succeeds");
        assert_eq!(movement.artifact(), &artifact());
        assert_eq!(movement.source(), Tier::Disk);
        assert_eq!(movement.target(), Tier::Object);
        assert_eq!(movement.byte_len(), bytes.len());
        assert_eq!(
            stack.read_from(&artifact(), Tier::Object),
            Ok(bytes.to_vec())
        );
        assert_eq!(
            stack.read_from(&artifact(), Tier::Disk),
            Err(TierError::ArtifactMissing)
        );
    }

    #[test]
    fn a_move_to_an_unavailable_tier_keeps_the_source() {
        let bytes = b"page-bytes";
        let digest = ContentDigest::of_bytes(bytes);
        let mut stack = stack_with(&[Tier::Disk], bytes);
        stack.set_status(Tier::Object, crate::tier::TierStatus::Unavailable);
        assert_eq!(
            stack.move_artifact(&artifact(), digest, Tier::Object),
            Err(TierError::TierUnavailable)
        );
        assert_eq!(stack.read_from(&artifact(), Tier::Disk), Ok(bytes.to_vec()));
    }

    #[test]
    fn an_only_copy_eviction_is_refused() {
        let bytes = b"page-bytes";
        let digest = ContentDigest::of_bytes(bytes);
        let mut stack = stack_with(&[Tier::Disk], bytes);
        assert_eq!(
            stack.evict(&artifact(), digest, Tier::Disk),
            Err(TierError::OnlyCopyEviction)
        );
        assert_eq!(stack.read_from(&artifact(), Tier::Disk), Ok(bytes.to_vec()));
    }

    #[test]
    fn an_eviction_with_a_second_verified_copy_succeeds() {
        let bytes = b"page-bytes";
        let digest = ContentDigest::of_bytes(bytes);
        let mut stack = stack_with(&[Tier::Ram, Tier::Object], bytes);
        assert_eq!(stack.evict(&artifact(), digest, Tier::Ram), Ok(()));
        assert_eq!(
            stack.read_from(&artifact(), Tier::Object),
            Ok(bytes.to_vec())
        );
    }

    #[test]
    fn a_hot_copy_does_not_release_a_disk_pin() {
        let bytes = b"page-bytes";
        let digest = ContentDigest::of_bytes(bytes);
        let mut stack = stack_with(&[Tier::Ram, Tier::Disk], bytes);
        assert_eq!(
            stack.evict(&artifact(), digest, Tier::Disk),
            Err(TierError::OnlyCopyEviction)
        );
        assert_eq!(stack.read_from(&artifact(), Tier::Disk), Ok(bytes.to_vec()));
    }

    #[test]
    fn a_corrupt_source_is_never_moved() {
        let bytes = b"page-bytes";
        let digest = ContentDigest::of_bytes(b"other-bytes");
        let mut stack = stack_with(&[Tier::Disk], bytes);
        assert_eq!(
            stack.move_artifact(&artifact(), digest, Tier::Object),
            Err(TierError::ChecksumMismatch)
        );
        assert_eq!(stack.read_from(&artifact(), Tier::Disk), Ok(bytes.to_vec()));
    }

    #[test]
    fn a_read_falls_back_from_an_unavailable_hot_tier() {
        let bytes = b"page-bytes";
        let mut stack = stack_with(&[Tier::Object], bytes);
        stack.set_status(Tier::Ram, crate::tier::TierStatus::Unavailable);
        assert_eq!(
            stack.read_through(&artifact()),
            Ok((Tier::Object, bytes.to_vec()))
        );
    }

    #[test]
    fn a_move_keeps_the_page_order() {
        let mut stack = TierStack::memory();
        for sequence in 1..=3 {
            let bytes = [b'p', u8::try_from(sequence).expect("the page fits")];
            stack
                .put(Tier::Disk, page(sequence), bytes.to_vec())
                .expect("the disk tier accepts the page");
            stack
                .move_artifact(
                    &page(sequence),
                    ContentDigest::of_bytes(&bytes),
                    Tier::Object,
                )
                .expect("the move succeeds");
        }
        for sequence in 1..=3 {
            let expected = [b'p', u8::try_from(sequence).expect("the page fits")];
            assert_eq!(
                stack.read_from(&page(sequence), Tier::Object),
                Ok(expected.to_vec())
            );
        }
    }
}
