//! The canonical error and receipt registry.
//!
//! Every error code in the 1.0 contract appears once in [`ERROR_REGISTRY`].
//! Each entry names one kind, one receipt state, one retry class, one terminal
//! flag, and one wire shape.
//!
//! A code with no receipt is terminal for the call. A code with a `Queued`
//! receipt is live. A code with a terminal receipt is terminal.
//!
//! The registry is a candidate until the 1.0 freeze. A new code is additive.
//! A removed code stays reserved.

use core::fmt;

use crate::receipt::ReceiptState;

/// The subsystem kind of one error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ErrorKind {
    /// A key encoding or key value error.
    Key,
    /// A snapshot or page format error.
    Format,
    /// A manifest error.
    Manifest,
    /// A policy error.
    Policy,
    /// A rule lifecycle error.
    Rule,
    /// A write path error.
    Write,
    /// A shard error.
    Shard,
    /// A receipt error.
    Receipt,
    /// An outbox error.
    Outbox,
    /// A projection error.
    Projection,
    /// A scope error.
    Scope,
    /// A sync wire error.
    Sync,
    /// A read error.
    Read,
    /// A search error.
    Search,
    /// A query budget error.
    Query,
    /// A shard map error.
    Map,
    /// A replication error.
    Replication,
    /// An event error.
    Event,
    /// A consumer error.
    Consumer,
    /// An authentication error.
    Auth,
    /// An audit error.
    Audit,
    /// An operations error.
    Operations,
    /// An RPC error.
    Rpc,
    /// An internal invariant error.
    Internal,
}

impl ErrorKind {
    /// Returns the canonical kind name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Format => "format",
            Self::Manifest => "manifest",
            Self::Policy => "policy",
            Self::Rule => "rule",
            Self::Write => "write",
            Self::Shard => "shard",
            Self::Receipt => "receipt",
            Self::Outbox => "outbox",
            Self::Projection => "projection",
            Self::Scope => "scope",
            Self::Sync => "sync",
            Self::Read => "read",
            Self::Search => "search",
            Self::Query => "query",
            Self::Map => "map",
            Self::Replication => "replication",
            Self::Event => "event",
            Self::Consumer => "consumer",
            Self::Auth => "auth",
            Self::Audit => "audit",
            Self::Operations => "operations",
            Self::Rpc => "rpc",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The retry class of one error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RetryClass {
    /// Do not retry the request.
    Never,
    /// Fetch the page again.
    Refetch,
    /// Regenerate the artifact.
    Regenerate,
    /// Retry after a fix.
    AfterFix,
    /// Retry after an approval.
    AfterApproval,
    /// Retry after a rollout.
    AfterRollout,
    /// Retry after a re-read.
    AfterReRead,
    /// Resubmit the request.
    Resubmit,
    /// Retry after the outbox drains.
    AfterDrain,
    /// Retry after a backoff.
    Backoff,
    /// Retry after a rebuild.
    AfterRebuild,
    /// Retry after a catch-up.
    AfterCatchUp,
    /// Fall back to a snapshot.
    SnapshotFallback,
    /// Request a new cursor.
    NewCursor,
    /// Retry after authentication.
    AfterAuth,
    /// Request a new pin.
    NewPin,
    /// Request a new query.
    NewQuery,
    /// Narrow the query.
    NarrowQuery,
    /// Rebuild the index.
    Rebuild,
    /// Retry after a lease re-acquire.
    AfterReAcquire,
    /// Retry after a lease hint.
    AfterHint,
    /// The operator must act.
    OperatorAction,
    /// Continue with a cursor.
    ContinueWithCursor,
    /// Backfill from a snapshot.
    SnapshotBackfill,
    /// Move the record to a dead-letter queue.
    DeadLetter,
    /// Retry after a credential refresh.
    AfterRefresh,
    /// Restart the backup.
    RestartBackup,
    /// Restart the restore.
    RestartRestore,
    /// Resume the migration.
    ResumeMigration,
    /// Retry after a rebase.
    AfterRebase,
    /// Retry after a quota change.
    AfterQuota,
    /// Regenerate the client.
    RegenerateClient,
    /// Reconnect the transport.
    Reconnect,
    /// Retry after maintenance.
    AfterMaintenance,
    /// Retry after a change.
    AfterChange,
    /// Retry after a policy sync.
    AfterPolicySync,
}

impl RetryClass {
    /// Every retry class.
    pub const ALL: &'static [Self] = &[
        Self::Never,
        Self::Refetch,
        Self::Regenerate,
        Self::AfterFix,
        Self::AfterApproval,
        Self::AfterRollout,
        Self::AfterReRead,
        Self::Resubmit,
        Self::AfterDrain,
        Self::Backoff,
        Self::AfterRebuild,
        Self::AfterCatchUp,
        Self::SnapshotFallback,
        Self::NewCursor,
        Self::AfterAuth,
        Self::NewPin,
        Self::NewQuery,
        Self::NarrowQuery,
        Self::Rebuild,
        Self::AfterReAcquire,
        Self::AfterHint,
        Self::OperatorAction,
        Self::ContinueWithCursor,
        Self::SnapshotBackfill,
        Self::DeadLetter,
        Self::AfterRefresh,
        Self::RestartBackup,
        Self::RestartRestore,
        Self::ResumeMigration,
        Self::AfterRebase,
        Self::AfterQuota,
        Self::RegenerateClient,
        Self::Reconnect,
        Self::AfterMaintenance,
        Self::AfterChange,
        Self::AfterPolicySync,
    ];

