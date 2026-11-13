//! Version vector helpers.
//!
//! The contract exposes an immutable [`ScopeVersionVector`]. These helpers
//! rebuild a vector with one new entry. They also compute the version lag. The
//! applied vector never decreases.

use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId};

/// Sets one shard entry in a version vector.
pub(crate) fn set_version(
    target: &mut ScopeVersionVector,
    shard_id: ShardId,
    commit_version: CommitVersion,
) {
    let mut vector = target.as_vector().clone();
    vector.set(shard_id, commit_version);
    *target = ScopeVersionVector::from_vector(vector);
}

/// Merges one version vector into another with a maximum per shard.
pub(crate) fn merge_max(target: &mut ScopeVersionVector, source: &ScopeVersionVector) {
    for (shard_id, commit_version) in source.as_vector().iter() {
        if target
            .get(shard_id)
            .is_none_or(|current| commit_version > current)
        {
            set_version(target, shard_id.clone(), commit_version);
        }
    }
}

/// Returns the maximum version in a version vector.
pub(crate) fn max_version(versions: &ScopeVersionVector) -> u64 {
    versions
        .as_vector()
        .iter()
        .map(|(_, commit_version)| commit_version.get())
        .max()
        .unwrap_or(0)
}

/// Returns the maximum version lag across the shards.
#[must_use]
pub fn version_lag(applied: &ScopeVersionVector, watermark: &ScopeVersionVector) -> u64 {
    watermark
        .as_vector()
        .iter()
        .map(|(shard_id, watermark_version)| {
            watermark_version
                .get()
                .saturating_sub(applied.get(shard_id).map_or(0, CommitVersion::get))
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{CommitVersion, ScopeVersionVector, ShardId, VersionVector};

    use super::{merge_max, set_version, version_lag};

    fn vector(entries: &[(&[u8], u64)]) -> ScopeVersionVector {
        ScopeVersionVector::from_vector(VersionVector::from_entries(
            entries.iter().map(|(shard, version)| {
                (ShardId::new(shard.to_vec()), CommitVersion::new(*version))
            }),
        ))
    }

    #[test]
    fn an_entry_is_set_without_a_borrow_of_the_target() {
        let mut vector = ScopeVersionVector::new();
        set_version(&mut vector, ShardId::new(vec![1]), CommitVersion::new(7));
        assert_eq!(
            vector.get(&ShardId::new(vec![1])),
            Some(CommitVersion::new(7))
        );
    }

    #[test]
    fn the_lag_is_the_maximum_shard_distance() {
        let applied = vector(&[(&[1], 10), (&[2], 20)]);
        let watermark = vector(&[(&[1], 13), (&[2], 20), (&[3], 4)]);
        assert_eq!(version_lag(&applied, &watermark), 4);
        assert_eq!(version_lag(&watermark, &applied), 0);
    }

    #[test]
    fn a_merge_never_lowers_an_entry() {
        let mut target = vector(&[(&[1], 10)]);
        merge_max(&mut target, &vector(&[(&[1], 8), (&[2], 4)]));
        assert_eq!(
            target.get(&ShardId::new(vec![1])),
            Some(CommitVersion::new(10))
        );
        assert_eq!(
            target.get(&ShardId::new(vec![2])),
            Some(CommitVersion::new(4))
        );
    }
}
