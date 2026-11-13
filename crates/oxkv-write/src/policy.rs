//! Pre-write policy hooks.
//!
//! Policy runs after the cross-shard check and after the base-version check.
//! Policy is reject-only. It cannot rewrite the batch.
//!
//! A policy implementation must be pure. It must not read a clock, draw
//! randomness, perform I/O, or keep state.

use oxkv_contracts::version::KeyVersion;

use crate::record::KeyRecord;
use crate::request::{Key, Mutation, WriteRequest};

/// One request row that a policy can inspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyRow<'a> {
    mutation: &'a Mutation,
    current: Option<&'a KeyRecord>,
}

impl<'a> PolicyRow<'a> {
    /// Creates a policy row.
    pub(crate) const fn new(mutation: &'a Mutation, current: Option<&'a KeyRecord>) -> Self {
        Self { mutation, current }
    }

    /// Returns the request mutation.
    #[must_use]
    pub const fn mutation(&self) -> &'a Mutation {
        self.mutation
    }

    /// Returns the current record. The value is `None` when the key is absent.
    #[must_use]
    pub const fn current(&self) -> Option<&'a KeyRecord> {
        self.current
    }

    /// Returns the key.
    #[must_use]
    pub const fn key(&self) -> &'a Key {
        self.mutation.key()
    }

    /// Returns the current key version. An absent key has version zero.
    #[must_use]
    pub fn current_version(&self) -> KeyVersion {
        self.current
            .map_or(KeyVersion::new(0), KeyRecord::key_version)
    }

    /// Returns the current live value. A tombstone returns `None`.
    #[must_use]
    pub fn current_value(&self) -> Option<&'a [u8]> {
        self.current.and_then(KeyRecord::as_value)
    }
}

/// The verdict of one policy evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyVerdict {
    /// The request may commit.
    Allow,
    /// The authority refuses the request for the named reason code.
    Reject {
        /// A registered error code that maps to a `Rejected` receipt.
        reason: &'static str,
    },
}

impl PolicyVerdict {
    /// Returns true when the verdict allows the request.
    #[must_use]
    pub const fn is_allow(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// A pure pre-write policy.
pub trait Policy {
    /// Evaluates one request against the current key records.
    fn evaluate(&self, request: &WriteRequest, rows: &[PolicyRow<'_>]) -> PolicyVerdict;
}

/// A policy that allows every request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AllowAll;

impl Policy for AllowAll {
    fn evaluate(&self, _request: &WriteRequest, _rows: &[PolicyRow<'_>]) -> PolicyVerdict {
        PolicyVerdict::Allow
    }
}