    /// Returns the canonical retry class text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Never => "Never",
            Self::Refetch => "Refetch",
            Self::Regenerate => "Regenerate",
            Self::AfterFix => "After fix",
            Self::AfterApproval => "After approval",
            Self::AfterRollout => "After rollout",
            Self::AfterReRead => "After re-read",
            Self::Resubmit => "Resubmit",
            Self::AfterDrain => "After drain",
            Self::Backoff => "Backoff",
            Self::AfterRebuild => "After rebuild",
            Self::AfterCatchUp => "After catch-up",
            Self::SnapshotFallback => "Snapshot fallback",
            Self::NewCursor => "New cursor",
            Self::AfterAuth => "After auth",
            Self::NewPin => "New pin",
            Self::NewQuery => "New query",
            Self::NarrowQuery => "Narrow query",
            Self::Rebuild => "Rebuild",
            Self::AfterReAcquire => "After re-acquire",
            Self::AfterHint => "After hint",
            Self::OperatorAction => "Operator action",
            Self::ContinueWithCursor => "Continue with cursor",
            Self::SnapshotBackfill => "Snapshot backfill",
            Self::DeadLetter => "Dead-letter",
            Self::AfterRefresh => "After refresh",
            Self::RestartBackup => "Restart backup",
            Self::RestartRestore => "Restart restore",
            Self::ResumeMigration => "Resume migration",
            Self::AfterRebase => "After rebase",
            Self::AfterQuota => "After quota",
            Self::RegenerateClient => "Regenerate client",
            Self::Reconnect => "Reconnect",
            Self::AfterMaintenance => "After maintenance",
            Self::AfterChange => "After change",
            Self::AfterPolicySync => "After policy sync",
        }
    }
}

impl fmt::Display for RetryClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The wire shape of one error code.
///
/// The shape states what a client receives for one code. A code with no data
/// beyond the code text uses [`WireShape::Code`]. A code with a reason, a
/// field, an operation, or a retry hint uses [`WireShape::CodeDetail`]. A fixed
/// human-readable message is not a detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WireShape {
    /// The wire value is the code text only.
    Code,
    /// The wire value is the code text and a structured detail.
    CodeDetail,
}

impl WireShape {
    /// Every wire shape.
    pub const ALL: &'static [Self] = &[Self::Code, Self::CodeDetail];

    /// Returns the canonical shape name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::CodeDetail => "code_detail",
        }
    }
}

impl fmt::Display for WireShape {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One row of the canonical error registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorSpec {
    /// The lowercase dot-separated error code.
    pub code: &'static str,
    /// The subsystem kind.
    pub kind: ErrorKind,
    /// The receipt state. A value of `None` means "no receipt".
    pub receipt: Option<ReceiptState>,
    /// The retry class.
    pub retry: RetryClass,
    /// True when the error ends the operation in place.
    pub terminal: bool,
    /// The wire shape of the error value.
    pub wire_shape: WireShape,
    /// The current key version field for a `Conflict` receipt.
    pub conflict_key_version_field: Option<&'static str>,
    /// True when a `Rejected` receipt consumes the idempotency key.
    pub idempotency_consumed: Option<bool>,
    /// The deadline field for a `Queued` receipt.
    pub queued_deadline: Option<&'static str>,
}

impl ErrorSpec {
    /// Returns the full diagnostic text for one row.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} kind={} receipt={} retry={} terminal={} wire_shape={}",
            self.code,
            self.kind,
            self.receipt
                .map_or_else(|| "none".to_owned(), |state| state.name().to_owned()),
            self.retry,
            self.terminal,
            self.wire_shape
        )
    }
}

impl fmt::Display for ErrorSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

/// A checked reference to one registry row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode(&'static ErrorSpec);

impl ErrorCode {
    /// Looks up a code.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        lookup(code).map(Self)
    }

    /// Returns the registry row.
    #[must_use]
    pub const fn spec(self) -> &'static ErrorSpec {
        self.0
    }

    /// Returns the code text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0.code
    }

    /// Returns the subsystem kind.
    #[must_use]
    pub const fn kind(self) -> ErrorKind {
        self.0.kind
    }

    /// Returns the receipt state.
    #[must_use]
    pub const fn receipt(self) -> Option<ReceiptState> {
        self.0.receipt
    }

    /// Returns the retry class.
    #[must_use]
    pub const fn retry(self) -> RetryClass {
        self.0.retry
    }

    /// Returns true when the error is terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        self.0.terminal
    }

    /// Returns the wire shape.
    #[must_use]
    pub const fn wire_shape(self) -> WireShape {
        self.0.wire_shape
    }

    /// Returns the current key version field for a `Conflict` receipt.
    #[must_use]
    pub const fn conflict_key_version_field(self) -> Option<&'static str> {
        self.0.conflict_key_version_field
    }

    /// Returns true when a `Rejected` receipt consumes the idempotency key.
    #[must_use]
    pub const fn idempotency_consumed(self) -> Option<bool> {
        self.0.idempotency_consumed
    }

    /// Returns the deadline field for a `Queued` receipt.
    #[must_use]
    pub const fn queued_deadline(self) -> Option<&'static str> {
        self.0.queued_deadline
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.code)
    }
}

/// An error from registry lookup or validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorRegistryError {
    /// The code is not registered.
    #[error("The error code is not registered: {code}.")]
    Unknown {
        /// The unknown code.
        code: String,
    },
    /// The code appears more than one time.
    #[error("The error code is duplicated: {code}.")]
    Duplicate {
        /// The duplicate code.
        code: String,
    },
    /// The code does not use the lowercase dot namespace form.
    #[error("The error code has an invalid shape: {code}.")]
    InvalidShape {
        /// The invalid code.
        code: String,
    },
    /// The code exists but maps to no receipt.
    #[error("The error code maps to no receipt: {code}.")]
    NoReceipt {
        /// The code without a receipt.
        code: String,
    },
}

