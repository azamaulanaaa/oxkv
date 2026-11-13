//! Golden vectors for the hook contract.
//!
//! Every vector file is append-only for one contract version. A released case
//! does not change.

mod support;

use oxkv_contracts::version::CommitVersion;
use oxkv_hooks::{HookPipeline, Key, ReplayRecord, RuleId, replay};

#[test]
fn the_golden_contract_is_pinned() {
    let text = include_str!("vectors/hooks.json");
    let value: serde_json::Value = serde_json::from_str(text).expect("the file is valid JSON");
    assert_eq!(value["contract"].as_str(), Some("oxkv/hooks/v1"));
}

fn commit_version() -> CommitVersion {
    CommitVersion::new(9)
}

#[test]
fn pre_write_rejection_changes_no_state() {
    let case = support::case("pre_write_rejection_changes_no_state");
    let transaction = support::rejected_transaction();
    let before = transaction.clone();
    let result = support::rejecting_pipeline().execute(&transaction, commit_version());
    let error = result.expect_err("the pre-write rule rejects the batch");
    assert_eq!(error.code_str(), support::expected_str(&case, "code"));
    assert_eq!(
        error.rule_id().map(RuleId::as_str),
        Some(support::expected_str(&case, "rule_id"))
    );
    assert_eq!(error.op_index(), Some(1));
    assert_eq!(error.key(), Some(&Key::from("forbidden")));
    assert_eq!(transaction, before);
    assert!(support::expected_bool(&case, "fingerprint_unchanged"));
}

#[test]
fn post_write_order_equals_batch_order() {
    let case = support::case("post_write_order_equals_batch_order");
    let outcome = support::ordered_pipeline()
        .execute(&support::sample_transaction(), commit_version())
        .expect("the batch is accepted");
    assert!(support::expected_bool(&case, "accepted"));
    let order: Vec<String> = outcome
        .notifications()
        .iter()
        .map(|notification| {
            let op_index = notification.op_index();
            let kind = notification.kind().as_str();
            format!("{op_index}:{kind}")
        })
        .collect();
    assert_eq!(order, support::expected_strings(&case, "order"));
    let emission_indexes: Vec<u64> = outcome
        .notifications()
        .iter()
        .map(|notification| u64::from(notification.emission_index()))
        .collect();
    assert_eq!(
        emission_indexes,
        support::expected_u64s(&case, "emission_indexes")
    );
}

#[test]
fn post_write_failure_aborts_batch() {
    let case = support::case("post_write_failure_aborts_batch");
    let transaction = support::failing_transaction();
    let mut state = support::pre_state(&transaction);
    let before = state.clone();
    match support::failing_pipeline().execute(&transaction, commit_version()) {
        Ok(outcome) => support::apply(&outcome, &mut state),
        Err(error) => {
            assert!(!support::expected_bool(&case, "accepted"));
            assert_eq!(error.code_str(), support::expected_str(&case, "code"));
            assert_eq!(
                error.rule_id().map(RuleId::as_str),
                Some(support::expected_str(&case, "rule_id"))
            );
            assert_eq!(
                error.op_index(),
                Some(1),
                "the failure aborts at the failed operation"
            );
        }
    }
    assert_eq!(state, before);
    assert!(support::expected_bool(&case, "state_unchanged"));
}

#[test]
fn replay_notifications_are_identical() {
    let case = support::case("replay_notifications_are_identical");
    let pipeline = support::ordered_pipeline();
    let records = vec![
        ReplayRecord::new(support::sample_transaction(), commit_version()),
        ReplayRecord::new(support::sample_transaction(), commit_version()),
    ];
    let outcomes = replay(&pipeline, &records).expect("the replay is accepted");
    assert_eq!(outcomes.len(), 2);
    assert_eq!(
        outcomes[0].notification_bytes(),
        outcomes[1].notification_bytes()
    );
    assert_eq!(outcomes[0].notifications(), outcomes[1].notifications());
    let hexes: Vec<String> = outcomes[0]
        .notification_bytes()
        .iter()
        .map(|bytes| support::hex(bytes))
        .collect();
    assert_eq!(hexes, support::expected_strings(&case, "notification_hex"));
}

#[test]
fn a_hook_cannot_mutate_the_batch() {
    let case = support::case("a_hook_cannot_mutate_the_batch");
    let transaction = support::sample_transaction();
    let before = transaction.clone();
    let fingerprint = transaction.fingerprint();
    support::ordered_pipeline()
        .execute(&transaction, commit_version())
        .expect("the batch is accepted");
    assert_eq!(transaction, before);
    assert_eq!(transaction.fingerprint(), fingerprint);
    assert_eq!(
        u64::try_from(transaction.op_count()).expect("the count fits"),
        support::expected_u64(&case, "op_count")
    );
    assert!(support::expected_bool(&case, "fingerprint_unchanged"));
}

