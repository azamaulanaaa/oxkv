//! The append-only log engine.

use std::collections::BTreeMap;

use oxkv_contracts::CursorDirection;
use oxkv_contracts::version::{CommitVersion, ShardId};

use super::{OrderedMap, build_page, validate_batch};
use crate::batch::Batch;
use crate::cursor::Cursor;
use crate::entry::Entry;
use crate::error::KvError;
use crate::limits::Limits;
use crate::operation::Operation;
use crate::page::Page;
use crate::range::KeyRange;

/// An append-only log engine.
///
/// The engine appends every committed batch to one log. A read replays the
/// log at the requested commit version. The engine models the LSM seam. It
/// hides the log from every caller.
#[derive(Debug, Clone)]
pub struct LsmEngine {
    shard_id: ShardId,
    limits: Limits,
    log: Vec<Batch>,
    last_commit_version: Option<CommitVersion>,
}

impl LsmEngine {
    /// Creates an append-only log engine.
    #[must_use]
    pub fn new(shard_id: ShardId, limits: Limits) -> Self {
        Self {
            shard_id,
            limits,
            log: Vec::new(),
            last_commit_version: None,
        }
    }

    fn state_at(&self, version: CommitVersion) -> BTreeMap<Vec<u8>, (CommitVersion, Vec<u8>)> {
        let mut state = BTreeMap::new();
        for batch in &self.log {
            if batch.commit_version() > version {
                break;
            }
            for operation in batch.operations() {
                match operation {
                    Operation::Put { key, value } => {
                        state.insert(key.clone(), (batch.commit_version(), value.clone()));
                    }
                    Operation::Delete { key } => {
                        state.remove(key);
                    }
                }
            }
        }
        state
    }

    fn matching(
        &self,
        version: CommitVersion,
        range: &KeyRange,
        direction: CursorDirection,
        after: Option<&[u8]>,
    ) -> Vec<Entry> {
        let state = self.state_at(version);
        let mut entries = Vec::new();
        match direction {
            CursorDirection::Ascending => {
                for (key, (commit_version, value)) in &state {
                    if !range.contains(key) || after.is_some_and(|last| key.as_slice() <= last) {
                        continue;
                    }
                    entries.push(Entry::new(key.clone(), value.clone(), *commit_version));
                }
            }
            CursorDirection::Descending => {
                for (key, (commit_version, value)) in state.iter().rev() {
                    if !range.contains(key) || after.is_some_and(|last| key.as_slice() >= last) {
                        continue;
                    }
                    entries.push(Entry::new(key.clone(), value.clone(), *commit_version));
                }
            }
        }
        entries
    }
}

impl OrderedMap for LsmEngine {
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
        Ok(self
            .state_at(version)
            .get(key)
            .map(|(commit_version, value)| {
                Entry::new(key.to_vec(), value.clone(), *commit_version)
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
        self.log.push(batch);
        self.last_commit_version = Some(commit_version);
        Ok(commit_version)
    }
}
