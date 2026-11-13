//! The scope-projected mirror.
//!
//! The mirror stores `ScopeSnapshot` pages that the authority produced for one
//! scope. It also stores the user rows and projection facts that the pages
//! carry. It stores no receipt, event, audit record, or authority lease
//! record.
//!
//! The mirror accepts only authority data. [`Mirror::ingest_page`] and
//! [`Mirror::apply_diff`] are the only paths that change mirror bytes. A local
//! write has no path into the mirror.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::version::{
    CommitVersion, FactSourceId, FactVersion, KeyVersion, PageId, ScopeIdentity,
    ScopeVersionVector, ShardId,
};

use crate::codec::as_u64;
use crate::error::ReplicaError;
use crate::page::{MirroredRecord, RecordBody, ScopeSnapshotPage, snapshot_digest};
use crate::residency::EvictionReport;
use crate::scope::Scope;
use crate::versions::set_version;

/// The result of one ingest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    /// The mirror changed.
    Applied,
    /// The input was a duplicate or was superseded. The mirror did not change.
    Duplicate,
}

/// One user row in the mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowEntry {
    /// The version of the key record.
    pub key_version: KeyVersion,
    /// The commit version that delivered the row.
    pub commit_version: CommitVersion,
    /// The value. `None` is a tombstone.
    pub value: Option<Vec<u8>>,
    /// The page that delivered the row. `None` means a diff tail entry.
    pub page_id: Option<PageId>,
}

/// One projection fact in the mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactEntry {
    /// The fact version.
    pub fact_version: FactVersion,
    /// The commit version that delivered the fact.
    pub commit_version: CommitVersion,
    /// The fact value. `None` is a tombstone.
    pub value: Option<Vec<u8>>,
    /// The page that delivered the fact. `None` means a diff tail entry.
    pub page_id: Option<PageId>,
}

/// One authority diff frame for the mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFrame {
    /// The shard that owns the key.
    pub shard_id: ShardId,
    /// The shard commit version.
    pub commit_version: CommitVersion,
    /// The canonical key.
    pub key: Vec<u8>,
    /// The value. `None` is a tombstone.
    pub value: Option<Vec<u8>>,
    /// The key version.
    pub key_version: KeyVersion,
    /// The scope identity.
    pub scope_identity: ScopeIdentity,
}

/// The read-only scope mirror.
#[derive(Debug, Clone)]
pub struct Mirror {
    scope: Scope,
    applied: ScopeVersionVector,
    pages: BTreeMap<PageId, ScopeSnapshotPage>,
    rows: BTreeMap<Vec<u8>, RowEntry>,
    facts: BTreeMap<(FactSourceId, Vec<u8>), FactEntry>,
    resident_bytes: u64,
    ceiling_bytes: u64,
    tail_entries: u64,
    generation: u64,
}

impl Mirror {
    /// Creates an empty mirror for one scope.
    #[must_use]
    pub fn new(scope: Scope, ceiling_bytes: u64, generation: u64) -> Self {
        Self {
            scope,
            applied: ScopeVersionVector::new(),
            pages: BTreeMap::new(),
            rows: BTreeMap::new(),
            facts: BTreeMap::new(),
            resident_bytes: 0,
            ceiling_bytes,
            tail_entries: 0,
            generation,
        }
    }

    /// Returns the scope.
    #[must_use]
    pub const fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Returns the applied version vector.
    #[must_use]
    pub const fn applied(&self) -> &ScopeVersionVector {
        &self.applied
    }

    /// Returns the generation number.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the resident byte count.
    #[must_use]
    pub const fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    /// Returns the page count.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Returns the tail entry count.
    #[must_use]
    pub const fn tail_entries(&self) -> u64 {
        self.tail_entries
    }

    /// Returns the content digest of the stored page set.
    #[must_use]
    pub fn snapshot_digest(&self) -> oxkv_contracts::version::ContentDigest {
        snapshot_digest(self.pages.values())
    }

