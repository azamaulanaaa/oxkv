//! Tests for the hook pipeline internal diagnostic.

use oxkv_contracts::InternalDiagnostic;
use oxkv_hooks::{HookError, HookErrorCode};

#[test]
fn a_hook_invariant_names_the_hook_pipeline() {
    let error = HookError::invariant("The pipeline state is invalid.");
    assert_eq!(error.code(), HookErrorCode::InternalInvariantViolation);
    assert_eq!(error.code_str(), "internal.invariant_violation");
    assert_eq!(error.diagnostic(), Some(InternalDiagnostic::HookPipeline));
    assert_eq!(
        error
            .diagnostic()
            .expect("the hook error has a diagnostic")
            .diagnostic_name(),
        "hook_pipeline"
    );
}

#[test]
fn a_public_hook_error_has_no_internal_diagnostic() {
    let error = HookError::budget_exhausted("The step budget is exhausted.");
    assert_eq!(error.code(), HookErrorCode::PolicyBudgetExhausted);
    assert_eq!(error.diagnostic(), None);
}
