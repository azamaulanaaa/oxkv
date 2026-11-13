//! The ordered map interface and its two in-tree engines.
//!
//! [`BTreeEngine`] stores per-key version histories in a B-tree.
//! [`LsmEngine`] appends every committed batch to a log and replays the log
//! at read time. Both engines return the same ordered results for the same
//! operation log.

mod btree;
mod lsm;

pub use btree::BTreeEngine;
pub use lsm::LsmEngine;

use oxkv_contracts::CursorDirection;
use oxkv_contracts::version::{CommitVersion, ShardId};

use crate::batch::Batch;
use crate::cursor::Cursor;
use crate::entry::Entry;
use crate::error::KvError;
use crate::limits::Limits;
use crate::operation::Operation;
use crate::page::Page;
use crate::range::KeyRange;

/// One ordered map over immutable byte keys and versioned byte values.
///
/// The comparator is lexicographic over unsigned bytes. The order does not
/// depend on locale, platform, pointer width, or engine.
pub trait OrderedMap {
    /// Returns the shard of the engine.
    fn shard_id(&self) -> &ShardId;

    /// Returns the size limits of the engine.
    fn limits(&self) -> &Limits;

    /// Returns the last committed version.
    fn last_commit_version(&self) -> Option<CommitVersion>;

    /// Reads one entry at the given commit version.
    ///
    /// # Errors
    ///
    /// Returns an error only when the engine cannot serve the read.
    fn get_entry(&self, version: CommitVersion, key: &[u8]) -> Result<Option<Entry>, KvError>;

    /// Reads one value at the given commit version.
    ///
    /// # Errors
    ///
    /// Returns an error when the engine cannot serve the read.
    fn get(&self, version: CommitVersion, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        Ok(self.get_entry(version, key)?.map(|entry| entry.value))
    }

    /// Reads one required entry at the given commit version.
    ///
    /// # Errors
    ///
    /// Returns [`KvError::KeyNotFound`] when the key has no live value at the
    /// requested commit version.
    fn get_required(&self, version: CommitVersion, key: &[u8]) -> Result<Entry, KvError> {
        self.get_entry(version, key)?
            .ok_or_else(|| KvError::KeyNotFound { key: key.to_vec() })
    }

    /// Reads every live entry inside one inclusive range.
    ///
    /// # Errors
    ///
    /// Returns [`KvError::MalformedRange`] when the range is inverted.
    fn scan(
        &self,
        version: CommitVersion,
        range: &KeyRange,
        direction: CursorDirection,
    ) -> Result<Vec<Entry>, KvError>;

    /// Reads one bounded page from a cursor.
    ///
    /// # Errors
    ///
    /// Returns [`KvError::MalformedPage`] when `max_entries` is zero. Returns
    /// [`KvError::PageTooLarge`] when one entry does not fit the page budget.
    /// Returns [`KvError::MalformedRange`] when the range is inverted.
    fn page(&self, cursor: &Cursor, range: &KeyRange, max_entries: usize) -> Result<Page, KvError>;

    /// Writes one key at the given commit version.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`OrderedMap::commit`].
    fn put(
        &mut self,
        version: CommitVersion,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> Result<CommitVersion, KvError> {
        let shard_id = self.shard_id().clone();
        self.commit(Batch::new(
            shard_id,
            version,
            vec![Operation::put(key, value)],
        ))
    }

    /// Deletes one key at the given commit version.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`OrderedMap::commit`].
    fn delete(&mut self, version: CommitVersion, key: Vec<u8>) -> Result<CommitVersion, KvError> {
        let shard_id = self.shard_id().clone();
        self.commit(Batch::new(shard_id, version, vec![Operation::delete(key)]))
    }

    /// Commits one batch at its commit version.
    ///
    /// # Errors
    ///
    /// Returns [`KvError::CrossShardMutation`] when the batch belongs to a
    /// different shard. Returns [`KvError::BatchTooLarge`],
    /// [`KvError::KeyTooLarge`], or [`KvError::ValueTooLarge`] when a budget
    /// is breached. Returns [`KvError::UnknownVersion`] when the commit
    /// version is not greater than the last committed version.
    fn commit(&mut self, batch: Batch) -> Result<CommitVersion, KvError>;
}

pub(crate) fn validate_batch(
    batch: &Batch,
    shard_id: &ShardId,
    limits: &Limits,
    last_commit_version: Option<CommitVersion>,
) -> Result<(), KvError> {
    if batch.shard_id() != shard_id {
        return Err(KvError::CrossShardMutation {
            expected: shard_id.clone(),
            found: batch.shard_id().clone(),
        });
    }
    if batch.operations().len() > limits.batch_operations {
        return Err(KvError::BatchTooLarge {
            found: batch.operations().len(),
            limit: limits.batch_operations,
        });
    }
    for operation in batch.operations() {
        if operation.key().len() > limits.key_bytes {
            return Err(KvError::KeyTooLarge {
                found: operation.key().len(),
                limit: limits.key_bytes,
            });
        }
        if let Some(value) = operation.value()
            && value.len() > limits.value_bytes
        {
            return Err(KvError::ValueTooLarge {
                found: value.len(),
                limit: limits.value_bytes,
            });
        }
    }
    if let Some(last) = last_commit_version
        && batch.commit_version() <= last
    {
        return Err(KvError::UnknownVersion {
            version: batch.commit_version().get(),
        });
    }
    Ok(())
}

pub(crate) fn build_page(
    entries: impl Iterator<Item = Entry>,
    cursor: &Cursor,
    max_entries: usize,
    max_page_bytes: usize,
) -> Result<Page, KvError> {
    if max_entries == 0 {
        return Err(KvError::MalformedPage);
    }
    let mut iterator = entries;
    let mut page_entries = Vec::new();
    let mut used = 0_usize;
    let mut has_more = false;
    while let Some(entry) = iterator.next() {
        let length = entry.encoded_len();
        if used.saturating_add(length) > max_page_bytes {
            if page_entries.is_empty() {
                return Err(KvError::PageTooLarge {
                    found: length,
                    limit: max_page_bytes,
                });
            }
            has_more = true;
            break;
        }
        used += length;
        page_entries.push(entry);
        if page_entries.len() == max_entries {
            has_more = iterator.next().is_some();
            break;
        }
    }
    let next = if has_more {
        page_entries.last().map(|entry| {
            Cursor::after(
                cursor.snapshot_version(),
                entry.key.clone(),
                cursor.direction(),
            )
        })
    } else {
        None
    };
    Ok(Page::new(page_entries, next))
}
