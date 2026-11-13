//! Conflict detail and client rebase.
//!
//! A conflict is terminal for one intent. A client rebase must re-read the key
//! and use the current key version as the new base. A rebase must use a new
//! idempotency key. The old conflict stays visible in the receipt ledger.

use oxkv_contracts::receipt::{IdempotencyKey, RequestId};
use oxkv_contracts::version::KeyVersion;

use crate::request::{Key, Mutation, WriteRequest};

/// The detail of one version conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictDetail {
    key: Key,
    base_version: KeyVersion,
    current_key_version: KeyVersion,
}

impl ConflictDetail {
    /// Creates a conflict detail.
    #[must_use]
    pub const fn new(key: Key, base_version: KeyVersion, current_key_version: KeyVersion) -> Self {
        Self {
            key,
            base_version,
            current_key_version,
        }
    }

    /// Returns the conflicting key.
    #[must_use]
    pub const fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the base version that the request named.
    #[must_use]
    pub const fn base_version(&self) -> KeyVersion {
        self.base_version
    }

    /// Returns the current key version.
    #[must_use]
    pub const fn current_key_version(&self) -> KeyVersion {
        self.current_key_version
    }

    /// Returns the base version for a rebase.
    #[must_use]
    pub const fn next_base_version(&self) -> KeyVersion {
        self.current_key_version
    }
}

/// An error from a rebase.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RebaseError {
    /// The new idempotency key equals the old key.
    #[error("A rebase must use a new idempotency key.")]
    SameIdempotencyKey,
    /// The current versions do not name the key.
    #[error("The rebase has no current version for the key {key}.")]
    MissingBase {
        /// The key without a current version.
        key: Key,
    },
}

impl RebaseError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SameIdempotencyKey => "write.idempotency_key_reuse",
            Self::MissingBase { .. } => "write.missing_base_version",
        }
    }
}

/// Rebuilds a request with new bases and a new idempotency key.
///
/// The caller must re-read the current key versions and pass them in
/// `current_versions`. The old conflict remains in the ledger.
///
/// # Errors
///
/// Returns [`RebaseError::SameIdempotencyKey`] when the new key equals the old
/// key. Returns [`RebaseError::MissingBase`] when a current version is absent.
pub fn rebase_request(
    request: &WriteRequest,
    request_id: RequestId,
    idempotency_key: IdempotencyKey,
    current_versions: &[(Key, KeyVersion)],
) -> Result<WriteRequest, RebaseError> {
    if idempotency_key == *request.idempotency_key() {
        return Err(RebaseError::SameIdempotencyKey);
    }

    let mutations = request
        .mutations()
        .iter()
        .map(|mutation| {
            let base_version = current_versions
                .iter()
                .find(|(key, _)| key == mutation.key())
                .map(|(_, version)| *version)
                .ok_or_else(|| RebaseError::MissingBase {
                    key: mutation.key().clone(),
                })?;
            Ok(Mutation::from_parts(
                mutation.key().clone(),
                mutation.shard_id().clone(),
                Some(base_version),
                mutation.operation().clone(),
            ))
        })
        .collect::<Result<Vec<_>, RebaseError>>()?;

    Ok(WriteRequest::new(request_id, idempotency_key, mutations))
}
