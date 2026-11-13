//! Named scope errors and their registry codes.
//!
//! Every error carries one code from the 1.0 error registry. A caller can
//! match the variant or the code. The text uses short active sentences.

use oxkv_contracts::version::{CommitVersion, PageId, ScopeIdentity, ShardId};

/// One admission limit from the scope declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AdmissionLimit {
    /// The maximum member count.
    Members,
    /// The maximum member byte count.
    Bytes,
    /// The maximum shard count.
    Shards,
}

impl AdmissionLimit {
    /// Returns the qualified limit name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Members => "max_members",
            Self::Bytes => "max_bytes",
            Self::Shards => "max_shards",
        }
    }
}

impl core::fmt::Display for AdmissionLimit {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// An error from scope declaration, admission, or diff application.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    /// The declaration has no user table prefix.
    #[error("The scope has no user table prefix.")]
    EmptyScope,
    /// The declaration names an authority system namespace.
    #[error("The table {name} is an authority system namespace.")]
    SystemNamespace {
        /// The rejected table name.
        name: String,
    },
    /// The table name does not match the user table grammar.
    #[error("The table name {name} does not match the table name grammar.")]
    InvalidTableName {
        /// The rejected table name.
        name: String,
    },
    /// The rule set selector does not match the selector grammar.
    #[error("The rule set selector {value} does not match the selector grammar.")]
    InvalidRuleSetSelector {
        /// The rejected selector text.
        value: String,
    },
    /// The projection field does not match the field name grammar.
    #[error("The field name {field} does not match the field name grammar.")]
    InvalidFieldName {
        /// The rejected field name.
        field: String,
    },
    /// One key has two records in the same diff interval.
    #[error("The diff interval has two records for the key {key:?}.")]
    DuplicateDiffRecord {
        /// The duplicated key.
        key: Vec<u8>,
    },
    /// A remove record names a key that is not a member.
    #[error("The remove record names the key {key:?}. The key is not a member.")]
    RemoveUnknownMember {
        /// The key in the remove record.
        key: Vec<u8>,
    },
    /// A commit version would decrease.
    #[error("The shard {shard_id:?} commit version would decrease from {current} to {proposed}.")]
    VersionRegression {
        /// The shard with the regression.
        shard_id: ShardId,
        /// The current commit version.
        current: CommitVersion,
        /// The proposed commit version.
        proposed: CommitVersion,
    },
    /// The shard plan has no entry for the shard.
    #[error("The shard plan has no entry for the shard {shard_id:?}.")]
    UnknownShard {
        /// The unknown shard.
        shard_id: ShardId,
    },
    /// The shard map does not assign the table.
    #[error("The shard map does not assign the table {table}.")]
    UnmappedTable {
        /// The unassigned table.
        table: String,
    },
    /// The shard map assigns the table more than once.
    #[error("The shard map assigns the table {table} more than once.")]
    DuplicateTableAssignment {
        /// The table with two assignments.
        table: String,
    },
    /// The scope exceeds an admission limit.
    #[error(
        "The scope exceeds {limit} with value {limit_value}. The observed value is {observed}."
    )]
    OverLimit {
        /// The limit that the scope exceeds.
        limit: AdmissionLimit,
        /// The declared limit value.
        limit_value: u64,
        /// The observed value.
        observed: u64,
    },
    /// The scope does not fit the device budget.
    #[error("The scope needs {required} device bytes. The device budget is {available} bytes.")]
    DeviceBudgetExceeded {
        /// The required byte count.
        required: u64,
        /// The available byte count.
        available: u64,
    },
    /// The scope does not fit the hydration budget.
    #[error("The scope needs {required} hydration bytes. The hydration budget is {limit} bytes.")]
    HydrationBudgetExceeded {
        /// The required overhead byte count.
        required: u64,
        /// The available overhead byte count.
        limit: u64,
    },
    /// The computed identity does not match the expected identity.
    #[error("The scope identity {found} does not match the expected identity {expected}.")]
    IdentityMismatch {
        /// The expected identity.
        expected: ScopeIdentity,
        /// The computed identity.
        found: ScopeIdentity,
    },
    /// A diff interval sequence does not follow the previous interval.
    #[error("The diff interval sequence {found} does not follow the expected sequence {expected}.")]
    DiffSequence {
        /// The expected interval sequence.
        expected: u64,
        /// The found interval sequence.
        found: u64,
    },
    /// A diff interval base does not match the current scope version.
    #[error("The diff interval base does not match the current scope version.")]
    BaseVersionMismatch,
    /// The diff byte format has an unknown version.
    #[error("The diff byte version {found} is not supported.")]
    UnsupportedDiffVersion {
        /// The found byte format version.
        found: u8,
    },
    /// The diff bytes are malformed.
    #[error("The diff bytes are malformed.")]
    MalformedDiff,
    /// A byte count exceeds the `u64` range.
    #[error("The byte count exceeds the u64 range.")]
    ByteCountOverflow,
    /// The scope version vector has no entry for a planned shard.
    #[error("The pin has no entry for the shard {shard_id:?}.")]
    MissingPinShard {
        /// The shard without a pin entry.
        shard_id: ShardId,
    },
    /// A user scope received a system key.
    #[error("The user scope cannot contain the system key {key:?}.")]
    SystemKey {
        /// The rejected system key.
        key: Vec<u8>,
    },
}