#[test]
fn duplicate_validator_registration_is_deterministic() {
    let case = support::case("duplicate_validator_registration_is_deterministic");
    let first = duplicate_registration_error();
    let second = duplicate_registration_error();
    assert_eq!(first, second);
    assert_eq!(first.to_string(), second.to_string());
    assert_eq!(first.code_str(), support::expected_str(&case, "code"));
    assert_eq!(
        first.rule_id().as_str(),
        support::expected_str(&case, "rule_id")
    );
    assert!(support::expected_bool(&case, "same_message"));
}

fn duplicate_registration_error() -> oxkv_hooks::PipelineBuildError {
    let mut builder = HookPipeline::builder();
    builder
        .register_pre(support::AllowAll::new())
        .expect("the first rule is unique");
    builder
        .register_pre(support::AllowAll::new())
        .expect_err("the second rule is a duplicate")
}

#[test]
fn notification_bytes_are_native_wasm_identical() {
    let case = support::case("notification_bytes_are_native_wasm_identical");
    let outcome = support::ordered_pipeline()
        .execute(&support::sample_transaction(), commit_version())
        .expect("the batch is accepted");
    let hexes: Vec<String> = outcome
        .notification_bytes()
        .iter()
        .map(|bytes| support::hex(bytes))
        .collect();
    assert_eq!(hexes, support::expected_strings(&case, "notification_hex"));
}

#[test]
fn value_budget_is_named() {
    let case = support::case("value_budget_is_named");
    let error = support::value_budget_pipeline()
        .execute(&support::large_value_transaction(), commit_version())
        .expect_err("the value is above the budget");
    assert_eq!(error.code_str(), support::expected_str(&case, "code"));
    assert_eq!(error.path(), Some("value"));
}

#[test]
fn step_budget_is_named() {
    let case = support::case("step_budget_is_named");
    let error = support::step_budget_pipeline()
        .execute(&support::sample_transaction(), commit_version())
        .expect_err("the step budget is empty");
    assert_eq!(error.code_str(), support::expected_str(&case, "code"));
}

#[test]
fn batch_budget_is_named() {
    let case = support::case("batch_budget_is_named");
    let error = support::batch_budget_pipeline()
        .execute(&support::sample_transaction(), commit_version())
        .expect_err("the batch is above the operation budget");
    assert_eq!(error.code_str(), support::expected_str(&case, "code"));
}

#[test]
fn event_payload_budget_is_named() {
    let case = support::case("event_payload_budget_is_named");
    let error = support::payload_budget_pipeline()
        .execute(&support::sample_transaction(), commit_version())
        .expect_err("the notification payload is above the event budget");
    assert_eq!(error.code_str(), support::expected_str(&case, "code"));
    assert_eq!(
        error.rule_id().map(RuleId::as_str),
        Some(support::expected_str(&case, "rule_id"))
    );
}

#[test]
fn unknown_field_is_named() {
    let case = support::case("unknown_field_is_named");
    let error = support::outside_pipeline()
        .execute(&support::sample_transaction(), commit_version())
        .expect_err("the rule reads outside the envelope");
    assert_eq!(error.code_str(), support::expected_str(&case, "code"));
    assert_eq!(
        error.rule_id().map(RuleId::as_str),
        Some(support::expected_str(&case, "rule_id"))
    );
    assert_eq!(error.path(), Some("key"));
}

#[test]
fn validator_view_uses_pre_value_for_current_key() {
    let case = support::case("validator_view_uses_pre_value_for_current_key");
    let transaction = support::prior_transaction();
    let outcome = support::pre_value_pipeline()
        .execute(&transaction, commit_version())
        .expect("the validator reads the pre-transaction value");
    assert!(support::expected_bool(&case, "accepted"));
    let pre = transaction
        .pre_state()
        .get(&Key::from("alpha"))
        .expect("the key is in the envelope")
        .as_ref()
        .expect("the key has a prior row");
    assert_eq!(
        pre.value(),
        support::expected_str(&case, "pre_value").as_bytes()
    );
    let staged = outcome.applied_value(&Key::from("alpha"));
    assert_eq!(
        staged.expect("the key has a staged value"),
        support::expected_str(&case, "staged_value").as_bytes()
    );
}

#[test]
fn the_error_codes_are_in_the_canonical_registry() {
    for code in oxkv_hooks::HookErrorCode::ALL {
        assert!(
            oxkv_contracts::error::lookup(code.as_str()).is_some(),
            "the code {code} is absent from the registry"
        );
    }
}