    /// Returns the live member keys in canonical order.
    #[must_use]
    pub fn membership(&self) -> BTreeSet<Vec<u8>> {
        self.rows.keys().cloned().collect()
    }

    /// Iterates over the rows in canonical key order.
    pub fn rows(&self) -> impl Iterator<Item = (&[u8], &RowEntry)> {
        self.rows.iter().map(|(key, entry)| (key.as_slice(), entry))
    }

    /// Iterates over the facts in canonical order.
    pub fn facts(&self) -> impl Iterator<Item = (&FactSourceId, &[u8], &FactEntry)> {
        self.facts
            .iter()
            .map(|((source, key), entry)| (source, key.as_slice(), entry))
    }

    /// Returns one row.
    #[must_use]
    pub fn row(&self, key: &[u8]) -> Option<&RowEntry> {
        self.rows.get(key)
    }

    /// Returns one fact.
    #[must_use]
    pub fn fact(&self, source: &FactSourceId, key: &[u8]) -> Option<&FactEntry> {
        self.facts.get(&(source.clone(), key.to_vec()))
    }

    /// Ingests one scope snapshot page.
    ///
    /// The method verifies the page digest, the scope pin, the page bounds,
    /// and every record. It refuses a page that carries a record outside the
    /// scope. A duplicate or superseded page changes no state.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::CorruptPage`] for a digest or count mismatch.
    /// Returns [`ReplicaError::ScopePinMismatch`] for another scope pin.
    /// Returns [`ReplicaError::ScopeViolation`] for an out-of-scope record.
    /// Returns [`ReplicaError::PageOutOfBounds`] for a key outside the bounds.
    /// Returns [`ReplicaError::ResourceExhausted`] when the ceiling cannot hold
    /// the page after eviction.
    pub fn ingest_page(&mut self, page: ScopeSnapshotPage) -> Result<Ingest, ReplicaError> {
        page.verify()?;
        self.check_pin(&page)?;
        for record in &page.records {
            if !page.header.contains(&record.key) {
                return Err(ReplicaError::PageOutOfBounds);
            }
            self.check_record_scope(page.header.page_id, record)?;
        }
        if self.pages.contains_key(&page.header.page_id) {
            return Ok(Ingest::Duplicate);
        }
        let shard_id = page.header.shard_id.clone();
        let commit_version = page.header.commit_version;
        if self
            .applied
            .get(&shard_id)
            .is_some_and(|current| commit_version < current)
        {
            return Ok(Ingest::Duplicate);
        }
        let plan = self.plan_records(&page);
        let page_bytes = page.resident_bytes();
        self.reserve(page_bytes.saturating_add(plan.delta_bytes))?;
        for (key, entry) in plan.rows {
            self.rows.insert(key, entry);
        }
        for (key, entry) in plan.facts {
            self.facts.insert(key, entry);
        }
        let page_id = page.header.page_id;
        self.pages.insert(page_id, page);
        if self
            .applied
            .get(&shard_id)
            .is_none_or(|current| commit_version > current)
        {
            set_version(&mut self.applied, shard_id, commit_version);
        }
        self.recount();
        Ok(Ingest::Applied)
    }

