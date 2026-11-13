//! Rule lifecycle errors.
//!
//! Every error maps to one code in [`oxkv_contracts::error::ERROR_REGISTRY`].
//! A rule change that reaches admission returns a receipt. The crate returns a
//! [`RuleError`] only when a caller breaks the lifecycle API.

use oxkv_contracts::error::{ErrorCode, RetryClass, lookup};
use oxkv_contracts::receipt::ReceiptState;

/// The error codes that the rule lifecycle can return.
pub const RULE_ERROR_CODES: &[&str] = &[
    "rule.compile_failed",
    "rule.dry_run_failed",
    "rule.approval_required",
    "rule.rollout_limit",
    "manifest.policy_catalog_missing",
    "manifest.rule_unknown_path",
    "manifest.rule_unbounded",
    "policy.parse_error",
    "policy.ast_unsupported",
    "scope.policy_version_unknown",
    "manifest.duplicate_name",
    "write.version_conflict",
];

/// One lifecycle or policy failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    /// The policy did not compile.
    #[error("The policy did not compile. {detail}")]
    CompileFailed {
        /// The compile failure detail.
        detail: String,
    },
    /// The dry run did not finish or it found an evaluation error.
    #[error("The dry run failed. {detail}")]
    DryRunFailed {
        /// The dry-run failure detail.
        detail: String,
    },
    /// The change needs more approval.
    #[error("The change needs approval. {detail}")]
    ApprovalRequired {
        /// The approval detail.
        detail: String,
    },
    /// The policy id would serve a third live version.
    #[error("The policy id {policy_id} already serves two versions.")]
    RolloutLimit {
        /// The policy id.
        policy_id: String,
    },
    /// A rollout is absent for the cohort.
    #[error("The cohort has no active rollout.")]
    RolloutMissing,
    /// The manifest has no policy catalog.
    #[error("The manifest has no policy catalog.")]
    PolicyCatalogMissing,
    /// A rule reads a path that the manifest does not declare.
    #[error("The rule {rule_id} reads the unknown path {path}.")]
    RuleUnknownPath {
        /// The rule identity.
        rule_id: String,
        /// The undeclared path.
        path: String,
    },
    /// A rule has no finite cost bound.
    #[error("The rule {rule_id} is over its cost bound.")]
    RuleUnbounded {
        /// The rule identity.
        rule_id: String,
    },
    /// The policy AST did not parse.
    #[error("The policy AST did not parse. {detail}")]
    PolicyParse {
        /// The parse failure detail.
        detail: String,
    },
    /// The policy AST version is not supported.
    #[error("The policy AST version {ast_version} is not supported.")]
    AstUnsupported {
        /// The unsupported AST version.
        ast_version: String,
    },
    /// The policy version is absent from the catalog or its retention window.
    #[error("The policy version {policy_version} is unknown.")]
    PolicyVersionUnknown {
        /// The unknown policy version.
        policy_version: String,
    },
    /// The manifest declares one rule identity two times.
    #[error("The manifest declares the rule {rule_id} two times.")]
    DuplicateRule {
        /// The duplicate rule identity.
        rule_id: String,
    },
    /// A base version does not match the active assignment.
    #[error("The base version does not match the active policy version.")]
    StaleBase,
}

impl RuleError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CompileFailed { .. } => "rule.compile_failed",
            Self::DryRunFailed { .. } => "rule.dry_run_failed",
            Self::ApprovalRequired { .. } => "rule.approval_required",
            Self::RolloutLimit { .. } | Self::RolloutMissing => "rule.rollout_limit",
            Self::PolicyCatalogMissing => "manifest.policy_catalog_missing",
            Self::RuleUnknownPath { .. } => "manifest.rule_unknown_path",
            Self::RuleUnbounded { .. } => "manifest.rule_unbounded",
            Self::PolicyParse { .. } => "policy.parse_error",
            Self::AstUnsupported { .. } => "policy.ast_unsupported",
            Self::PolicyVersionUnknown { .. } => "scope.policy_version_unknown",
            Self::DuplicateRule { .. } => "manifest.duplicate_name",
            Self::StaleBase => "write.version_conflict",
        }
    }

    /// Returns the registered receipt state for the error.
    ///
    /// A compile or scope error has no receipt. A rule gate rejection has a
    /// `Rejected` receipt. A stale base has a `Conflict` receipt.
    #[must_use]
    pub fn receipt_state(&self) -> Option<ReceiptState> {
        lookup(self.code()).and_then(|spec| spec.receipt)
    }

    /// Returns the registered retry class for the error.
    #[must_use]
    pub fn retry_class(&self) -> Option<RetryClass> {
        lookup(self.code()).map(|spec| spec.retry)
    }

    /// Returns the checked registry entry for the error.
    #[must_use]
    pub fn registry_code(&self) -> Option<ErrorCode> {
        ErrorCode::parse(self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_code_is_registered() {
        for code in RULE_ERROR_CODES {
            assert!(
                lookup(code).is_some(),
                "the error code {code} is not registered"
            );
        }
    }

    #[test]
    fn a_rule_gate_error_has_a_rejected_receipt() {
        let error = RuleError::ApprovalRequired {
            detail: "A security approver is missing.".to_owned(),
        };
        assert_eq!(error.code(), "rule.approval_required");
        assert_eq!(error.receipt_state(), Some(ReceiptState::Rejected));
        assert_eq!(
            error.retry_class(),
            Some(RetryClass::AfterApproval),
            "the error keeps its registry retry class"
        );
    }

    #[test]
    fn a_stale_base_has_a_conflict_receipt() {
        assert_eq!(RuleError::StaleBase.code(), "write.version_conflict");
        assert_eq!(
            RuleError::StaleBase.receipt_state(),
            Some(ReceiptState::Conflict)
        );
    }
}
