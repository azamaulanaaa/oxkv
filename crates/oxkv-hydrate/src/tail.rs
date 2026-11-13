//! The diff tail and its contiguous-prefix watermark.
//!
//! A tail frame applies only when its sequence follows the applied watermark.
//! A duplicate frame changes no state. A frame beyond the next sequence waits
//! in memory, and the caller must re-request the missing sequence.

use oxkv_contracts::version::{CommitVersion, ShardId};

/// One diff frame after the snapshot base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFrame {
    /// The strictly increasing frame sequence.
    pub sequence: u64,
    /// The shard that produced the frame.
    pub shard_id: ShardId,
    /// The shard commit version of the frame.
    pub commit_version: CommitVersion,
    /// The affected key.
    pub key: Vec<u8>,
    /// The new value. A value of `None` is a delete.
    pub value: Option<Vec<u8>>,
}

impl DiffFrame {
    /// Creates one diff frame.
    #[must_use]
    pub const fn new(
        sequence: u64,
        shard_id: ShardId,
        commit_version: CommitVersion,
        key: Vec<u8>,
        value: Option<Vec<u8>>,
    ) -> Self {
        Self {
            sequence,
            shard_id,
            commit_version,
            key,
            value,
        }
    }
}
