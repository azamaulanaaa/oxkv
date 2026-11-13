//! Enforcement behavior tests.

mod support;

use support::ScenarioOutcome;

fn state(outcome: &ScenarioOutcome) -> &str {
    outcome.receipt_state.as_deref().unwrap_or("none")
}

#[test]
fn a_committed_write_writes_one_audit_record() {
    let outcome = support::run("committed_write");
    assert_eq!(state(&outcome), "Committed");
    assert_eq!(outcome.audit_count, 1);
    assert_eq!(outcome.receipt_count, 1);
    assert!(outcome.data_changed);
    assert!(outcome.chain_verified);
}

#[test]
fn a_rejected_write_writes_one_receipt_and_one_audit_record() {
    let outcome = support::run("rejected_policy_write");
    assert_eq!(state(&outcome), "Rejected");
    assert_eq!(outcome.reason.as_deref(), Some("auth.forbidden"));
    assert_eq!(outcome.audit_count, 1);
    assert_eq!(outcome.receipt_count, 1);
    assert!(!outcome.data_changed);
}

#[test]
fn a_stale_base_with_a_policy_violation_returns_conflict() {
    let outcome = support::run("stale_base_policy_violation");
    assert_eq!(state(&outcome), "Conflict");
    assert_eq!(outcome.reason.as_deref(), Some("write.version_conflict"));
    assert_eq!(outcome.audit_count, 2);
    assert!(!outcome.data_changed);
}

#[test]
fn a_non_strict_over_bound_write_stays_queued_then_times_out() {
    let outcome = support::run("non_strict_over_bound_timeout");
    assert_eq!(outcome.queued_state.as_deref(), Some("Queued"));
    assert_eq!(outcome.queued_reason.as_deref(), Some("projection.lagging"));
    assert_eq!(outcome.queued_audit_count, Some(0));
    assert_eq!(state(&outcome), "Rejected");
    assert_eq!(outcome.reason.as_deref(), Some("write.queue_timeout"));
    assert_eq!(outcome.audit_count, 1);
    assert!(!outcome.data_changed);
}

#[test]
fn a_strict_over_bound_write_rejects_at_once() {
    let outcome = support::run("strict_over_bound");
    assert_eq!(state(&outcome), "Rejected");
    assert_eq!(outcome.reason.as_deref(), Some("authorization.stale_facts"));
    assert_eq!(outcome.audit_count, 1);
    assert_eq!(outcome.staleness_distance, Some(3));
    assert!(!outcome.data_changed);
}

#[test]
fn a_failed_audit_append_rejects_and_changes_no_state() {
    let outcome = support::run("audit_append_failure");
    assert_eq!(outcome.error_code.as_deref(), Some("audit.unavailable"));
    assert_eq!(outcome.audit_count, 0);
    assert_eq!(outcome.receipt_count, 0);
    assert!(!outcome.data_changed);
}

#[test]
fn a_decision_list_names_the_failing_row_rule_and_reason() {
    let outcome = support::run("per_row_decision_list");
    assert_eq!(state(&outcome), "Rejected");
    assert_eq!(outcome.decisions.len(), 2);
    let failing = &outcome.decisions[1];
    assert_eq!(failing.row_key, "row:8b");
    assert_eq!(failing.rule, "rule.payload");
    assert_eq!(failing.verdict, "reject");
    assert_eq!(failing.reason, "auth.forbidden");
}

#[test]
fn a_batch_verdict_is_the_conjunction_of_row_verdicts() {
    let outcome = support::run("batch_conjunction");
    assert_eq!(state(&outcome), "Rejected");
    assert!(!outcome.data_changed);
    assert_eq!(outcome.decisions[0].verdict, "allow");
    assert_eq!(outcome.decisions[1].verdict, "reject");
}

#[test]
fn the_audit_chain_verifies() {
    let outcome = support::run("audit_chain_verifies");
    assert_eq!(outcome.audit_count, 2);
    assert!(outcome.chain_verified);
    assert_ne!(outcome.previous_hash_hex, "0".repeat(64));
}

#[test]
fn a_receipt_names_the_audit_record() {
    let outcome = support::run("receipt_names_audit");
    assert!(outcome.receipt_names_audit);
}

#[test]
fn a_budget_failure_is_rejected_not_conflict() {
    let outcome = support::run("budget_exhaustion");
    assert_eq!(state(&outcome), "Rejected");
    assert_eq!(outcome.reason.as_deref(), Some("policy.budget_exhausted"));
    assert!(!outcome.data_changed);
}

#[test]
fn a_projection_checksum_failure_fails_closed() {
    let outcome = support::run("projection_checksum_failure");
    assert_eq!(state(&outcome), "Rejected");
    assert_eq!(
        outcome.reason.as_deref(),
        Some("projection.checksum_failed")
    );
    assert_eq!(outcome.audit_count, 1);
    assert!(!outcome.data_changed);
}

#[test]
fn a_client_projection_write_rejects() {
    let outcome = support::run("projection_client_write");
    assert_eq!(
        outcome.error_code.as_deref(),
        Some("projection.client_write_rejected")
    );
}

#[test]
fn a_cross_shard_write_rejects_before_version_and_policy() {
    let outcome = support::run("cross_shard_write");
    assert_eq!(state(&outcome), "Rejected");
    assert_eq!(
        outcome.reason.as_deref(),
        Some("shard.cross_mutation_rejected")
    );
    assert_eq!(outcome.decisions[0].rule, "shard");
    assert!(!outcome.data_changed);
}

#[test]
fn a_retry_with_the_same_key_and_body_returns_one_receipt() {
    let outcome = support::run("idempotency_retry");
    assert_eq!(state(&outcome), "Committed");
    assert_eq!(outcome.audit_count, 1);
    assert_eq!(outcome.receipt_count, 1);
}

#[test]
fn a_reused_key_with_a_different_body_writes_no_second_receipt() {
    let outcome = support::run("idempotency_key_reuse");
    assert_eq!(
        outcome.error_code.as_deref(),
        Some("write.idempotency_key_reuse")
    );
    assert_eq!(outcome.receipt_count, 1);
    assert_eq!(outcome.audit_count, 1);
}

#[test]
fn a_base_version_ahead_of_the_current_version_rejects() {
    let outcome = support::run("stale_base_version");
    assert_eq!(state(&outcome), "Rejected");
    assert_eq!(outcome.reason.as_deref(), Some("write.stale_base_version"));
    assert!(!outcome.data_changed);
}

#[test]
fn a_request_without_a_base_version_rejects() {
    let outcome = support::run("missing_base_version");
    assert_eq!(state(&outcome), "Rejected");
    assert_eq!(
        outcome.reason.as_deref(),
        Some("write.missing_base_version")
    );
    assert_eq!(outcome.audit_count, 1);
    assert!(!outcome.data_changed);
}
