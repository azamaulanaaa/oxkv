//! The scope version.
//!
//! A scope version is a version vector with one commit version per shard. One
//! scope read uses one vector. The version never decreases.

use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId, VersionVector};

use crate::error::ScopeError;

/// The version of one scope.
///
/// The type wraps a [`ScopeVersionVector`]. [`ScopeVersion::advance`] and
/// [`ScopeVersion::advance_to`] reject a decrease.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ScopeVersion {
    vector: ScopeVersionVector,
}

impl ScopeVersion {
    /// Creates an empty scope version.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a scope version from one vector.
    #[must_use]
    pub const fn from_vector(vector: ScopeVersionVector) -> Self {
        Self { vector }
    }

    /// Returns the inner version vector.
    #[must_use]
    pub const fn as_vector(&self) -> &ScopeVersionVector {
        &self.vector
    }

    /// Returns one shard entry.
    #[must_use]
    pub fn get(&self, shard_id: &ShardId) -> Option<CommitVersion> {
        self.vector.get(shard_id)
    }

    /// Returns the number of shard entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.vector.as_vector().len()
    }

    /// Returns true when the vector has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vector.is_empty()
    }

    /// Iterates over the entries in canonical shard order.
    pub fn iter(&self) -> impl Iterator<Item = (&ShardId, CommitVersion)> {
        self.vector.as_vector().iter()
    }

    /// Returns true when this version is at or above every entry of another.
    #[must_use]
    pub fn entails(&self, other: &ScopeVersionVector) -> bool {
        self.vector.as_vector().entails(other.as_vector())
    }

    /// Advances one shard entry.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::VersionRegression`] when the new commit version is
    /// below the current version.
    pub fn advance(
        &mut self,
        shard_id: ShardId,
        commit_version: CommitVersion,
    ) -> Result<(), ScopeError> {
        let mut target = self.vector.as_vector().clone();
        target.set(shard_id, commit_version);
        self.advance_to(&ScopeVersionVector::from_vector(target))
    }

    /// Replaces the vector with a target vector.
    ///
    /// The method checks every current entry. A missing target entry is a
    /// regression when the current version is above zero.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::VersionRegression`] when the target is below the
    /// current version for one shard.
    pub fn advance_to(&mut self, target: &ScopeVersionVector) -> Result<(), ScopeError> {
        for (shard_id, current) in self.vector.as_vector().iter() {
            let proposed = target.get(shard_id).unwrap_or(CommitVersion::new(0));
            if proposed < current {
                return Err(ScopeError::VersionRegression {
                    shard_id: shard_id.clone(),
                    current,
                    proposed,
                });
            }
        }
        self.vector = target.clone();
        Ok(())
    }

    /// Returns true when two vectors have the same entries and versions.
    #[must_use]
    pub fn same_as(&self, other: &ScopeVersionVector) -> bool {
        self.entails(other) && other.as_vector().entails(self.vector.as_vector())
    }
}

impl From<ScopeVersionVector> for ScopeVersion {
    fn from(vector: ScopeVersionVector) -> Self {
        Self::from_vector(vector)
    }
}

/// Builds a scope version vector from shard and version pairs.
#[must_use]
pub fn version_vector(
    entries: impl IntoIterator<Item = (ShardId, CommitVersion)>,
) -> ScopeVersionVector {
    ScopeVersionVector::from_vector(VersionVector::from_entries(entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(entries: &[(&[u8], u64)]) -> ScopeVersionVector {
        version_vector(entries.iter().map(|(shard, version)| {
            (
                ShardId::new((*shard).to_vec()),
                CommitVersion::new(*version),
            )
        }))
    }

    #[test]
    fn a_version_advances() {
        let mut scope_version = ScopeVersion::from_vector(vector(&[(&[1], 4), (&[2], 9)]));
        scope_version
            .advance(ShardId::new(vec![1]), CommitVersion::new(5))
            .expect("the version advances");
        assert_eq!(
            scope_version.get(&ShardId::new(vec![1])),
            Some(CommitVersion::new(5))
        );
        assert_eq!(
            scope_version.get(&ShardId::new(vec![2])),
            Some(CommitVersion::new(9))
        );
    }

    #[test]
    fn a_version_does_not_decrease() {
        let mut scope_version = ScopeVersion::from_vector(vector(&[(&[1], 4)]));
        assert_eq!(
            scope_version.advance(ShardId::new(vec![1]), CommitVersion::new(3)),
            Err(ScopeError::VersionRegression {
                shard_id: ShardId::new(vec![1]),
                current: CommitVersion::new(4),
                proposed: CommitVersion::new(3)
            })
        );
        assert_eq!(
            scope_version.get(&ShardId::new(vec![1])),
            Some(CommitVersion::new(4))
        );
    }

    #[test]
    fn a_missing_target_entry_is_a_regression() {
        let mut scope_version = ScopeVersion::from_vector(vector(&[(&[1], 4), (&[2], 9)]));
        assert!(scope_version.advance_to(&vector(&[(&[1], 4)])).is_err());
    }

    #[test]
    fn one_vector_entails_another() {
        let high = ScopeVersion::from_vector(vector(&[(&[1], 5), (&[2], 9)]));
        assert!(high.entails(&vector(&[(&[1], 4), (&[2], 9)])));
        assert!(!high.entails(&vector(&[(&[1], 6)])));
        assert!(!high.same_as(&vector(&[(&[1], 5)])));
    }
}
