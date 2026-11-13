//! The explicit shard plan for one scope.
//!
//! A cross-prefix scope compiles into one plan entry per shard. Each entry
//! keeps one sync cursor position. The sync order is shard identity ascending.

use std::collections::BTreeMap;

use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::version::{CommitVersion, ScopeIdentity, ShardId};

use crate::declaration::{ScopeDeclaration, TablePrefix};
use crate::error::ScopeError;

/// One table assignment from the shard map.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShardAssignment {
    /// The assigned table.
    pub table: TableName,
    /// The shard that owns the table.
    pub shard_id: ShardId,
}

impl ShardAssignment {
    /// Creates one table assignment.
    #[must_use]
    pub const fn new(table: TableName, shard_id: ShardId) -> Self {
        Self { table, shard_id }
    }
}

/// The sync cursor position of one shard.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShardCursor {
    shard_id: ShardId,
    applied: Option<CommitVersion>,
}

impl ShardCursor {
    /// Creates an empty cursor for one shard.
    #[must_use]
    pub const fn new(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            applied: None,
        }
    }

    /// Returns the shard identity.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the last applied commit version.
    #[must_use]
    pub const fn applied(&self) -> Option<CommitVersion> {
        self.applied
    }

    /// Advances the cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::VersionRegression`] when the commit version
    /// decreases.
    pub fn advance(&mut self, commit_version: CommitVersion) -> Result<(), ScopeError> {
        if let Some(current) = self.applied
            && commit_version < current
        {
            return Err(ScopeError::VersionRegression {
                shard_id: self.shard_id.clone(),
                current,
                proposed: commit_version,
            });
        }
        self.applied = Some(commit_version);
        Ok(())
    }
}

/// One shard plan entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShardPlanEntry {
    shard_id: ShardId,
    prefixes: Vec<TablePrefix>,
    cursor: ShardCursor,
}

impl ShardPlanEntry {
    /// Returns the shard identity.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the table prefixes that this shard covers.
    #[must_use]
    pub fn prefixes(&self) -> &[TablePrefix] {
        &self.prefixes
    }

    /// Returns the sync cursor.
    #[must_use]
    pub const fn cursor(&self) -> &ShardCursor {
        &self.cursor
    }
}

/// The explicit shard plan of one scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShardPlan {
    identity: ScopeIdentity,
    entries: BTreeMap<ShardId, ShardPlanEntry>,
}

impl ShardPlan {
    /// Builds the shard plan for one declaration.
    ///
    /// The method reads the table assignment list. It does not read a shard
    /// map or any storage.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::UnmappedTable`] when the map has no assignment.
    /// Returns [`ScopeError::DuplicateTableAssignment`] for two assignments of
    /// one table. Returns [`ScopeError::OverLimit`] when the plan exceeds the
    /// declared `max_shards`.
    pub fn build(
        declaration: &ScopeDeclaration,
        assignments: &[ShardAssignment],
    ) -> Result<Self, ScopeError> {
        let mut assigned = BTreeMap::new();
        for assignment in assignments {
            if assigned
                .insert(
                    assignment.table.as_str().to_owned(),
                    assignment.shard_id.clone(),
                )
                .is_some()
            {
                return Err(ScopeError::DuplicateTableAssignment {
                    table: assignment.table.as_str().to_owned(),
                });
            }
        }
        let mut entries: BTreeMap<ShardId, ShardPlanEntry> = BTreeMap::new();
        for prefix in declaration.prefixes() {
            let shard_id =
                assigned
                    .get(prefix.table().as_str())
                    .ok_or_else(|| ScopeError::UnmappedTable {
                        table: prefix.table().as_str().to_owned(),
                    })?;
            entries
                .entry(shard_id.clone())
                .or_insert_with(|| ShardPlanEntry {
                    shard_id: shard_id.clone(),
                    prefixes: Vec::new(),
                    cursor: ShardCursor::new(shard_id.clone()),
                })
                .prefixes
                .push(prefix.clone());
        }
        let shard_count = u64::try_from(entries.len()).unwrap_or(u64::MAX);
        if shard_count > declaration.limits().max_shards() {
            return Err(ScopeError::OverLimit {
                limit: crate::error::AdmissionLimit::Shards,
                limit_value: declaration.limits().max_shards(),
                observed: shard_count,
            });
        }
        Ok(Self {
            identity: declaration.identity(),
            entries,
        })
    }

