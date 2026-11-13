//! Atomic batches and their canonical operation log.

use oxkv_contracts::version::{CommitVersion, ShardId};

use crate::codec::Decoder;
use crate::error::KvError;
use crate::operation::{OP_DELETE, OP_PUT, Operation, push_bytes};

/// The batch log magic bytes.
const BATCH_MAGIC: [u8; 4] = *b"OXKB";
/// The batch log format version.
pub(crate) const BATCH_VERSION: u16 = 1;

/// One single-shard batch of operations.
///
/// The engine applies the operations in slice order. The batch commits all
/// operations at one commit version or changes no state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    shard_id: ShardId,
    commit_version: CommitVersion,
    operations: Vec<Operation>,
}

impl Batch {
    /// Creates a batch.
    #[must_use]
    pub const fn new(
        shard_id: ShardId,
        commit_version: CommitVersion,
        operations: Vec<Operation>,
    ) -> Self {
        Self {
            shard_id,
            commit_version,
            operations,
        }
    }

    /// Returns the shard of the batch.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the commit version of the batch.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the operations in application order.
    #[must_use]
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// Returns the encoded length of the operation log in bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        4 + 2
            + 8
            + self.shard_id.as_bytes().len()
            + 8
            + 8
            + self
                .operations
                .iter()
                .map(Operation::encoded_len)
                .sum::<usize>()
    }

    /// Encodes the batch as one canonical operation log.
    ///
    /// The encoding is identical on native and `wasm32`. It has a fixed
    /// big-endian form.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.encoded_len());
        bytes.extend_from_slice(&BATCH_MAGIC);
        bytes.extend_from_slice(&BATCH_VERSION.to_be_bytes());
        push_bytes(&mut bytes, self.shard_id.as_bytes());
        bytes.extend_from_slice(&self.commit_version.get().to_be_bytes());
        bytes.extend_from_slice(
            &u64::try_from(self.operations.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        for operation in &self.operations {
            operation.encode_into(&mut bytes);
        }
        bytes
    }

    /// Decodes one canonical operation log.
    ///
    /// # Errors
    ///
    /// Returns [`KvError::MalformedBatch`] for a bad magic value, a bad body,
    /// or trailing bytes. Returns [`KvError::UnknownVersion`] for an unknown
    /// batch format version.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KvError> {
        let mut decoder = Decoder::new(bytes);
        let magic = decoder.take(4).ok_or(KvError::MalformedBatch)?;
        if magic != BATCH_MAGIC {
            return Err(KvError::MalformedBatch);
        }
        let version = decoder.u16().ok_or(KvError::MalformedBatch)?;
        if version != BATCH_VERSION {
            return Err(KvError::UnknownVersion {
                version: u64::from(version),
            });
        }
        let shard_id = ShardId::new(decoder.bytes().ok_or(KvError::MalformedBatch)?.to_vec());
        let commit_version = CommitVersion::new(decoder.u64().ok_or(KvError::MalformedBatch)?);
        let count = decoder.u64().ok_or(KvError::MalformedBatch)?;
        let count = usize::try_from(count).map_err(|_| KvError::MalformedBatch)?;
        // One operation needs at least one tag byte and one key length.
        if count > decoder.remaining() / 9 {
            return Err(KvError::MalformedBatch);
        }
        let mut operations = Vec::with_capacity(count);
        for _ in 0..count {
            let tag = decoder.u8().ok_or(KvError::MalformedBatch)?;
            let key = decoder.bytes().ok_or(KvError::MalformedBatch)?.to_vec();
            let operation = match tag {
                OP_PUT => {
                    let value = decoder.bytes().ok_or(KvError::MalformedBatch)?.to_vec();
                    Operation::Put { key, value }
                }
                OP_DELETE => Operation::Delete { key },
                _ => return Err(KvError::MalformedBatch),
            };
            operations.push(operation);
        }
        if !decoder.is_finished() {
            return Err(KvError::MalformedBatch);
        }
        Ok(Self::new(shard_id, commit_version, operations))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Batch {
        Batch::new(
            ShardId::new(b"shard-1".to_vec()),
            CommitVersion::new(3),
            vec![
                Operation::put(b"a".to_vec(), b"1".to_vec()),
                Operation::delete(b"b".to_vec()),
            ],
        )
    }

    #[test]
    fn a_batch_round_trips_through_its_log() {
        let batch = sample();
        assert_eq!(Batch::from_bytes(&batch.to_bytes()), Ok(batch));
    }

    #[test]
    fn a_bad_magic_fails() {
        let mut bytes = sample().to_bytes();
        bytes[0] = 0;
        assert_eq!(Batch::from_bytes(&bytes), Err(KvError::MalformedBatch));
    }

    #[test]
    fn an_unknown_batch_version_fails() {
        let mut bytes = sample().to_bytes();
        bytes[4..6].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(
            Batch::from_bytes(&bytes),
            Err(KvError::UnknownVersion { version: 2 })
        );
    }

    #[test]
    fn trailing_bytes_fail() {
        let mut bytes = sample().to_bytes();
        bytes.push(0);
        assert_eq!(Batch::from_bytes(&bytes), Err(KvError::MalformedBatch));
    }

    #[test]
    fn an_impossible_operation_count_fails() {
        let mut bytes = sample().to_bytes();
        let count_start = 4 + 2 + 8 + b"shard-1".len() + 8;
        bytes[count_start..count_start + 8].copy_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(Batch::from_bytes(&bytes), Err(KvError::MalformedBatch));
    }

    #[test]
    fn the_operation_order_is_preserved() {
        let batch = sample();
        let decoded = Batch::from_bytes(&batch.to_bytes()).expect("the log decodes");
        assert_eq!(decoded.operations(), batch.operations());
    }
}
