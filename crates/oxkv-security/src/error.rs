//! Security error codes.
//!
//! Each error maps to one code in the contract registry. An error carries a
//! fixed token only. It carries no request data.

use oxkv_contracts::receipt::ReceiptState;

/// A security error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SecurityError {
    /// The credential is absent or unknown.
    #[error("Authentication failed.")]
    Unauthenticated,
    /// The credential is revoked and the staleness bound has passed.
    #[error("The credential is stale.")]
    StaleCredentials,
    /// The request is denied. The reason is deliberately coarse.
    #[error("The request is forbidden.")]
    Forbidden,
    /// The request reuses a nonce with different content.
    #[error("The request is a replay.")]
    ReplayedRequest,
    /// The audit chain cannot accept the record.
    ///
    /// The gate returns this error with no receipt. A rejected verdict needs
    /// one audit record. A failed append therefore leaves no receipt.
    #[error("The audit chain is unavailable.")]
    AuditUnavailable,
    /// The audit chain fails verification.
    #[error("The audit chain is invalid.")]
    AuditChainInvalid,
    /// The caller may not export the audit chain.
    #[error("The audit export is denied.")]
    AuditExportDenied,
    /// The policy or search uses a regular expression.
    #[error("The policy does not support a regular expression.")]
    RegexNotSupported,
    /// The policy step budget is exhausted.
    #[error("The policy step budget is exhausted.")]
    PolicyBudgetExhausted,
    /// The idempotency key has a different request body.
    #[error("The idempotency key has a different body.")]
    IdempotencyKeyReuse,
    /// An internal invariant failed.
    #[error("An internal invariant failed.")]
    InvariantViolation,
}

impl SecurityError {
    /// Every security error.
    pub const ALL: &'static [Self] = &[
        Self::Unauthenticated,
        Self::StaleCredentials,
        Self::Forbidden,
        Self::ReplayedRequest,
        Self::AuditUnavailable,
        Self::AuditChainInvalid,
        Self::AuditExportDenied,
        Self::RegexNotSupported,
        Self::PolicyBudgetExhausted,
        Self::IdempotencyKeyReuse,
        Self::InvariantViolation,
    ];

    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unauthenticated => "auth.unauthenticated",
            Self::StaleCredentials => "auth.stale_credentials",
            Self::Forbidden => "auth.forbidden",
            Self::ReplayedRequest => "auth.replayed_request",
            Self::AuditUnavailable => "audit.unavailable",
            Self::AuditChainInvalid => "audit.chain_invalid",
            Self::AuditExportDenied => "audit.export_denied",
            Self::RegexNotSupported => "policy.regex_not_supported",
            Self::PolicyBudgetExhausted => "policy.budget_exhausted",
            Self::IdempotencyKeyReuse => "write.idempotency_key_reuse",
            Self::InvariantViolation => "internal.invariant_violation",
        }
    }

    /// Returns the receipt state from the contract registry.
    ///
    /// A value of `None` means that the error has no receipt.
    #[must_use]
    pub const fn receipt_state(self) -> Option<ReceiptState> {
        match self {
            Self::Forbidden | Self::PolicyBudgetExhausted => Some(ReceiptState::Rejected),
            Self::Unauthenticated
            | Self::StaleCredentials
            | Self::ReplayedRequest
            | Self::AuditUnavailable
            | Self::AuditChainInvalid
            | Self::AuditExportDenied
            | Self::RegexNotSupported
            | Self::IdempotencyKeyReuse
            | Self::InvariantViolation => None,
        }
    }
}
