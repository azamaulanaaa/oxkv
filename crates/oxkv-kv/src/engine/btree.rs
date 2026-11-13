//! The ordered B-tree engine.

use std::collections::BTreeMap;

use oxkv_contracts::CursorDirection;
use oxkv_contracts::version::{CommitVersion, ShardId};

use super::{OrderedMap, build_page, validate_batch};
use crate::batch::Batch;
use crate::cursor::Cursor;
use crate::entry::Entry;
use crate::error::KvError;
use crate::limits::Limits;
use crate::page::Page;
use crate::range::KeyRange;

/// An ordered B-tree engine.
///
/// The engine keeps one version history for each key. A read selects the
/// newest record at or before the requested commit version.
#[derive(Debug, Clone)]
pub struct BTreeEngine {
    shard_id: ShardId,
    limits: Limits,
    versions: BTreeMap<Vec<u8>, BTreeMap<CommitVersion, Option<Vec<u8>>>>,
    last_commit_version: Option<CommitVersion>,
}

impl BTreeEngine {
    /// Creates a B-tree engine.
    #[must_use]
    pub fn new(shard_id: ShardId, limits: Limits) -> Self {
        Self {
            shard_id,
            limits,
            versions: BTreeMap::new(),
            last_commit_version: None,
        }
    }

    fn matching(
        &self,
        version: CommitVersion,
        range: &KeyRange,
        direction: CursorDirection,
        after: Option<&[u8]>,
    ) -> Vec<Entry> {
        let mut entries = Vec::new();
        match direction {
            CursorDirection::Ascending => {
                for (key, history) in &self.versions {
                    if !range.contains(key) || after.is_some_and(|last| key.as_slice() <= last) {
                        continue;
                    }
                    if let Some((commit_version, Some(value))) =
                        history.range(..=version).next_back()
                    {
                        entries.push(Entry::new(key.clone(), value.clone(), *commit_version));
                    }
                }
            }
            CursorDirection::Descending => {
                for (key, history) in self.versions.iter().rev() {
                    if !range.contains(key) || after.is_some_and(|last| key.as_slice() >= last) {
                        continue;
                    }
                    if let Some((commit_version, Some(value))) =
                        history.range(..=version).next_back()
                    {
                        entries.push(Entry::new(key.clone(), value.clone(), *commit_version));
                    }
                }
            }
        }
        entries
    }
}

impl OrderedMap for BTreeEngine {
    fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    fn limits(&self) -> &Limits {
        &self.limits
    }

    fn last_commit_version(&self) -> Option<CommitVersion> {
        self.last_commit_version
    }

    fn get_entry(&self, version: CommitVersion, key: &[u8]) -> Result<Option<Entry>, KvError> {
        Ok(self.versions.get(key).and_then(|history| {
            history
                .range(..=version)
                .next_back()
                .and_then(|(commit_version, value)| {
                    value
                        .as_ref()
                        .map(|value| Entry::new(key.to_vec(), value.clone(), *commit_version))
                })
        }))
    }

    fn scan(
        &self,
        version: CommitVersion,
        range: &KeyRange,
        direction: CursorDirection,
    ) -> Result<Vec<Entry>, KvError> {
        Ok(self.matching(version, range, direction, None))
    }

    fn page(&self, cursor: &Cursor, range: &KeyRange, max_entries: usize) -> Result<Page, KvError> {
        let entries = self.matching(
            cursor.snapshot_version(),
            range,
            cursor.direction(),
            cursor.position(),
        );
        build_page(
            entries.into_iter(),
            cursor,
            max_entries,
            self.limits.page_bytes,
        )
    }

    fn commit(&mut self, batch: Batch) -> Result<CommitVersion, KvError> {
        validate_batch(
            &batch,
            &self.shard_id,
            &self.limits,
            self.last_commit_version,
        )?;
        let commit_version = batch.commit_version();
        for operation in batch.operations() {
            match operation {
                crate::operation::Operation::Put { key, value } => {
                    self.versions
                        .entry(key.clone())
                        .or_default()
                        .insert(commit_version, Some(value.clone()));
                }
                crate::operation::Operation::Delete { key } => {
                    self.versions
                        .entry(key.clone())
                        .or_default()
                        .insert(commit_version, None);
                }
            }
        }
        self.last_commit_version = Some(commit_version);
        Ok(commit_version)
    }
}
