//! Named errors for the ordered key-value substrate.
//!
//! Each error names one cause. [`KvError::code`] returns the registered
//! error code from `oxkv-contracts`.

use oxkv_contracts::version::ShardId;
use thiserror::Error;

/// A named error from the ordered key-value substrate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KvError {
    /// The key has no live value at the requested commit version.
    #[error("The key is not found.")]
    KeyNotFound {
        /// The missing key.
        key: Vec<u8>,
    },
    /// The lower bound is greater than the upper bound.
    #[error("The range is malformed. The lower bound is greater than the upper bound.")]
    MalformedRange,
    /// The cursor token is malformed or tampered.
    #[error("The cursor token is malformed or tampered.")]
    MalformedCursor,
    /// The batch encoding is malformed.
    #[error("The batch encoding is malformed.")]
    MalformedBatch,
    /// The page request is malformed.
    #[error("The page request is malformed.")]
    MalformedPage,
    /// The version is not known.
    #[error("The version {version} is unknown.")]
    UnknownVersion {
        /// The unknown version.
        version: u64,
    },
    /// The batch belongs to another shard.
    #[error("The batch belongs to another shard. Expected {expected:?}, found {found:?}.")]
    CrossShardMutation {
        /// The shard of the engine.
        expected: ShardId,
        /// The shard of the batch.
        found: ShardId,
    },
    /// The key is larger than the key budget.
    #[error("The key is too large. Limit {limit} bytes, found {found} bytes.")]
    KeyTooLarge {
        /// The key length.
        found: usize,
        /// The key budget.
        limit: usize,
    },
    /// The value is larger than the value budget.
    #[error("The value is too large. Limit {limit} bytes, found {found} bytes.")]
    ValueTooLarge {
        /// The value length.
        found: usize,
        /// The value budget.
        limit: usize,
    },
    /// The batch has more operations than the batch budget.
    #[error("The batch is too large. Limit {limit} operations, found {found} operations.")]
    BatchTooLarge {
        /// The operation count.
        found: usize,
        /// The operation budget.
        limit: usize,
    },
    /// One page record is larger than the page budget.
    #[error("The page is too large. Limit {limit} bytes, found {found} bytes.")]
    PageTooLarge {
        /// The record length.
        found: usize,
        /// The page budget.
        limit: usize,
    },
}

impl KvError {
    /// Returns the registered error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::KeyNotFound { .. } => "key.not_found",
            Self::MalformedRange => "kv.malformed_range",
            Self::MalformedCursor => "kv.malformed_cursor",
            Self::MalformedBatch => "kv.malformed_batch",
            Self::MalformedPage => "kv.malformed_page",
            Self::UnknownVersion { .. } => "key.unknown_version",
            Self::CrossShardMutation { .. } => "shard.cross_mutation_rejected",
            Self::KeyTooLarge { .. } => "key.too_large",
            Self::ValueTooLarge { .. } => "write.value_too_large",
            Self::BatchTooLarge { .. } => "write.batch_too_large",
            Self::PageTooLarge { .. } => "format.page_too_large",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_names_its_registered_code() {
        let errors = [
            (KvError::KeyNotFound { key: vec![1] }, "key.not_found"),
            (KvError::MalformedRange, "kv.malformed_range"),
            (KvError::MalformedCursor, "kv.malformed_cursor"),
            (KvError::MalformedBatch, "kv.malformed_batch"),
            (KvError::MalformedPage, "kv.malformed_page"),
            (
                KvError::UnknownVersion { version: 2 },
                "key.unknown_version",
            ),
            (
                KvError::CrossShardMutation {
                    expected: ShardId::new(vec![1]),
                    found: ShardId::new(vec![2]),
                },
                "shard.cross_mutation_rejected",
            ),
            (KvError::KeyTooLarge { found: 3, limit: 2 }, "key.too_large"),
            (
                KvError::ValueTooLarge { found: 3, limit: 2 },
                "write.value_too_large",
            ),
            (
                KvError::BatchTooLarge { found: 3, limit: 2 },
                "write.batch_too_large",
            ),
            (
                KvError::PageTooLarge { found: 3, limit: 2 },
                "format.page_too_large",
            ),
        ];
        for (error, code) in errors {
            assert_eq!(error.code(), code);
            assert!(
                oxkv_contracts::error::lookup(code).is_some(),
                "the code {code} is not registered"
            );
        }
    }
}