const fn terminal_for(receipt: Option<ReceiptState>) -> bool {
    !matches!(receipt, Some(ReceiptState::Queued))
}

const fn conflict_field_for(receipt: Option<ReceiptState>) -> Option<&'static str> {
    match receipt {
        Some(ReceiptState::Conflict) => Some("current_key_version"),
        _ => None,
    }
}

const fn idempotency_for(receipt: Option<ReceiptState>) -> Option<bool> {
    match receipt {
        Some(ReceiptState::Rejected) => Some(true),
        _ => None,
    }
}

const fn deadline_for(receipt: Option<ReceiptState>) -> Option<&'static str> {
    match receipt {
        Some(ReceiptState::Queued) => Some("queue_deadline"),
        _ => None,
    }
}

const fn bytes_equal(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

const fn is_lower_letter(byte: u8) -> bool {
    byte >= b'a' && byte <= b'z'
}

const fn is_digit(byte: u8) -> bool {
    byte >= b'0' && byte <= b'9'
}

const fn valid_code_shape(code: &str) -> bool {
    let bytes = code.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut index = 0;
    let mut segment_start = 0;
    let mut dots = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'.' {
            if index == segment_start {
                return false;
            }
            dots += 1;
            segment_start = index + 1;
        } else if index == segment_start {
            if !is_lower_letter(byte) {
                return false;
            }
        } else if !(is_lower_letter(byte) || is_digit(byte) || byte == b'_') {
            return false;
        }
        index += 1;
    }
    if segment_start == bytes.len() {
        return false;
    }
    dots > 0
}

const fn assert_registry(table: &[ErrorSpec]) {
    let mut left = 0;
    while left < table.len() {
        assert!(
            valid_code_shape(table[left].code),
            "an error code does not use the lowercase dot namespace form"
        );
        let mut right = left + 1;
        while right < table.len() {
            assert!(
                !bytes_equal(table[left].code, table[right].code),
                "an error code appears more than one time"
            );
            right += 1;
        }
        left += 1;
    }
}

macro_rules! error_registry {
    ($(($code:literal, $kind:ident, $receipt:expr, $retry:ident, $shape:ident)),* $(,)?) => {
        /// The canonical error registry.
        ///
        /// The table has one row per code. The rows follow section 8.3 of the
        /// 1.0 specification.
        pub static ERROR_REGISTRY: &[ErrorSpec] = &[
            $(ErrorSpec {
                code: $code,
                kind: ErrorKind::$kind,
                receipt: $receipt,
                retry: RetryClass::$retry,
                terminal: terminal_for($receipt),
                wire_shape: WireShape::$shape,
                conflict_key_version_field: conflict_field_for($receipt),
                idempotency_consumed: idempotency_for($receipt),
                queued_deadline: deadline_for($receipt),
            }),*
        ];
    };
}