    /// Applies one authority diff frame.
    ///
    /// A frame at or below the applied shard version is a duplicate. A frame
    /// for a key outside the scope removes a stale row and stores no new row.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::ScopePinMismatch`] for another scope identity.
    /// Returns [`ReplicaError::ScopeViolation`] when the frame would store an
    /// out-of-scope row. Returns [`ReplicaError::ResourceExhausted`] when the
    /// ceiling cannot hold the frame after eviction.
    pub fn apply_diff(&mut self, diff: &DiffFrame) -> Result<Ingest, ReplicaError> {
        if diff.scope_identity != self.scope.identity() {
            return Err(ReplicaError::ScopePinMismatch);
        }
        if let Some(current) = self.applied.get(&diff.shard_id) {
            if diff.commit_version <= current {
                return Ok(Ingest::Duplicate);
            }
            let expected = CommitVersion::new(current.get().saturating_add(1));
            if diff.commit_version != expected {
                return Err(ReplicaError::DiffGap {
                    shard_id: diff.shard_id.clone(),
                    expected,
                    found: diff.commit_version,
                });
            }
        }
        let tail_bytes = diff_bytes(diff);
        if !self.scope.admits_row(&diff.key) {
            self.rows.remove(&diff.key);
            set_version(
                &mut self.applied,
                diff.shard_id.clone(),
                diff.commit_version,
            );
            self.tail_entries = self.tail_entries.saturating_add(1);
            self.recount();
            return Ok(Ingest::Applied);
        }
        let entry = RowEntry {
            key_version: diff.key_version,
            commit_version: diff.commit_version,
            value: diff.value.clone(),
            page_id: None,
        };
        if let Some(current) = self.rows.get(&diff.key)
            && current.key_version >= diff.key_version
        {
            return Ok(Ingest::Duplicate);
        }
        let delta = row_bytes(&diff.key, &entry).saturating_sub(
            self.rows
                .get(&diff.key)
                .map_or(0, |current| row_bytes(&diff.key, current)),
        );
        self.reserve(tail_bytes.saturating_add(delta))?;
        self.rows.insert(diff.key.clone(), entry);
        set_version(
            &mut self.applied,
            diff.shard_id.clone(),
            diff.commit_version,
        );
        self.tail_entries = self.tail_entries.saturating_add(1);
        self.recount();
        Ok(Ingest::Applied)
    }

    /// Evicts pages that the applied version superseded.
    ///
    /// The method removes no live member. Rows and facts stay resident. The
    /// page count and the page bytes fall.
    #[must_use]
    pub fn evict_superseded(&mut self) -> EvictionReport {
        let mut removed = Vec::new();
        for (page_id, page) in &self.pages {
            let current = self.applied.get(&page.header.shard_id);
            if current.is_some_and(|version| page.header.commit_version < version) {
                removed.push(*page_id);
            }
        }
        let mut bytes_freed = 0_u64;
        for page_id in &removed {
            if let Some(page) = self.pages.remove(page_id) {
                bytes_freed = bytes_freed.saturating_add(page.resident_bytes());
            }
        }
        self.recount();
        EvictionReport {
            pages_removed: as_u64(removed.len()),
            bytes_freed,
        }
    }

    fn check_pin(&self, page: &ScopeSnapshotPage) -> Result<(), ReplicaError> {
        if page.header.scope_identity != self.scope.identity()
            || page.header.manifest_digest != self.scope.manifest_digest()
            || page.header.policy_version != self.scope.policy_version()
        {
            return Err(ReplicaError::ScopePinMismatch);
        }
        Ok(())
    }

    fn check_record_scope(
        &self,
        page_id: PageId,
        record: &MirroredRecord,
    ) -> Result<(), ReplicaError> {
        let admitted = match &record.body {
            RecordBody::Row { .. } => self.scope.admits_row(&record.key),
            RecordBody::Fact { source, .. } => self.scope.admits_fact(source),
        };
        if admitted {
            Ok(())
        } else {
            Err(ReplicaError::ScopeViolation { page_id })
        }
    }