    /// Returns the scope identity.
    #[must_use]
    pub const fn identity(&self) -> ScopeIdentity {
        self.identity
    }

    /// Returns the number of shards.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the plan has no shard.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates over the shard identities in ascending order.
    pub fn shards(&self) -> impl Iterator<Item = &ShardId> {
        self.entries.keys()
    }

    /// Iterates over the plan entries in shard order.
    pub fn entries(&self) -> impl Iterator<Item = &ShardPlanEntry> {
        self.entries.values()
    }

    /// Returns one plan entry.
    #[must_use]
    pub fn entry(&self, shard_id: &ShardId) -> Option<&ShardPlanEntry> {
        self.entries.get(shard_id)
    }

    /// Returns one sync cursor.
    #[must_use]
    pub fn cursor(&self, shard_id: &ShardId) -> Option<&ShardCursor> {
        self.entries.get(shard_id).map(ShardPlanEntry::cursor)
    }

    /// Advances one shard cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeError::UnknownShard`] when the plan has no shard entry.
    /// Returns [`ScopeError::VersionRegression`] when the commit version
    /// decreases.
    pub fn advance_cursor(
        &mut self,
        shard_id: &ShardId,
        commit_version: CommitVersion,
    ) -> Result<(), ScopeError> {
        let entry = self
            .entries
            .get_mut(shard_id)
            .ok_or_else(|| ScopeError::UnknownShard {
                shard_id: shard_id.clone(),
            })?;
        entry.cursor.advance(commit_version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declaration::{
        AdmissionLimits, Ordering, PolicySelector, ProjectionSelector, RuleSetSelector,
    };

    fn declaration(tables: &[&str], max_shards: u64) -> ScopeDeclaration {
        ScopeDeclaration::new(
            tables
                .iter()
                .map(|name| TablePrefix::parse(name).expect("valid table")),
            PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
            ProjectionSelector::all(),
            Ordering::CanonicalKey,
            AdmissionLimits::new(10, 100, max_shards),
        )
        .expect("valid declaration")
    }

    fn assignment(table: &str, shard: u8) -> ShardAssignment {
        ShardAssignment::new(
            TableName::parse_user(table).expect("valid table"),
            ShardId::new(vec![shard]),
        )
    }

    #[test]
    fn a_cross_prefix_scope_has_one_cursor_per_shard() {
        let declaration = declaration(&["accounts", "ledger_entry", "audit_log"], 3);
        let plan = ShardPlan::build(
            &declaration,
            &[
                assignment("accounts", 1),
                assignment("ledger_entry", 2),
                assignment("audit_log", 3),
            ],
        )
        .expect("the plan builds");
        assert_eq!(plan.len(), 3);
        assert_eq!(plan.entries().count(), 3);
        for shard_id in plan.shards() {
            assert!(plan.cursor(shard_id).is_some());
        }
    }

    #[test]
    fn a_missing_assignment_fails() {
        let declaration = declaration(&["accounts", "ledger_entry"], 2);
        let error = ShardPlan::build(&declaration, &[assignment("accounts", 1)]).unwrap_err();
        assert_eq!(
            error,
            ScopeError::UnmappedTable {
                table: "ledger_entry".to_owned()
            }
        );
        assert_eq!(error.code(), "map.table_unmapped");
    }

    #[test]
    fn a_shard_count_over_the_limit_fails() {
        let declaration = declaration(&["accounts", "ledger_entry"], 1);
        let error = ShardPlan::build(
            &declaration,
            &[assignment("accounts", 1), assignment("ledger_entry", 2)],
        )
        .unwrap_err();
        assert_eq!(
            error,
            ScopeError::OverLimit {
                limit: crate::error::AdmissionLimit::Shards,
                limit_value: 1,
                observed: 2
            }
        );
    }

    #[test]
    fn a_cursor_does_not_decrease() {
        let declaration = declaration(&["accounts"], 1);
        let mut plan =
            ShardPlan::build(&declaration, &[assignment("accounts", 1)]).expect("the plan builds");
        let shard_id = ShardId::new(vec![1]);
        plan.advance_cursor(&shard_id, CommitVersion::new(4))
            .expect("the cursor advances");
        assert!(
            plan.advance_cursor(&shard_id, CommitVersion::new(3))
                .is_err()
        );
        assert_eq!(
            plan.cursor(&shard_id).expect("the cursor exists").applied(),
            Some(CommitVersion::new(4))
        );
    }
}
