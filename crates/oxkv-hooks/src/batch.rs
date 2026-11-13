//! The staged write batch and its explicit input envelope.

use std::collections::BTreeMap;

use oxkv_contracts::version::{Hash32, KeyVersion, ShardId};

use crate::error::HookError;

/// An ordered key as raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(Vec<u8>);

impl Key {
    /// Creates a key from bytes.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Returns the key bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the key length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns true when the key has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<u8>> for Key {
    fn from(bytes: Vec<u8>) -> Self {
        Self::new(bytes)
    }
}

impl From<&[u8]> for Key {
    fn from(bytes: &[u8]) -> Self {
        Self::new(bytes.to_vec())
    }
}

impl From<&str> for Key {
    fn from(text: &str) -> Self {
        Self::new(text.as_bytes().to_vec())
    }
}

impl From<String> for Key {
    fn from(text: String) -> Self {
        Self::new(text.into_bytes())
    }
}

/// The value of one row before the transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorRow {
    value: Vec<u8>,
    key_version: KeyVersion,
}

impl PriorRow {
    /// Creates a prior row.
    #[must_use]
    pub fn new(value: impl Into<Vec<u8>>, key_version: KeyVersion) -> Self {
        Self {
            value: value.into(),
            key_version,
        }
    }

    /// Returns the row value.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }

    /// Returns the key version of the row.
    #[must_use]
    pub const fn key_version(&self) -> KeyVersion {
        self.key_version
    }
}

/// The operation of one staged write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpKind {
    /// The operation writes the given value.
    Put(Vec<u8>),
    /// The operation removes the key.
    Delete,
}

impl OpKind {
    /// Returns the put value.
    #[must_use]
    pub fn value(&self) -> Option<&[u8]> {
        match self {
            Self::Put(value) => Some(value),
            Self::Delete => None,
        }
    }

    /// Returns true when the operation is a delete.
    #[must_use]
    pub const fn is_delete(&self) -> bool {
        matches!(self, Self::Delete)
    }

    const fn tag(&self) -> u8 {
        match self {
            Self::Put(_) => 0,
            Self::Delete => 1,
        }
    }
}

/// One operation in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedOp {
    key: Key,
    kind: OpKind,
    base_version: KeyVersion,
    prior: Option<PriorRow>,
}

impl StagedOp {
    /// Creates a put operation.
    #[must_use]
    pub fn put(
        key: Key,
        value: impl Into<Vec<u8>>,
        base_version: KeyVersion,
        prior: Option<PriorRow>,
    ) -> Self {
        Self {
            key,
            kind: OpKind::Put(value.into()),
            base_version,
            prior,
        }
    }

    /// Creates a delete operation.
    #[must_use]
    pub fn delete(key: Key, base_version: KeyVersion, prior: Option<PriorRow>) -> Self {
        Self {
            key,
            kind: OpKind::Delete,
            base_version,
            prior,
        }
    }

    /// Returns the operation key.
    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the operation kind.
    #[must_use]
    pub const fn kind(&self) -> &OpKind {
        &self.kind
    }

    /// Returns the put value.
    #[must_use]
    pub fn value(&self) -> Option<&[u8]> {
        self.kind.value()
    }

    /// Returns the base version of the key.
    #[must_use]
    pub const fn base_version(&self) -> KeyVersion {
        self.base_version
    }

    /// Returns the pre-transaction row for the key.
    #[must_use]
    pub const fn prior(&self) -> Option<&PriorRow> {
        self.prior.as_ref()
    }

    /// Returns true when the operation is a delete.
    #[must_use]
    pub const fn is_delete(&self) -> bool {
        self.kind.is_delete()
    }
}

/// One batch with its pre-transaction values and final staged values.
///
/// The transaction keeps the operations in declaration order. It computes the
/// final staged value for each touched key. One key can appear more than one
/// time. Every operation for the key must carry the same prior value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    shard_id: ShardId,
    ops: Vec<StagedOp>,
    pre_state: BTreeMap<Key, Option<PriorRow>>,
    staged_state: BTreeMap<Key, Option<Vec<u8>>>,
}