    fn plan_records(&self, page: &ScopeSnapshotPage) -> RecordPlan {
        let mut plan = RecordPlan::default();
        for record in &page.records {
            match &record.body {
                RecordBody::Row { key_version } => {
                    let existing = self
                        .rows
                        .get(&record.key)
                        .or_else(|| plan.rows.get(&record.key));
                    if existing.is_some_and(|entry| entry.key_version >= *key_version) {
                        continue;
                    }
                    let entry = RowEntry {
                        key_version: *key_version,
                        commit_version: page.header.commit_version,
                        value: record.value.clone(),
                        page_id: Some(page.header.page_id),
                    };
                    plan.delta_bytes = plan
                        .delta_bytes
                        .saturating_add(row_bytes(&record.key, &entry))
                        .saturating_sub(
                            self.rows
                                .get(&record.key)
                                .map_or(0, |current| row_bytes(&record.key, current)),
                        );
                    plan.rows.insert(record.key.clone(), entry);
                }
                RecordBody::Fact {
                    source,
                    fact_version,
                } => {
                    let key = (source.clone(), record.key.clone());
                    let existing = self.facts.get(&key).or_else(|| plan.facts.get(&key));
                    if existing.is_some_and(|entry| entry.fact_version >= *fact_version) {
                        continue;
                    }
                    let entry = FactEntry {
                        fact_version: *fact_version,
                        commit_version: page.header.commit_version,
                        value: record.value.clone(),
                        page_id: Some(page.header.page_id),
                    };
                    plan.delta_bytes = plan
                        .delta_bytes
                        .saturating_add(fact_bytes(source, &record.key, &entry))
                        .saturating_sub(
                            self.facts
                                .get(&key)
                                .map_or(0, |current| fact_bytes(source, &record.key, current)),
                        );
                    plan.facts.insert(key, entry);
                }
            }
        }
        plan
    }

    fn reserve(&mut self, incoming_bytes: u64) -> Result<(), ReplicaError> {
        let projected = self.resident_bytes.saturating_add(incoming_bytes);
        if projected <= self.ceiling_bytes {
            return Ok(());
        }
        let _ = self.evict_superseded();
        let projected = self.resident_bytes.saturating_add(incoming_bytes);
        if projected > self.ceiling_bytes {
            return Err(ReplicaError::ResourceExhausted {
                required: projected,
                ceiling: self.ceiling_bytes,
            });
        }
        Ok(())
    }

    fn recount(&mut self) {
        let pages = self.pages.values().fold(0_u64, |total, page| {
            total.saturating_add(page.resident_bytes())
        });
        let rows = self.rows.iter().fold(0_u64, |total, (key, entry)| {
            total.saturating_add(row_bytes(key, entry))
        });
        let facts = self
            .facts
            .iter()
            .fold(0_u64, |total, ((source, key), entry)| {
                total.saturating_add(fact_bytes(source, key, entry))
            });
        let tail = self.tail_entries.saturating_mul(TAIL_ENTRY_OVERHEAD_BYTES);
        self.resident_bytes = pages
            .saturating_add(rows)
            .saturating_add(facts)
            .saturating_add(tail);
    }
}

const TAIL_ENTRY_OVERHEAD_BYTES: u64 = 64;

#[derive(Default)]
struct RecordPlan {
    rows: BTreeMap<Vec<u8>, RowEntry>,
    facts: BTreeMap<(FactSourceId, Vec<u8>), FactEntry>,
    delta_bytes: u64,
}

fn row_bytes(key: &[u8], entry: &RowEntry) -> u64 {
    as_u64(key.len())
        .saturating_add(entry.value.as_ref().map_or(0, |value| as_u64(value.len())))
        .saturating_add(24)
}

fn fact_bytes(source: &FactSourceId, key: &[u8], entry: &FactEntry) -> u64 {
    as_u64(source.as_str().len())
        .saturating_add(as_u64(key.len()))
        .saturating_add(entry.value.as_ref().map_or(0, |value| as_u64(value.len())))
        .saturating_add(24)
}

fn diff_bytes(diff: &DiffFrame) -> u64 {
    as_u64(diff.key.len())
        .saturating_add(diff.value.as_ref().map_or(0, |value| as_u64(value.len())))
        .saturating_add(as_u64(diff.shard_id.as_bytes().len()))
        .saturating_add(TAIL_ENTRY_OVERHEAD_BYTES)
}
