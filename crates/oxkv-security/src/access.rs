//! The read access log.
//!
//! A read scope evaluation writes to this log. It never writes to the write
//! audit chain. The access log has its own retention class.

use core::fmt;

use oxkv_contracts::receipt::RequestId;
use oxkv_contracts::version::{Hash32, ScopeIdentity, ShardId};

use crate::auth::{LogicalTime, PrincipalId};

/// A retention class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RetentionClass {
    /// The write audit chain retention.
    Audit,
    /// The read access log retention.
    AccessLog,
}

impl RetentionClass {
    /// Every retention class.
    pub const ALL: [Self; 2] = [Self::Audit, Self::AccessLog];

    /// Returns the canonical retention class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Audit => "audit_retention",
            Self::AccessLog => "access_log_retention",
        }
    }
}

impl fmt::Display for RetentionClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The decision of one read scope evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AccessDecision {
    /// The scope admits the read.
    Allowed,
    /// The scope refuses the read.
    Denied,
}

impl AccessDecision {
    /// Returns the canonical decision name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Allowed => "Allowed",
            Self::Denied => "Denied",
        }
    }
}

impl fmt::Display for AccessDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One read scope evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessEvent {
    /// The request identity.
    pub request_id: RequestId,
    /// The authenticated principal.
    pub principal: PrincipalId,
    /// The shard that served the read.
    pub shard_id: ShardId,
    /// The digest of the requested key. The log does not store the key.
    pub key_digest: Hash32,
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
    /// The read decision.
    pub decision: AccessDecision,
    /// The coarse reason code. An allowed read has no reason code.
    pub reason_code: Option<&'static str>,
    /// The injected logical time.
    pub observed_at: LogicalTime,
}

/// One entry in the access log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessEntry {
    /// The one-based position in the access log.
    pub access_sequence: u64,
    /// The read scope evaluation.
    pub event: AccessEvent,
}

impl AccessEntry {
    /// Returns the access log retention class.
    #[must_use]
    pub const fn retention_class() -> RetentionClass {
        RetentionClass::AccessLog
    }
}

/// A reference to one access log entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccessReference {
    /// The one-based position in the access log.
    pub access_sequence: u64,
}

/// An append-only access log with its own retention class.
#[derive(Debug, Clone, Default)]
pub struct AccessLog {
    entries: Vec<AccessEntry>,
}

impl AccessLog {
    /// Creates an empty access log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the access log retention class.
    #[must_use]
    pub const fn retention_class() -> RetentionClass {
        RetentionClass::AccessLog
    }

    /// Appends one read scope evaluation.
    pub fn append(&mut self, event: AccessEvent) -> AccessEntry {
        let next = self
            .entries
            .len()
            .checked_add(1)
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(u64::MAX);
        let entry = AccessEntry {
            access_sequence: next,
            event,
        };
        self.entries.push(entry.clone());
        entry
    }

    /// Returns every entry in sequence order.
    #[must_use]
    pub fn entries(&self) -> &[AccessEntry] {
        &self.entries
    }

    /// Returns the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when the log has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
