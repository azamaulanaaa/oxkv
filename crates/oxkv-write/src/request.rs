//! Write request types.
//!
//! A request carries one client identity, one idempotency key, and one base
//! version for each mutated key. The client never assigns a version. The
//! authority assigns a new key version at commit.
//!
//! A mutation without a base version is malformed. The authority rejects it
//! with `write.missing_base_version` before it changes data.

use core::fmt;

use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
use oxkv_contracts::version::{DigestDomain, Hash32, KeyVersion, ShardId};

use crate::util::{push_bytes, push_u64};

/// An opaque key in the ordered key space.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(Vec<u8>);

impl Key {
    /// Creates a key from raw bytes.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Returns the key bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
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

impl AsRef<[u8]> for Key {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl fmt::Display for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// A client identity.
///
/// The authority takes the client identity from the request. The simulator has
/// no authentication layer, so the request carries the identity as an explicit
/// value. The receipt id covers this value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(Vec<u8>);

impl ClientId {
    /// Creates a client identity from raw bytes.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Returns the raw identity bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for ClientId {
    fn from(bytes: Vec<u8>) -> Self {
        Self::new(bytes)
    }
}

impl From<&[u8]> for ClientId {
    fn from(bytes: &[u8]) -> Self {
        Self::new(bytes.to_vec())
    }
}

/// The operation that one mutation applies to one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOperation {
    /// Stores a value.
    Put(Vec<u8>),
    /// Removes the value and leaves a tombstone.
    Delete,
}

impl WriteOperation {
    /// Creates a put operation.
    #[must_use]
    pub fn put(value: impl Into<Vec<u8>>) -> Self {
        Self::Put(value.into())
    }

    /// Creates a delete operation.
    #[must_use]
    pub const fn delete() -> Self {
        Self::Delete
    }

    /// Returns the stable operation tag.
    ///
    /// The tag does not depend on the Rust enum discriminant.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Put(_) => 1,
            Self::Delete => 2,
        }
    }

    /// Returns true when the operation deletes the value.
    #[must_use]
    pub const fn is_delete(&self) -> bool {
        matches!(self, Self::Delete)
    }

    /// Returns the value of a put operation.
    #[must_use]
    pub fn value(&self) -> Option<&[u8]> {
        match self {
            Self::Put(value) => Some(value),
            Self::Delete => None,
        }
    }
}

/// One key mutation with its base version and target shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutation {
    key: Key,
    shard_id: ShardId,
    base_version: Option<KeyVersion>,
    operation: WriteOperation,
}

impl Mutation {
    /// Creates a mutation from its parts.
    pub(crate) const fn from_parts(
        key: Key,
        shard_id: ShardId,
        base_version: Option<KeyVersion>,
        operation: WriteOperation,
    ) -> Self {
        Self {
            key,
            shard_id,
            base_version,
            operation,
        }
    }

    /// Creates a put mutation.
    #[must_use]
    pub fn put(
        key: Key,
        shard_id: ShardId,
        base_version: KeyVersion,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            key,
            shard_id,
            base_version: Some(base_version),
            operation: WriteOperation::put(value),
        }
    }

    /// Creates a delete mutation.
    #[must_use]
    pub fn delete(key: Key, shard_id: ShardId, base_version: KeyVersion) -> Self {
        Self {
            key,
            shard_id,
            base_version: Some(base_version),
            operation: WriteOperation::delete(),
        }
    }

    /// Creates a mutation without a base version.
    ///
    /// Use this constructor only to model a malformed request. The authority
    /// rejects the request with `write.missing_base_version`.
    #[must_use]
    pub fn without_base(key: Key, shard_id: ShardId, operation: WriteOperation) -> Self {
        Self {
            key,
            shard_id,
            base_version: None,
            operation,
        }
    }

    /// Returns the key.
    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the target shard.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the base version. A value of `None` means the request is
    /// malformed.
    #[must_use]
    pub const fn base_version(&self) -> Option<KeyVersion> {
        self.base_version
    }

    /// Returns the operation.
    #[must_use]
    pub const fn operation(&self) -> &WriteOperation {
        &self.operation
    }

    /// Returns a copy with another base version.
    #[must_use]
    pub fn with_base_version(mut self, base_version: KeyVersion) -> Self {
        self.base_version = Some(base_version);
        self
    }

    /// Returns a copy with another operation.
    #[must_use]
    pub fn with_operation(mut self, operation: WriteOperation) -> Self {
        self.operation = operation;
        self
    }
}

/// One versioned write request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    request_id: RequestId,
    client_id: ClientId,
    idempotency_key: IdempotencyKey,
    mutations: Vec<Mutation>,
}

impl WriteRequest {
    /// Creates a write request.
    ///
    /// The caller supplies the client identity. The constructor has no
    /// default identity.
    #[must_use]
    pub fn new(
        request_id: RequestId,
        client_id: ClientId,
        idempotency_key: IdempotencyKey,
        mutations: impl IntoIterator<Item = Mutation>,
    ) -> Self {
        Self {
            request_id,
            client_id,
            idempotency_key,
            mutations: mutations.into_iter().collect(),
        }
    }

    /// Returns the request identity.
    #[must_use]
    pub const fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Returns the client identity.
    #[must_use]
    pub const fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    /// Returns the idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    /// Returns the mutations in request order.
    #[must_use]
    pub fn mutations(&self) -> &[Mutation] {
        &self.mutations
    }

    /// Returns true when the request has no mutation.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mutations.is_empty()
    }

    /// Returns the keys in request order.
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.mutations.iter().map(Mutation::key)
    }

    /// Returns the first mutation that has no base version.
    #[must_use]
    pub fn first_missing_base(&self) -> Option<&Key> {
        self.mutations
            .iter()
            .find(|mutation| mutation.base_version().is_none())
            .map(Mutation::key)
    }

    /// Returns the first mutation that targets a shard other than `shard_id`.
    #[must_use]
    pub fn cross_shard_key(&self, shard_id: &ShardId) -> Option<&Key> {
        self.mutations
            .iter()
            .find(|mutation| mutation.shard_id() != shard_id)
            .map(Mutation::key)
    }

    /// Returns the canonical body digest.
    ///
    /// The digest covers the mutation list only. It excludes the request
    /// identity, so a retry with a new request identity still matches its
    /// idempotency key.
    #[must_use]
    pub fn body_digest(&self) -> Hash32 {
        let mut bytes = Vec::new();
        push_bytes(&mut bytes, b"oxkv/write-body/v1");
        for mutation in &self.mutations {
            push_bytes(&mut bytes, mutation.key().as_bytes());
            push_bytes(&mut bytes, mutation.shard_id().as_bytes());
            match mutation.base_version() {
                Some(base_version) => {
                    bytes.push(1);
                    push_u64(&mut bytes, base_version.get());
                }
                None => bytes.push(0),
            }
            bytes.push(mutation.operation().tag());
            if let Some(value) = mutation.operation().value() {
                push_bytes(&mut bytes, value);
            }
        }
        Hash32::of_domain(DigestDomain::Content, &bytes)
    }
}