impl ScopeError {
    /// Returns the canonical error code from the 1.0 registry.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EmptyScope | Self::InvalidTableName { .. } => "key.malformed",
            Self::SystemNamespace { .. } => "manifest.reserved_table_name",
            Self::InvalidRuleSetSelector { .. } => "policy.parse_error",
            Self::InvalidFieldName { .. } => "manifest.unknown_field",
            Self::UnmappedTable { .. } => "map.table_unmapped",
            Self::UnknownShard { .. } => "scope.unknown_shard",
            Self::MissingPinShard { .. } => "scope.missing_pin_shard",
            Self::OverLimit { .. } => "scope.over_limit",
            Self::DeviceBudgetExceeded { .. } => "scope.device_budget_exceeded",
            Self::HydrationBudgetExceeded { .. } => "scope.hydration_budget_exceeded",
            Self::IdentityMismatch { .. } => "scope.identity_mismatch",
            Self::UnsupportedDiffVersion { .. } => "sync.unsupported_version",
            Self::DuplicateDiffRecord { .. }
            | Self::VersionRegression { .. }
            | Self::DiffSequence { .. }
            | Self::BaseVersionMismatch
            | Self::MalformedDiff => "sync.cursor_invalid",
            Self::DuplicateTableAssignment { .. } => "map.duplicate_id",
            Self::RemoveUnknownMember { .. } => "scope.remove_unknown_member",
            Self::SystemKey { .. } => "scope.system_key_rejected",
            Self::ByteCountOverflow => "format.page_too_large",
        }
    }
}

/// An error from replica residency eviction.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResidencyError {
    /// The residency has no page with the given identity.
    #[error("The residency has no page {page_id:?}.")]
    PageNotFound {
        /// The missing page.
        page_id: PageId,
    },
    /// The page holds an in-scope logical member.
    #[error("The page {page_id:?} holds the in-scope member {key:?}.")]
    InScopeMember {
        /// The page with the member.
        page_id: PageId,
        /// The in-scope key.
        key: Vec<u8>,
    },
    /// The page is not out of scope and not superseded.
    #[error("The page {page_id:?} is not out of scope and not superseded.")]
    NotEvictable {
        /// The page that cannot leave.
        page_id: PageId,
    },
}

impl ResidencyError {
    /// Returns the canonical error code from the 1.0 registry.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PageNotFound { .. } => "key.not_found",
            Self::InScopeMember { .. } | Self::NotEvictable { .. } => {
                "replication.only_copy_eviction"
            }
        }
    }
}

/// An error from an exact shard query.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    /// A targeted shard is unreachable.
    #[error("The shard {shard_id:?} is unreachable.")]
    ShardUnreachable {
        /// The unreachable shard.
        shard_id: ShardId,
    },
    /// A shard served a version below the pin.
    #[error("The shard {shard_id:?} served version {served}. The pin needs {requested}.")]
    ShardNotAtPin {
        /// The shard with the wrong version.
        shard_id: ShardId,
        /// The requested commit version.
        requested: CommitVersion,
        /// The served commit version.
        served: CommitVersion,
    },
    /// The pin has no entry for a planned shard.
    #[error("The pin has no entry for the shard {shard_id:?}.")]
    PinIncomplete {
        /// The shard without a pin entry.
        shard_id: ShardId,
    },
    /// The plan has no shard with the given identity.
    #[error("The plan has no shard {shard_id:?}.")]
    UnknownShard {
        /// The unknown shard.
        shard_id: ShardId,
    },
    /// Two responses name the same shard.
    #[error("The responses name the shard {shard_id:?} more than once.")]
    DuplicateResponse {
        /// The duplicated shard.
        shard_id: ShardId,
    },
    /// Two rows name the same key.
    #[error("The result has two rows for the key {key:?}.")]
    DuplicateRow {
        /// The duplicated key.
        key: Vec<u8>,
    },
    /// A user scope result received a system key.
    #[error("The user scope result cannot contain the system key {key:?}.")]
    SystemKey {
        /// The rejected system key.
        key: Vec<u8>,
    },
}

impl QueryError {
    /// Returns the canonical error code from the 1.0 registry.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ShardUnreachable { .. } | Self::ShardNotAtPin { .. } => "scope.shard_unreachable",
            Self::PinIncomplete { .. } => "scope.missing_pin_shard",
            Self::UnknownShard { .. } => "scope.unknown_shard",
            Self::SystemKey { .. } => "scope.system_key_rejected",
            Self::DuplicateResponse { .. } | Self::DuplicateRow { .. } => {
                "internal.invariant_violation"
            }
        }
    }
}