error_registry!(
    (
        "key.unknown_version",
        Key,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "key.malformed",
        Key,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "key.non_canonical",
        Key,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "key.outside_table",
        Key,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "key.invalid_utf8",
        Key,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "key.too_large",
        Key,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    ("key.not_found", Key, None, Never, CodeDetail),
    (
        "key.reserved_system_marker",
        Key,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "key.unknown_system_id",
        Key,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "format.unsupported_version",
        Format,
        None,
        Never,
        CodeDetail
    ),
    (
        "format.missing_required_feature",
        Format,
        None,
        Never,
        CodeDetail
    ),
    ("format.corrupt_page", Format, None, Refetch, CodeDetail),
    ("format.page_out_of_bounds", Format, None, Never, CodeDetail),
    (
        "format.truncated_snapshot",
        Format,
        None,
        Refetch,
        CodeDetail
    ),
    ("format.missing_provenance", Format, None, Never, CodeDetail),
    ("format.page_too_large", Format, None, Never, CodeDetail),
    (
        "manifest.unsupported_schema",
        Manifest,
        None,
        Never,
        CodeDetail
    ),
    ("manifest.unknown_field", Manifest, None, Never, CodeDetail),
    ("manifest.duplicate_name", Manifest, None, Never, CodeDetail),
    (
        "manifest.reserved_table_name",
        Manifest,
        None,
        Never,
        CodeDetail
    ),
    (
        "manifest.key_schema_missing",
        Manifest,
        None,
        Never,
        CodeDetail
    ),
    (
        "manifest.float_not_allowed",
        Manifest,
        None,
        Never,
        CodeDetail
    ),
    (
        "manifest.index_unknown_field",
        Manifest,
        None,
        Never,
        CodeDetail
    ),
    (
        "manifest.index_scope_unreachable",
        Manifest,
        None,
        Never,
        CodeDetail
    ),
    (
        "manifest.rule_unknown_path",
        Manifest,
        None,
        Never,
        CodeDetail
    ),
    ("manifest.rule_unbounded", Manifest, None, Never, CodeDetail),
    (
        "manifest.operation_cross_shard",
        Manifest,
        None,
        Never,
        CodeDetail
    ),
    (
        "manifest.operation_unknown_field",
        Manifest,
        None,
        Never,
        CodeDetail
    ),
    (
        "manifest.artifact_drift",
        Manifest,
        None,
        Regenerate,
        CodeDetail
    ),
    (
        "manifest.policy_catalog_missing",
        Manifest,
        None,
        Never,
        Code
    ),
    ("manifest.json_syntax", Manifest, None, Never, CodeDetail),
    ("manifest.missing_field", Manifest, None, Never, CodeDetail),
    ("manifest.type_mismatch", Manifest, None, Never, CodeDetail),
    ("manifest.invalid_value", Manifest, None, Never, CodeDetail),
    ("policy.parse_error", Policy, None, Never, CodeDetail),
    (
        "policy.unknown_field",
        Policy,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "policy.budget_exhausted",
        Policy,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "policy.not_total",
        Policy,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    ("policy.ast_unsupported", Policy, None, Never, CodeDetail),
    (
        "policy.regex_not_supported",
        Policy,
        None,
        Never,
        CodeDetail
    ),
    (
        "rule.compile_failed",
        Rule,
        Some(ReceiptState::Rejected),
        AfterFix,
        CodeDetail
    ),
    (
        "rule.dry_run_failed",
        Rule,
        Some(ReceiptState::Rejected),
        AfterFix,
        CodeDetail
    ),
    (
        "rule.approval_required",
        Rule,
        Some(ReceiptState::Rejected),
        AfterApproval,
        CodeDetail
    ),
    (
        "rule.rollout_limit",
        Rule,
        Some(ReceiptState::Rejected),
        AfterRollout,
        CodeDetail
    ),
    (
        "rule.rollout_missing",
        Rule,
        Some(ReceiptState::Rejected),
        AfterRollout,
        Code
    ),
    ("write.missing_base_version", Write, None, Never, CodeDetail),
    (
        "write.stale_base_version",
        Write,
        Some(ReceiptState::Rejected),
        AfterReRead,
        Code
    ),
    (
        "write.version_conflict",
        Write,
        Some(ReceiptState::Conflict),
        AfterRebase,
        CodeDetail
    ),
    (
        "write.idempotency_key_reuse",
        Write,
        None,
        Never,
        CodeDetail
    ),
    (
        "write.queue_timeout",
        Write,
        Some(ReceiptState::Rejected),
        AfterReRead,
        Code
    ),
    (
        "write.operation_unknown",
        Write,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "write.batch_too_large",
        Write,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "write.value_too_large",
        Write,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "shard.cross_mutation_rejected",
        Shard,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    ("receipt.expired", Receipt, None, AfterReRead, CodeDetail),
    ("receipt.not_found", Receipt, None, Resubmit, CodeDetail),
    ("receipt.state_invalid", Receipt, None, Never, CodeDetail),
    ("outbox.full", Outbox, None, AfterDrain, Code),
    ("outbox.dependency_cycle", Outbox, None, Never, CodeDetail),
    ("outbox.cross_shard_edge", Outbox, None, Never, CodeDetail),
    ("outbox.forward_reference", Outbox, None, Never, CodeDetail),
    ("outbox.fan_in_exceeded", Outbox, None, Never, CodeDetail),
    ("outbox.duplicate_request", Outbox, None, Never, CodeDetail),
    (
        "projection.lagging",
        Projection,
        Some(ReceiptState::Queued),
        Backoff,
        CodeDetail
    ),
    (
        "projection.epoch_invalid",
        Projection,
        Some(ReceiptState::Rejected),
        AfterRebuild,
        Code
    ),
    (
        "projection.watermark_regression",
        Projection,
        Some(ReceiptState::Rejected),
        AfterRebuild,
        CodeDetail
    ),
    (
        "projection.checksum_failed",
        Projection,
        Some(ReceiptState::Rejected),
        AfterRebuild,
        Code
    ),
    (
        "projection.client_write_rejected",
        Projection,
        Some(ReceiptState::Rejected),
        Never,
        Code
    ),
    (
        "projection.fan_in_exceeded",
        Projection,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "projection.read_set_exceeded",
        Projection,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "projection.fact_set_mismatch",
        Projection,
        None,
        AfterRebuild,
        Code
    ),
    (
        "projection.fact_type_mismatch",
        Projection,
        None,
        AfterRebuild,
        CodeDetail
    ),
    (
        "projection.feed_sequence_invalid",
        Projection,
        None,
        AfterRebuild,
        CodeDetail
    ),
    (
        "authorization.stale_facts",
        Policy,
        Some(ReceiptState::Rejected),
        AfterCatchUp,
        Code
    ),
    ("scope.over_limit", Scope, None, Never, CodeDetail),
    (
        "scope.device_budget_exceeded",
        Scope,
        None,
        Never,
        CodeDetail
    ),
    (
        "scope.hydration_budget_exceeded",
        Scope,
        None,
        Never,
        CodeDetail
    ),
    ("scope.identity_mismatch", Scope, None, Never, CodeDetail),
    (
        "scope.policy_version_unknown",
        Scope,
        None,
        AfterPolicySync,
        CodeDetail
    ),
    ("scope.shard_unreachable", Scope, None, Backoff, CodeDetail),
    ("scope.system_key_rejected", Scope, None, Never, CodeDetail),
    (
        "scope.remove_unknown_member",
        Scope,
        None,
        Never,
        CodeDetail
    ),
    ("scope.unknown_shard", Scope, None, Never, CodeDetail),
    ("scope.missing_pin_shard", Scope, None, Never, CodeDetail),
    ("sync.unsupported_version", Sync, None, Never, CodeDetail),
    ("sync.unknown_capability", Sync, None, Never, Code),
    ("sync.scope_rejected", Sync, None, AfterChange, Code),
    (
        "sync.cursor_expired",
        Sync,
        None,
        SnapshotFallback,
        CodeDetail
    ),
    ("sync.cursor_invalid", Sync, None, NewCursor, Code),
    (
        "sync.cursor_type_mismatch",
        Sync,
        None,
        NewCursor,
        CodeDetail
    ),
    (
        "sync.gap_too_large",
        Sync,
        None,
        SnapshotFallback,
        CodeDetail
    ),
    ("sync.snapshot_unavailable", Sync, None, Backoff, Code),
    ("sync.rate_limited", Sync, None, Backoff, Code),
    ("sync.unauthorized", Sync, None, AfterAuth, Code),
    ("sync.shard_unavailable", Sync, None, Backoff, CodeDetail),
    ("sync.negotiation_required", Sync, None, Never, Code),
    ("sync.incompatible_manifest", Sync, None, Never, CodeDetail),
    ("read.pin_expired", Read, None, NewPin, CodeDetail),
    ("read.pin_unreadable", Read, None, NewPin, CodeDetail),
    ("read.behind_commit", Read, None, Backoff, CodeDetail),
    ("read.not_hydrated", Read, None, Backoff, CodeDetail),
    ("read.not_resident", Read, None, Backoff, Code),
    ("read.stale", Read, None, Backoff, CodeDetail),
    ("search.pin_expired", Search, None, NewPin, CodeDetail),
    ("search.cursor_invalid", Search, None, NewQuery, CodeDetail),
    (
        "search.fanout_exceeded",
        Search,
        None,
        NarrowQuery,
        CodeDetail
    ),
    ("search.index_mismatch", Search, None, Rebuild, CodeDetail),
    (
        "search.tokenizer_mismatch",
        Search,
        None,
        Rebuild,
        CodeDetail
    ),
    ("search.tokenizer_unknown", Search, None, Never, CodeDetail),
    (
        "search.token_budget_exceeded",
        Search,
        None,
        NarrowQuery,
        CodeDetail
    ),
    (
        "index.overhead_exceeded",
        Search,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "query.budget_exceeded",
        Query,
        None,
        NarrowQuery,
        CodeDetail
    ),
    ("map.unknown_version", Map, None, Never, CodeDetail),
    ("map.overlap", Map, None, Never, CodeDetail),
    ("map.gap", Map, None, Never, CodeDetail),
    ("map.duplicate_id", Map, None, Never, CodeDetail),
    ("map.empty_range", Map, None, Never, CodeDetail),
    ("map.table_unmapped", Map, None, Never, CodeDetail),
    ("map.table_split_unsupported", Map, None, Never, CodeDetail),
    (
        "replication.stale_epoch",
        Replication,
        Some(ReceiptState::Rejected),
        AfterReAcquire,
        CodeDetail
    ),
    (
        "replication.lease_expired",
        Replication,
        Some(ReceiptState::Rejected),
        AfterReAcquire,
        CodeDetail
    ),
    (
        "replication.lease_held",
        Replication,
        None,
        AfterHint,
        CodeDetail
    ),
    (
        "replication.only_copy_eviction",
        Replication,
        None,
        OperatorAction,
        Code
    ),
    (
        "replication.tier_unavailable",
        Replication,
        None,
        Backoff,
        CodeDetail
    ),
    (
        "replication.warmup_budget_exceeded",
        Replication,
        None,
        ContinueWithCursor,
        CodeDetail
    ),
    (
        "replication.placement_violation",
        Replication,
        Some(ReceiptState::Rejected),
        Never,
        CodeDetail
    ),
    (
        "replication.journal_corrupt",
        Replication,
        None,
        Never,
        CodeDetail
    ),
    (
        "replication.journal_io",
        Replication,
        None,
        Backoff,
        CodeDetail
    ),
    ("event.unknown_version", Event, None, Never, CodeDetail),
    (
        "event.retention_gap",
        Event,
        None,
        SnapshotBackfill,
        CodeDetail
    ),
    ("event.poison", Event, None, DeadLetter, CodeDetail),
    ("event.payload_too_large", Event, None, Never, CodeDetail),
    ("consumer.too_slow", Consumer, None, Backoff, CodeDetail),
    (
        "consumer.handler_failed",
        Consumer,
        None,
        Backoff,
        CodeDetail
    ),
    (
        "consumer.dead_lettered",
        Consumer,
        None,
        OperatorAction,
        CodeDetail
    ),
    (
        "consumer.cursor_expired",
        Consumer,
        None,
        SnapshotBackfill,
        CodeDetail
    ),
    ("auth.unauthenticated", Auth, None, AfterAuth, Code),
    ("auth.stale_credentials", Auth, None, AfterRefresh, Code),
    (
        "auth.forbidden",
        Auth,
        Some(ReceiptState::Rejected),
        Never,
        Code
    ),
    ("auth.replayed_request", Auth, None, Never, Code),
    ("audit.unavailable", Audit, None, Backoff, Code),
    (
        "audit.chain_invalid",
        Audit,
        None,
        OperatorAction,
        CodeDetail
    ),
    ("audit.export_denied", Audit, None, AfterAuth, Code),
    (
        "shard.frozen_for_maintenance",
        Operations,
        Some(ReceiptState::Rejected),
        AfterMaintenance,
        CodeDetail
    ),
    (
        "ops.backup_pin_lost",
        Operations,
        None,
        RestartBackup,
        CodeDetail
    ),
    (
        "ops.restore_checksum_mismatch",
        Operations,
        None,
        RestartRestore,
        CodeDetail
    ),
    (
        "ops.migration_incomplete",
        Operations,
        None,
        ResumeMigration,
        CodeDetail
    ),
    ("ops.downgrade_not_allowed", Operations, None, Never, Code),
    (
        "ops.client_outbox_stale",
        Operations,
        None,
        AfterRebase,
        CodeDetail
    ),
    (
        "ops.quota_exceeded",
        Operations,
        Some(ReceiptState::Rejected),
        AfterQuota,
        CodeDetail
    ),
    ("ops.rate_limited", Operations, None, Backoff, CodeDetail),
    ("rpc.unknown_method", Rpc, None, Never, CodeDetail),
    (
        "rpc.descriptor_mismatch",
        Rpc,
        None,
        RegenerateClient,
        CodeDetail
    ),
    ("rpc.codec_unsupported", Rpc, None, Reconnect, CodeDetail),
    ("internal.invariant_violation", Internal, None, Never, Code),
    (
        "internal.resource_exhausted",
        Internal,
        Some(ReceiptState::Rejected),
        Backoff,
        CodeDetail
    ),
    (
        "projection.unknown_fact",
        Projection,
        None,
        Never,
        CodeDetail
    ),
    (
        "projection.duplicate_fact",
        Projection,
        None,
        Never,
        CodeDetail
    ),
    (
        "projection.fact_unavailable",
        Projection,
        None,
        Backoff,
        CodeDetail
    ),
    (
        "projection.epoch_overflow",
        Projection,
        None,
        AfterRebuild,
        Code
    ),
    ("scope.empty_scope", Scope, None, Never, Code),
    ("scope.invalid_table_name", Scope, None, Never, CodeDetail),
    (
        "scope.invalid_rule_set_selector",
        Scope,
        None,
        Never,
        CodeDetail
    ),
    ("scope.invalid_field_name", Scope, None, Never, CodeDetail),
    ("scope.byte_count_overflow", Scope, None, Never, Code),
    ("scope.shard_not_at_pin", Scope, None, Backoff, CodeDetail),
    ("scope.duplicate_response", Scope, None, Never, CodeDetail),
    ("scope.duplicate_row", Scope, None, Never, CodeDetail),
    (
        "scope.unsupported_diff_version",
        Scope,
        None,
        Never,
        CodeDetail
    ),
    (
        "scope.duplicate_diff_record",
        Scope,
        None,
        Never,
        CodeDetail
    ),
    ("scope.version_regression", Scope, None, Never, CodeDetail),
    (
        "scope.diff_sequence_invalid",
        Scope,
        None,
        Never,
        CodeDetail
    ),
    ("scope.base_version_mismatch", Scope, None, Never, Code),
    ("scope.malformed_diff", Scope, None, Never, Code),
    (
        "scope.duplicate_table_assignment",
        Scope,
        None,
        Never,
        CodeDetail
    ),
    ("scope.page_not_found", Scope, None, Never, CodeDetail),
    ("scope.in_scope_member", Scope, None, Never, CodeDetail),
    ("scope.not_evictable", Scope, None, Never, CodeDetail),
    ("scope.shard_not_pinned", Scope, None, Never, CodeDetail),
    ("manifest.duplicate_key", Manifest, None, Never, CodeDetail),
    ("manifest.unsupported_value", Manifest, None, Never, Code),
    ("kv.malformed_range", Key, None, Never, Code),
    ("kv.malformed_cursor", Key, None, Never, Code),
    ("kv.malformed_batch", Key, None, Never, Code),
    ("kv.malformed_page", Key, None, Never, Code),
    ("map.malformed_key", Map, None, Never, CodeDetail),
    ("map.invalid_table", Map, None, Never, CodeDetail),
    ("map.malformed_canonical", Map, None, Never, CodeDetail),
    (
        "index.field_not_searchable",
        Search,
        None,
        Never,
        CodeDetail
    ),
    ("index.unknown_flag", Search, None, Never, CodeDetail),
    ("index.malformed_document", Search, None, Never, CodeDetail),
    (
        "index.version_not_monotonic",
        Search,
        None,
        Never,
        CodeDetail
    ),
    ("index.empty_write", Search, None, Never, Code),
    ("index.invalid_manifest", Search, None, Never, CodeDetail),
    ("index.invalid_scorer", Search, None, Never, CodeDetail),
    ("hydrate.unsupported_signature", Format, None, Never, Code),
    (
        "hydrate.storage_failed",
        Internal,
        None,
        Backoff,
        CodeDetail
    ),
    ("hydrate.not_started", Internal, None, Never, Code),
    ("hydrate.already_published", Internal, None, Never, Code),
    (
        "replica.content_digest_mismatch",
        Format,
        None,
        Refetch,
        CodeDetail
    ),
    ("replica.scope_violation", Scope, None, Never, CodeDetail),
    ("replica.scope_pin_mismatch", Scope, None, Never, Code),
    ("replica.local_mutation_rejected", Write, None, Never, Code),
    ("replica.version_regression", Replication, None, Never, Code),
    (
        "replica.hydration_in_progress",
        Replication,
        None,
        Never,
        Code
    ),
    (
        "replica.hydration_not_started",
        Replication,
        None,
        Never,
        Code
    ),
    (
        "replica.partial_generation",
        Format,
        None,
        Refetch,
        CodeDetail
    ),
    ("replica.tab_lease_held", Replication, None, AfterHint, Code),
    (
        "replica.stale_tab_lease",
        Replication,
        None,
        AfterReAcquire,
        CodeDetail
    ),
    ("query.duplicate_shard", Query, None, Never, CodeDetail),
    ("query.duplicate_key", Query, None, Never, CodeDetail),
    ("query.counter_overflow", Query, None, Never, CodeDetail),
    ("search.pin_missing_shard", Search, None, NewPin, CodeDetail),
    ("search.unordered_shard", Search, None, Rebuild, CodeDetail),
    ("search.duplicate_hit", Search, None, Rebuild, CodeDetail),
    ("sync.map_mismatch", Sync, None, Never, CodeDetail),
    ("rpc.already_negotiated", Rpc, None, Never, Code),
    ("rpc.handler_violation", Rpc, None, Never, CodeDetail),
    ("rpc.invalid_envelope", Rpc, None, Never, Code),
    ("rpc.unknown_remote_code", Rpc, None, Never, CodeDetail),
    ("sync.corrupt_frame", Sync, None, Refetch, Code),
    ("sync.protocol_error", Sync, None, Reconnect, Code),
    ("format.malformed_header", Format, None, Never, CodeDetail),
    (
        "format.duplicate_record_key",
        Format,
        None,
        Never,
        CodeDetail
    ),
    (
        "format.forbidden_namespace",
        Format,
        None,
        Never,
        CodeDetail
    ),
    (
        "format.system_key_not_allowed",
        Format,
        None,
        Never,
        CodeDetail
    ),
    (
        "format.not_projection_fact",
        Format,
        None,
        Never,
        CodeDetail
    ),
    (
        "format.out_of_scope_record",
        Format,
        None,
        Never,
        CodeDetail
    ),
    (
        "replication.artifact_missing",
        Replication,
        None,
        Backoff,
        Code
    ),
    (
        "replication.replica_authority_lease",
        Replication,
        None,
        Never,
        Code
    ),
    (
        "replication.term_exhausted",
        Replication,
        None,
        OperatorAction,
        Code
    ),
    ("receipt.terminal_entry", Receipt, None, Never, CodeDetail),
    ("receipt.missing_outcome", Receipt, None, Never, CodeDetail),
    ("receipt.not_terminal", Receipt, None, Never, CodeDetail),
);

const _: () = assert_registry(ERROR_REGISTRY);

/// Looks up one error code.
#[must_use]
pub fn lookup(code: &str) -> Option<&'static ErrorSpec> {
    ERROR_REGISTRY.iter().find(|spec| spec.code == code)
}

/// Looks up one error code or returns a registry error.
///
/// # Errors
///
/// Returns [`ErrorRegistryError::Unknown`] when the code is absent.
pub fn lookup_required(code: &str) -> Result<&'static ErrorSpec, ErrorRegistryError> {
    lookup(code).ok_or_else(|| ErrorRegistryError::Unknown {
        code: code.to_owned(),
    })
}

/// Looks up one receipt reason code.
///
/// # Errors
///
/// Returns [`ErrorRegistryError::Unknown`] when the code is absent. Returns
/// [`ErrorRegistryError::NoReceipt`] when the code maps to no receipt.
pub fn lookup_receipt_reason(code: &str) -> Result<&'static ErrorSpec, ErrorRegistryError> {
    let spec = lookup_required(code)?;
    if spec.receipt.is_none() {
        return Err(ErrorRegistryError::NoReceipt {
            code: code.to_owned(),
        });
    }
    Ok(spec)
}

