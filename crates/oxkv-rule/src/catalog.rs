//! The content-addressed policy catalog.
//!
//! The catalog is one table. It stores each policy version as an immutable
//! record. A body change creates a new record. An existing record never
//! changes. A version stays resolvable while its generation is at or above the
//! audit retention floor.

use std::collections::BTreeMap;
use std::collections::btree_map::Iter;

use oxkv_contracts::keyspace::CATALOG_TABLE;
use oxkv_contracts::version::{PolicyVersion, ShardId};

use crate::error::RuleError;
use crate::policy::PolicyContent;

/// The oldest generation that the catalog must keep.
///
/// Generation `0` keeps every record. Generation `1` keeps every record that
/// the catalog created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct RetentionFloor(u64);

impl RetentionFloor {
    /// Creates a retention floor.
    #[must_use]
    pub const fn new(generation: u64) -> Self {
        Self(generation)
    }

    /// Returns the oldest retained generation.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.0
    }
}

/// One immutable policy catalog record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRecord {
    version: PolicyVersion,
    content: PolicyContent,
    canonical: Vec<u8>,
    generation: u64,
}

impl PolicyRecord {
    /// Returns the policy version.
    #[must_use]
    pub const fn version(&self) -> PolicyVersion {
        self.version
    }

    /// Returns the stored policy content.
    #[must_use]
    pub const fn content(&self) -> &PolicyContent {
        &self.content
    }

    /// Returns the canonical bytes that produced the version.
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }

    /// Returns the catalog generation of the record.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// The policy catalog of one catalog shard.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyCatalog {
    records: BTreeMap<PolicyVersion, PolicyRecord>,
    next_generation: u64,
    retention_floor: RetentionFloor,
}

impl<'a> IntoIterator for &'a PolicyCatalog {
    type Item = (&'a PolicyVersion, &'a PolicyRecord);
    type IntoIter = Iter<'a, PolicyVersion, PolicyRecord>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl PolicyCatalog {
    /// Creates an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true when the catalog holds no record.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Returns the record count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Stores one policy content object.
    ///
    /// A repeated insert returns the existing version and changes no byte.
    /// The method returns the catalog version.
    #[must_use]
    pub fn insert(&mut self, content: PolicyContent) -> PolicyVersion {
        let version = content.version();
        if self.records.contains_key(&version) {
            return version;
        }
        self.next_generation += 1;
        let canonical = content.canonical_bytes();
        self.records.insert(
            version,
            PolicyRecord {
                version,
                content,
                canonical,
                generation: self.next_generation,
            },
        );
        version
    }

    /// Returns true when the catalog holds the version.
    #[must_use]
    pub fn contains(&self, version: PolicyVersion) -> bool {
        self.records.contains_key(&version)
    }

    /// Resolves one policy version.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::PolicyVersionUnknown`] when the version is absent
    /// or below the retention floor.
    pub fn resolve(&self, version: PolicyVersion) -> Result<&PolicyRecord, RuleError> {
        let record = self
            .records
            .get(&version)
            .ok_or_else(|| RuleError::PolicyVersionUnknown {
                policy_version: version.to_string(),
            })?;
        if record.generation < self.retention_floor.generation() {
            return Err(RuleError::PolicyVersionUnknown {
                policy_version: version.to_string(),
            });
        }
        Ok(record)
    }

    /// Iterates over the records in policy version order.
    pub fn iter(&self) -> Iter<'_, PolicyVersion, PolicyRecord> {
        self.records.iter()
    }

    /// Returns the current retention floor.
    #[must_use]
    pub const fn retention_floor(&self) -> RetentionFloor {
        self.retention_floor
    }

    /// Raises the retention floor.
    ///
    /// The floor never moves backward. The caller must keep the floor at or
    /// below every active policy version.
    pub fn advance_floor(&mut self, floor: RetentionFloor) -> RetentionFloor {
        if floor > self.retention_floor {
            self.retention_floor = floor;
        }
        self.retention_floor
    }
}

/// Returns the shard identity of the catalog shard.
#[must_use]
pub fn catalog_shard_id() -> ShardId {
    ShardId::new(CATALOG_TABLE.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{CostBound, RuleId};
    use crate::policy::{PolicyId, RuleBody, SUPPORTED_POLICY_AST_VERSION};
    use std::collections::{BTreeMap, BTreeSet};

    fn policy(body: &[u8]) -> PolicyContent {
        let mut rules = BTreeMap::new();
        rules.insert(
            RuleId::parse("tenant_scope").expect("the rule identity is valid"),
            RuleBody::new(body.to_vec(), BTreeSet::new(), 0, 0).expect("the body is valid"),
        );
        PolicyContent::new(
            PolicyId::parse("tenant_policy").expect("the policy identity is valid"),
            SUPPORTED_POLICY_AST_VERSION,
            rules,
            CostBound::new(100, 8),
        )
        .expect("the policy is valid")
    }

    #[test]
    fn a_repeated_insert_does_not_change_a_record() {
        let content = policy(b"allow owner");
        let version = content.version();
        let mut catalog = PolicyCatalog::new();
        assert_eq!(catalog.insert(content.clone()), version);
        let first = catalog.resolve(version).expect("the record resolves");
        let bytes = first.canonical_bytes().to_vec();
        assert_eq!(catalog.insert(content), version);
        let second = catalog.resolve(version).expect("the record resolves");
        assert_eq!(second.canonical_bytes(), bytes);
        assert_eq!(catalog.len(), 1);
    }

    #[test]
    fn a_retained_record_resolves_until_the_floor_passes_it() {
        let mut catalog = PolicyCatalog::new();
        let first = catalog.insert(policy(b"allow owner"));
        let second = catalog.insert(policy(b"deny owner"));
        assert!(catalog.resolve(first).is_ok());
        catalog.advance_floor(RetentionFloor::new(2));
        assert!(catalog.resolve(first).is_err());
        assert!(catalog.resolve(second).is_ok());
        catalog.advance_floor(RetentionFloor::new(1));
        assert_eq!(catalog.retention_floor(), RetentionFloor::new(2));
    }

    #[test]
    fn a_missing_policy_version_is_an_error() {
        let catalog = PolicyCatalog::new();
        let result = catalog.resolve(PolicyVersion::from_bytes([9; 32]));
        assert_eq!(result.unwrap_err().code(), "scope.policy_version_unknown");
    }
}
