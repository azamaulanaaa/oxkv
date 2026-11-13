//! The canonical error and receipt registry.
//!
//! Every error code in the 1.0 contract appears once in [`ERROR_REGISTRY`].
//! Each entry names one kind, one receipt state, one retry class, and one
//! terminal flag.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WireShape {
    /// The wire value is the code text only.
    Code,
    /// The wire value is the code text and a fixed message.
    CodeMessage,
    /// The wire value is the code text and a structured detail.
    CodeDetail,
}

impl WireShape {
    /// Every wire shape.
    pub const ALL: &'static [Self] = &[Self::Code, Self::CodeMessage, Self::CodeDetail];

    /// Returns the canonical shape name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::CodeMessage => "code_message",
            Self::CodeDetail => "code_detail",
        }
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
            "{} kind={} receipt={} retry={} terminal={}",
            self.code,
            self.kind,
            self.receipt
                .map_or_else(|| "none".to_owned(), |state| state.name().to_owned()),
            self.retry,
            self.terminal
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
    ($(($code:literal, $kind:ident, $receipt:expr, $retry:ident)),* $(,)?) => {
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
                wire_shape: WireShape::Code,
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
        Never
    ),
    ("key.malformed", Key, Some(ReceiptState::Rejected), Never),
    (
        "key.non_canonical",
        Key,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "key.outside_table",
        Key,
        Some(ReceiptState::Rejected),
        Never
    ),
    ("key.invalid_utf8", Key, Some(ReceiptState::Rejected), Never),
    ("key.too_large", Key, Some(ReceiptState::Rejected), Never),
    ("key.not_found", Key, None, Never),
    (
        "key.reserved_system_marker",
        Key,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "key.unknown_system_id",
        Key,
        Some(ReceiptState::Rejected),
        Never
    ),
    ("format.unsupported_version", Format, None, Never),
    ("format.missing_required_feature", Format, None, Never),
    ("format.corrupt_page", Format, None, Refetch),
    ("format.page_out_of_bounds", Format, None, Never),
    ("format.truncated_snapshot", Format, None, Refetch),
    ("format.missing_provenance", Format, None, Never),
    ("format.page_too_large", Format, None, Never),
    ("manifest.unsupported_schema", Manifest, None, Never),
    ("manifest.unknown_field", Manifest, None, Never),
    ("manifest.duplicate_name", Manifest, None, Never),
    ("manifest.reserved_table_name", Manifest, None, Never),
    ("manifest.key_schema_missing", Manifest, None, Never),
    ("manifest.float_not_allowed", Manifest, None, Never),
    ("manifest.index_unknown_field", Manifest, None, Never),
    ("manifest.index_scope_unreachable", Manifest, None, Never),
    ("manifest.rule_unknown_path", Manifest, None, Never),
    ("manifest.rule_unbounded", Manifest, None, Never),
    ("manifest.operation_cross_shard", Manifest, None, Never),
    ("manifest.operation_unknown_field", Manifest, None, Never),
    ("manifest.artifact_drift", Manifest, None, Regenerate),
    ("manifest.policy_catalog_missing", Manifest, None, Never),
    ("manifest.json_syntax", Manifest, None, Never),
    ("manifest.missing_field", Manifest, None, Never),
    ("manifest.type_mismatch", Manifest, None, Never),
    ("manifest.invalid_value", Manifest, None, Never),
    ("policy.parse_error", Policy, None, Never),
    (
        "policy.unknown_field",
        Policy,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "policy.budget_exhausted",
        Policy,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "policy.not_total",
        Policy,
        Some(ReceiptState::Rejected),
        Never
    ),
    ("policy.ast_unsupported", Policy, None, Never),
    ("policy.regex_not_supported", Policy, None, Never),
    (
        "rule.compile_failed",
        Rule,
        Some(ReceiptState::Rejected),
        AfterFix
    ),
    (
        "rule.dry_run_failed",
        Rule,
        Some(ReceiptState::Rejected),
        AfterFix
    ),
    (
        "rule.approval_required",
        Rule,
        Some(ReceiptState::Rejected),
        AfterApproval
    ),
    (
        "rule.rollout_limit",
        Rule,
        Some(ReceiptState::Rejected),
        AfterRollout
    ),
    ("write.missing_base_version", Write, None, Never),
    (
        "write.stale_base_version",
        Write,
        Some(ReceiptState::Rejected),
        AfterReRead
    ),
    (
        "write.version_conflict",
        Write,
        Some(ReceiptState::Conflict),
        AfterRebase
    ),
    ("write.idempotency_key_reuse", Write, None, Never),
    (
        "write.queue_timeout",
        Write,
        Some(ReceiptState::Rejected),
        AfterReRead
    ),
    (
        "write.operation_unknown",
        Write,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "write.batch_too_large",
        Write,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "write.value_too_large",
        Write,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "shard.cross_mutation_rejected",
        Shard,
        Some(ReceiptState::Rejected),
        Never
    ),
    ("receipt.expired", Receipt, None, AfterReRead),
    ("receipt.not_found", Receipt, None, Resubmit),
    ("receipt.state_invalid", Receipt, None, Never),
    ("outbox.full", Outbox, None, AfterDrain),
    ("outbox.dependency_cycle", Outbox, None, Never),
    ("outbox.cross_shard_edge", Outbox, None, Never),
    ("outbox.forward_reference", Outbox, None, Never),
    ("outbox.fan_in_exceeded", Outbox, None, Never),
    ("outbox.duplicate_request", Outbox, None, Never),
    (
        "projection.lagging",
        Projection,
        Some(ReceiptState::Queued),
        Backoff
    ),
    (
        "projection.epoch_invalid",
        Projection,
        Some(ReceiptState::Rejected),
        AfterRebuild
    ),
    (
        "projection.watermark_regression",
        Projection,
        Some(ReceiptState::Rejected),
        AfterRebuild
    ),
    (
        "projection.checksum_failed",
        Projection,
        Some(ReceiptState::Rejected),
        AfterRebuild
    ),
    (
        "projection.client_write_rejected",
        Projection,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "projection.fan_in_exceeded",
        Projection,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "projection.read_set_exceeded",
        Projection,
        Some(ReceiptState::Rejected),
        Never
    ),
    (
        "projection.fact_set_mismatch",
        Projection,
        None,
        AfterRebuild
    ),
    (
        "projection.fact_type_mismatch",
        Projection,
        None,
        AfterRebuild
    ),
    (
        "projection.feed_sequence_invalid",
        Projection,
        None,
        AfterRebuild
    ),
    (
        "authorization.stale_facts",
        Policy,
        Some(ReceiptState::Rejected),
        AfterCatchUp
    ),
    ("scope.over_limit", Scope, None, Never),
    ("scope.device_budget_exceeded", Scope, None, Never),
    ("scope.hydration_budget_exceeded", Scope, None, Never),
    ("scope.identity_mismatch", Scope, None, Never),
    ("scope.policy_version_unknown", Scope, None, AfterPolicySync),
    ("scope.shard_unreachable", Scope, None, Backoff),
    ("scope.system_key_rejected", Scope, None, Never),
    ("scope.remove_unknown_member", Scope, None, Never),
    ("scope.unknown_shard", Scope, None, Never),
    ("scope.missing_pin_shard", Scope, None, Never),
    ("sync.unsupported_version", Sync, None, Never),
    ("sync.unknown_capability", Sync, None, Never),
    ("sync.scope_rejected", Sync, None, AfterChange),
    ("sync.cursor_expired", Sync, None, SnapshotFallback),
    ("sync.cursor_invalid", Sync, None, NewCursor),
    ("sync.cursor_type_mismatch", Sync, None, NewCursor),
    ("sync.gap_too_large", Sync, None, SnapshotFallback),
    ("sync.snapshot_unavailable", Sync, None, Backoff),
    ("sync.rate_limited", Sync, None, Backoff),
    ("sync.unauthorized", Sync, None, AfterAuth),
    ("sync.shard_unavailable", Sync, None, Backoff),
    ("sync.negotiation_required", Sync, None, Never),
    ("sync.incompatible_manifest", Sync, None, Never),
    ("read.pin_expired", Read, None, NewPin),
    ("read.pin_unreadable", Read, None, NewPin),
    ("read.behind_commit", Read, None, Backoff),
    ("read.not_hydrated", Read, None, Backoff),
    ("read.not_resident", Read, None, Backoff),
    ("read.stale", Read, None, Backoff),
    ("search.pin_expired", Search, None, NewPin),
    ("search.cursor_invalid", Search, None, NewQuery),
    ("search.fanout_exceeded", Search, None, NarrowQuery),
    ("search.index_mismatch", Search, None, Rebuild),
    ("search.tokenizer_mismatch", Search, None, Rebuild),
    ("search.tokenizer_unknown", Search, None, Never),
    ("search.token_budget_exceeded", Search, None, NarrowQuery),
    (
        "index.overhead_exceeded",
        Search,
        Some(ReceiptState::Rejected),
        Never
    ),
    ("query.budget_exceeded", Query, None, NarrowQuery),
    ("map.unknown_version", Map, None, Never),
    ("map.overlap", Map, None, Never),
    ("map.gap", Map, None, Never),
    ("map.duplicate_id", Map, None, Never),
    ("map.empty_range", Map, None, Never),
    ("map.table_unmapped", Map, None, Never),
    ("map.table_split_unsupported", Map, None, Never),
    (
        "replication.stale_epoch",
        Replication,
        Some(ReceiptState::Rejected),
        AfterReAcquire
    ),
    (
        "replication.lease_expired",
        Replication,
        Some(ReceiptState::Rejected),
        AfterReAcquire
    ),
    ("replication.lease_held", Replication, None, AfterHint),
    (
        "replication.only_copy_eviction",
        Replication,
        None,
        OperatorAction
    ),
    ("replication.tier_unavailable", Replication, None, Backoff),
    (
        "replication.warmup_budget_exceeded",
        Replication,
        None,
        ContinueWithCursor
    ),
    (
        "replication.placement_violation",
        Replication,
        Some(ReceiptState::Rejected),
        Never
    ),
    ("event.unknown_version", Event, None, Never),
    ("event.retention_gap", Event, None, SnapshotBackfill),
    ("event.poison", Event, None, DeadLetter),
    ("event.payload_too_large", Event, None, Never),
    ("consumer.too_slow", Consumer, None, Backoff),
    ("consumer.handler_failed", Consumer, None, Backoff),
    ("consumer.dead_lettered", Consumer, None, OperatorAction),
    ("consumer.cursor_expired", Consumer, None, SnapshotBackfill),
    ("auth.unauthenticated", Auth, None, AfterAuth),
    ("auth.stale_credentials", Auth, None, AfterRefresh),
    ("auth.forbidden", Auth, Some(ReceiptState::Rejected), Never),
    ("auth.replayed_request", Auth, None, Never),
    ("audit.unavailable", Audit, None, Backoff),
    ("audit.chain_invalid", Audit, None, OperatorAction),
    ("audit.export_denied", Audit, None, AfterAuth),
    (
        "shard.frozen_for_maintenance",
        Operations,
        Some(ReceiptState::Rejected),
        AfterMaintenance
    ),
    ("ops.backup_pin_lost", Operations, None, RestartBackup),
    (
        "ops.restore_checksum_mismatch",
        Operations,
        None,
        RestartRestore
    ),
    (
        "ops.migration_incomplete",
        Operations,
        None,
        ResumeMigration
    ),
    ("ops.downgrade_not_allowed", Operations, None, Never),
    ("ops.client_outbox_stale", Operations, None, AfterRebase),
    (
        "ops.quota_exceeded",
        Operations,
        Some(ReceiptState::Rejected),
        AfterQuota
    ),
    ("ops.rate_limited", Operations, None, Backoff),
    ("rpc.unknown_method", Rpc, None, Never),
    ("rpc.descriptor_mismatch", Rpc, None, RegenerateClient),
    ("rpc.codec_unsupported", Rpc, None, Reconnect),
    ("internal.invariant_violation", Internal, None, Never),
    (
        "internal.resource_exhausted",
        Internal,
        Some(ReceiptState::Rejected),
        Backoff
    ),
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
        assert_eq!(ERROR_REGISTRY.len(), 153);
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

    #[test]
    fn the_distinct_cause_codes_are_pinned() {
        let cases = [
            (
                "scope.system_key_rejected",
                ErrorKind::Scope,
                None,
                RetryClass::Never,
            ),
            (
                "scope.remove_unknown_member",
                ErrorKind::Scope,
                None,
                RetryClass::Never,
            ),
            (
                "scope.unknown_shard",
                ErrorKind::Scope,
                None,
                RetryClass::Never,
            ),
            (
                "scope.missing_pin_shard",
                ErrorKind::Scope,
                None,
                RetryClass::Never,
            ),
            (
                "projection.fact_set_mismatch",
                ErrorKind::Projection,
                None,
                RetryClass::AfterRebuild,
            ),
            (
                "projection.fact_type_mismatch",
                ErrorKind::Projection,
                None,
                RetryClass::AfterRebuild,
            ),
            (
                "projection.feed_sequence_invalid",
                ErrorKind::Projection,
                None,
                RetryClass::AfterRebuild,
            ),
            (
                "manifest.json_syntax",
                ErrorKind::Manifest,
                None,
                RetryClass::Never,
            ),
            (
                "manifest.missing_field",
                ErrorKind::Manifest,
                None,
                RetryClass::Never,
            ),
            (
                "manifest.type_mismatch",
                ErrorKind::Manifest,
                None,
                RetryClass::Never,
            ),
            (
                "manifest.invalid_value",
                ErrorKind::Manifest,
                None,
                RetryClass::Never,
            ),
            (
                "outbox.forward_reference",
                ErrorKind::Outbox,
                None,
                RetryClass::Never,
            ),
            (
                "outbox.fan_in_exceeded",
                ErrorKind::Outbox,
                None,
                RetryClass::Never,
            ),
            (
                "outbox.duplicate_request",
                ErrorKind::Outbox,
                None,
                RetryClass::Never,
            ),
        ];
        for (code, kind, receipt, retry) in cases {
            let spec = lookup(code).unwrap_or_else(|| panic!("the code {code} is not registered"));
            assert_eq!(spec.kind, kind, "the kind is wrong for {code}");
            assert_eq!(spec.receipt, receipt, "the receipt is wrong for {code}");
            assert_eq!(spec.retry, retry, "the retry class is wrong for {code}");
            assert_eq!(
                spec.wire_shape,
                WireShape::Code,
                "the shape is wrong for {code}"
            );
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