/// Returns true when the code uses the lowercase dot namespace form.
#[must_use]
pub fn is_valid_code_shape(code: &str) -> bool {
    valid_code_shape(code)
}

/// Returns the first duplicate code in a table.
#[must_use]
pub fn find_duplicate_code(table: &[ErrorSpec]) -> Option<&str> {
    for (index, spec) in table.iter().enumerate() {
        for other in &table[index + 1..] {
            if spec.code == other.code {
                return Some(spec.code);
            }
        }
    }
    None
}

/// Returns every code that maps to one receipt state.
#[must_use]
pub fn codes_for_receipt_state(state: ReceiptState) -> Vec<&'static ErrorSpec> {
    ERROR_REGISTRY
        .iter()
        .filter(|spec| spec.receipt == Some(state))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_has_one_row_per_code() {
        assert_eq!(ERROR_REGISTRY.len(), 234);
        assert_eq!(find_duplicate_code(ERROR_REGISTRY), None);
    }

    #[test]
    fn every_code_has_a_valid_shape() {
        for spec in ERROR_REGISTRY {
            assert!(
                is_valid_code_shape(spec.code),
                "the code {} does not use the lowercase dot namespace form",
                spec.code
            );
        }
        assert!(!is_valid_code_shape("Key.UnknownVersion"));
        assert!(!is_valid_code_shape("key"));
        assert!(!is_valid_code_shape("key..unknown"));
        assert!(!is_valid_code_shape("key.unknown."));
        assert!(!is_valid_code_shape(".key.unknown"));
    }

    #[test]
    fn duplicate_detection_finds_a_copy() {
        let mut table = ERROR_REGISTRY.to_vec();
        table.push(ERROR_REGISTRY[0]);
        assert_eq!(find_duplicate_code(&table), Some(ERROR_REGISTRY[0].code));
    }

    #[test]
    fn every_receipt_state_that_carries_an_error_has_a_code() {
        for state in [
            ReceiptState::Queued,
            ReceiptState::Rejected,
            ReceiptState::Conflict,
        ] {
            assert!(
                !codes_for_receipt_state(state).is_empty(),
                "the receipt state {state} has no error code"
            );
        }
        // Committed is a success outcome. No error code maps to it. The
        // receipt module still defines all four states.
        assert_eq!(codes_for_receipt_state(ReceiptState::Committed).len(), 0);
    }

    #[test]
    fn every_terminal_flag_matches_its_receipt_state() {
        for spec in ERROR_REGISTRY {
            let expected = !matches!(spec.receipt, Some(ReceiptState::Queued));
            assert_eq!(
                spec.terminal, expected,
                "the terminal flag is wrong for {}",
                spec.code
            );
        }
    }

    #[test]
    fn a_queued_code_is_live_and_declares_a_retry_class() {
        let spec = lookup("projection.lagging").expect("the code is registered");
        assert_eq!(spec.receipt, Some(ReceiptState::Queued));
        assert!(!spec.terminal);
        assert_ne!(spec.retry, RetryClass::Never);
    }

    #[test]
    fn a_conflict_code_names_the_key_version_field() {
        let spec = lookup("write.version_conflict").expect("the code is registered");
        assert_eq!(spec.receipt, Some(ReceiptState::Conflict));
        assert!(spec.terminal);
    }

    #[test]
    fn every_conflict_code_names_the_current_key_version_field() {
        for spec in ERROR_REGISTRY {
            if spec.receipt == Some(ReceiptState::Conflict) {
                assert_eq!(spec.conflict_key_version_field, Some("current_key_version"));
            } else {
                assert_eq!(spec.conflict_key_version_field, None);
            }
        }
    }

    #[test]
    fn every_rejected_code_declares_idempotency_consumption() {
        for spec in ERROR_REGISTRY {
            if spec.receipt == Some(ReceiptState::Rejected) {
                assert_eq!(spec.idempotency_consumed, Some(true));
            } else {
                assert_eq!(spec.idempotency_consumed, None);
            }
        }
    }

    #[test]
    fn every_queued_code_declares_a_deadline_and_a_retry_class() {
        for spec in ERROR_REGISTRY {
            if spec.receipt == Some(ReceiptState::Queued) {
                assert_eq!(spec.queued_deadline, Some("queue_deadline"));
                assert_ne!(spec.retry, RetryClass::Never);
            } else {
                assert_eq!(spec.queued_deadline, None);
            }
        }
    }

    #[test]
    fn every_code_declares_a_wire_shape() {
        for spec in ERROR_REGISTRY {
            assert!(WireShape::ALL.contains(&spec.wire_shape));
        }
    }

    #[test]
    fn a_receipt_reason_must_name_a_code_with_a_receipt() {
        assert!(lookup_receipt_reason("write.version_conflict").is_ok());
        assert!(lookup_receipt_reason("projection.lagging").is_ok());
        assert_eq!(
            lookup_receipt_reason("key.not_found"),
            Err(ErrorRegistryError::NoReceipt {
                code: "key.not_found".to_owned()
            })
        );
        assert!(matches!(
            lookup_receipt_reason("not.registered"),
            Err(ErrorRegistryError::Unknown { .. })
        ));
    }

    #[test]
    fn a_failed_audit_append_has_no_receipt() {
        let spec = lookup("audit.unavailable").expect("the code is registered");
        assert_eq!(spec.receipt, None);
        assert_eq!(
            lookup_receipt_reason("audit.unavailable"),
            Err(ErrorRegistryError::NoReceipt {
                code: "audit.unavailable".to_owned()
            })
        );
    }

    const DISTINCT_CAUSE_CODES: [(&str, ErrorKind, Option<ReceiptState>, RetryClass, WireShape);
        14] = [
        (
            "scope.system_key_rejected",
            ErrorKind::Scope,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "scope.remove_unknown_member",
            ErrorKind::Scope,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "scope.unknown_shard",
            ErrorKind::Scope,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "scope.missing_pin_shard",
            ErrorKind::Scope,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "projection.fact_set_mismatch",
            ErrorKind::Projection,
            None,
            RetryClass::AfterRebuild,
            WireShape::Code,
        ),
        (
            "projection.fact_type_mismatch",
            ErrorKind::Projection,
            None,
            RetryClass::AfterRebuild,
            WireShape::CodeDetail,
        ),
        (
            "projection.feed_sequence_invalid",
            ErrorKind::Projection,
            None,
            RetryClass::AfterRebuild,
            WireShape::CodeDetail,
        ),
        (
            "manifest.json_syntax",
            ErrorKind::Manifest,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "manifest.missing_field",
            ErrorKind::Manifest,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "manifest.type_mismatch",
            ErrorKind::Manifest,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "manifest.invalid_value",
            ErrorKind::Manifest,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "outbox.forward_reference",
            ErrorKind::Outbox,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "outbox.fan_in_exceeded",
            ErrorKind::Outbox,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
        (
            "outbox.duplicate_request",
            ErrorKind::Outbox,
            None,
            RetryClass::Never,
            WireShape::CodeDetail,
        ),
    ];

    #[test]
    fn the_distinct_cause_codes_are_pinned() {
        for (code, kind, receipt, retry, shape) in DISTINCT_CAUSE_CODES {
            let spec = lookup(code).unwrap_or_else(|| panic!("the code {code} is not registered"));
            assert_eq!(spec.kind, kind, "the kind is wrong for {code}");
            assert_eq!(spec.receipt, receipt, "the receipt is wrong for {code}");
            assert_eq!(spec.retry, retry, "the retry class is wrong for {code}");
            assert_eq!(spec.wire_shape, shape, "the shape is wrong for {code}");
        }
    }

    #[test]
    fn unknown_lookup_fails() {
        assert!(lookup("key.unknown").is_none());
        assert!(lookup("").is_none());
        assert!(ErrorCode::parse("write.not_a_code").is_none());
        assert_eq!(
            lookup_required("write.not_a_code"),
            Err(ErrorRegistryError::Unknown {
                code: "write.not_a_code".to_owned()
            })
        );
    }

    #[test]
    fn the_canonical_display_is_the_code() {
        for spec in ERROR_REGISTRY {
            assert_eq!(spec.to_string(), spec.code);
        }
        let code = ErrorCode::parse("write.version_conflict").expect("the code is registered");
        assert_eq!(code.to_string(), "write.version_conflict");
        assert_eq!(code.kind(), ErrorKind::Write);
        assert_eq!(code.receipt(), Some(ReceiptState::Conflict));
        assert_eq!(code.retry(), RetryClass::AfterRebase);
        assert!(code.is_terminal());
    }

    #[test]
    fn every_retry_class_is_used() {
        for retry in RetryClass::ALL {
            assert!(
                ERROR_REGISTRY.iter().any(|spec| spec.retry == *retry),
                "the retry class {retry} is not used"
            );
        }
    }
}