impl Transaction {
    /// Creates an empty transaction for one shard.
    #[must_use]
    pub fn new(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            ops: Vec::new(),
            pre_state: BTreeMap::new(),
            staged_state: BTreeMap::new(),
        }
    }

    /// Creates a transaction from operations in declaration order.
    ///
    /// # Errors
    ///
    /// Returns `internal.invariant_violation` when one key has two different
    /// prior values.
    pub fn from_ops(shard_id: ShardId, ops: Vec<StagedOp>) -> Result<Self, HookError> {
        let mut transaction = Self::new(shard_id);
        for op in ops {
            transaction.stage(op)?;
        }
        Ok(transaction)
    }

    /// Adds one operation to the end of the batch.
    ///
    /// # Errors
    ///
    /// Returns `internal.invariant_violation` when the key already has a
    /// different prior value.
    pub fn stage(&mut self, op: StagedOp) -> Result<(), HookError> {
        let op_index = u32::try_from(self.ops.len()).unwrap_or(u32::MAX);
        if let Some(existing) = self.pre_state.get(op.key()) {
            if existing != &op.prior {
                return Err(HookError::invariant(
                    "The batch supplies two different prior values for one key.",
                )
                .with_op_index(op_index)
                .with_key(op.key().clone()));
            }
        } else {
            self.pre_state.insert(op.key().clone(), op.prior.clone());
        }
        match op.kind() {
            OpKind::Put(value) => {
                self.staged_state
                    .insert(op.key().clone(), Some(value.clone()));
            }
            OpKind::Delete => {
                self.staged_state.insert(op.key().clone(), None);
            }
        }
        self.ops.push(op);
        Ok(())
    }

    /// Returns the shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the operations in declaration order.
    #[must_use]
    pub fn ops(&self) -> &[StagedOp] {
        &self.ops
    }

    /// Returns the number of operations.
    #[must_use]
    pub fn op_count(&self) -> usize {
        self.ops.len()
    }

    /// Returns the pre-transaction value for every touched key.
    #[must_use]
    pub const fn pre_state(&self) -> &BTreeMap<Key, Option<PriorRow>> {
        &self.pre_state
    }

    /// Returns the final staged value for every touched key.
    #[must_use]
    pub const fn staged_state(&self) -> &BTreeMap<Key, Option<Vec<u8>>> {
        &self.staged_state
    }

    /// Returns a deterministic fingerprint of the batch bytes.
    #[must_use]
    pub fn fingerprint(&self) -> Hash32 {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"oxkv-hooks-batch/v1\0");
        push_len(&mut bytes, self.shard_id.as_bytes().len());
        bytes.extend_from_slice(self.shard_id.as_bytes());
        push_len(&mut bytes, self.ops.len());
        for op in &self.ops {
            bytes.push(op.kind.tag());
            push_len(&mut bytes, op.key.len());
            bytes.extend_from_slice(op.key.as_bytes());
            match op.kind() {
                OpKind::Put(value) => {
                    bytes.push(0);
                    push_len(&mut bytes, value.len());
                    bytes.extend_from_slice(value);
                }
                OpKind::Delete => bytes.push(1),
            }
            bytes.extend_from_slice(&op.base_version.get().to_be_bytes());
            match &op.prior {
                Some(prior) => {
                    bytes.push(1);
                    push_len(&mut bytes, prior.value().len());
                    bytes.extend_from_slice(prior.value());
                    bytes.extend_from_slice(&prior.key_version().get().to_be_bytes());
                }
                None => bytes.push(0),
            }
        }
        Hash32::of(&bytes)
    }
}

fn push_len(bytes: &mut Vec<u8>, len: usize) {
    bytes.extend_from_slice(&u64::try_from(len).unwrap_or(u64::MAX).to_be_bytes());
}
