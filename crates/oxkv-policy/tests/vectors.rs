//! Golden vector runner for policy AST v1.
//!
//! Every executable vector runs the production evaluator and the test-only
//! differential oracle. The test checks that both agree on the verdict, the
//! reason, the evaluated path, and every budget counter.

mod support;

use oxkv_policy::{EvalContext, Evaluation, Expr, Literal, PathRoot, Policy, Row, RuleId};
use serde_json::Value;
use support::{BatchEntry, GoldenCase, GoldenFile};

fn load_vectors() -> GoldenFile {
    support::load(include_str!("vectors/policy-v1.json"))
}

#[test]
fn policy_golden_vectors_are_pinned() {
    let file = load_vectors();
    assert_eq!(file.contract, "policy-ast/v1");
    assert!(
        file.cases.len() >= 14,
        "the corpus covers every required case"
    );
    for case in &file.cases {
        run_case(case);
    }
}

fn run_case(case: &GoldenCase) {
    let rule = RuleId::new(case.rule.clone()).expect("the rule identity is valid");
    let policy_result = Policy::new(
        rule,
        support::budget(&case.budget),
        support::expr(&case.expr),
    );
    if let Some(expected) = &case.compile_error {
        let error = match policy_result {
            Err(error) => error,
            Ok(policy) => {
                let schema = case
                    .schema
                    .as_ref()
                    .expect("a compile error case has a schema or a rejected tree");
                policy
                    .validate_fields(&support::schema(schema))
                    .expect_err("the schema rejects the policy")
            }
        };
        assert_eq!(
            error.code(),
            expected,
            "the compile error for {}",
            case.name
        );
        return;
    }
    let policy = policy_result.expect("the policy is valid");
    if let Some(schema) = &case.schema {
        policy
            .validate_fields(&support::schema(schema))
            .expect("the schema declares every path");
    }
    let expect = case
        .expect
        .as_ref()
        .expect("an executable case has expected output");
    assert_pins(&case.name, &policy, expect);
    let decoded =
        Policy::from_canonical_bytes(&policy.canonical_bytes()).expect("the bytes decode");
    assert_eq!(decoded.version(), policy.version());
    assert_eq!(decoded.canonical_bytes(), policy.canonical_bytes());
    if let Some(batch) = &case.batch {
        run_batch(&policy, case, batch);
    } else {
        run_single(&policy, case, expect);
    }
}

fn assert_pins(name: &str, policy: &Policy, expect: &Value) {
    assert_eq!(
        support::hex(&policy.canonical_bytes()),
        expect["canonical_hex"]
            .as_str()
            .unwrap_or_else(|| panic!("the case {name} pins the AST bytes")),
        "the canonical AST bytes changed for {name}"
    );
    assert_eq!(
        policy.version().to_hex(),
        expect["policy_version"]
            .as_str()
            .unwrap_or_else(|| panic!("the case {name} pins the policy version")),
        "the policy version changed for {name}"
    );
}

fn run_single(policy: &Policy, case: &GoldenCase, expect: &Value) {
    let before = support::row(&case.before);
    let after = case.after.as_ref().map(support::row);
    let after = after.as_ref().unwrap_or(&before);
    let key = support::unhex(&case.row_key);
    let context = if case.mode == "read" {
        EvalContext::read(&key, &before)
    } else {
        EvalContext::write(&key, &before, after)
    };
    let evaluation = policy.evaluate(&context);
    assert_evaluation(&case.name, &evaluation, expect);
    let oracle = support::oracle::evaluate(policy, &context);
    assert_oracle(&case.name, &evaluation, &oracle);
}

fn run_batch(policy: &Policy, case: &GoldenCase, batch: &[BatchEntry]) {
    let before: Vec<Row> = batch
        .iter()
        .map(|entry| support::row(&entry.before))
        .collect();
    let after: Vec<Option<Row>> = batch
        .iter()
        .map(|entry| entry.after.as_ref().map(support::row))
        .collect();
    let keys: Vec<Vec<u8>> = batch
        .iter()
        .map(|entry| support::unhex(&entry.row_key))
        .collect();
    let contexts: Vec<EvalContext<'_>> = batch
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let stored = &before[index];
            let proposed = after[index].as_ref().unwrap_or(stored);
            if case.mode == "read" {
                EvalContext::read(&keys[index], stored)
            } else {
                EvalContext::write(&keys[index], stored, proposed)
            }
        })
        .collect();
    let outcome = policy.evaluate_batch(&contexts);
    let expect = case
        .expect
        .as_ref()
        .expect("a batch case has an expected verdict");
    assert_eq!(
        outcome.verdict.name(),
        expect["verdict"]
            .as_str()
            .unwrap_or_else(|| panic!("the batch case {} pins the verdict", case.name))
    );
    assert_eq!(outcome.decisions.len(), batch.len());
    assert_eq!(outcome.may_commit(), outcome.verdict.is_allow());
    for (index, entry) in batch.iter().enumerate() {
        let decision = &outcome.decisions[index];
        assert_evaluation(
            &format!("{}[{}]", case.name, entry.row_key),
            decision,
            &entry.expect,
        );
        let oracle = support::oracle::evaluate(policy, &contexts[index]);
        assert_oracle(
            &format!("{}[{}]", case.name, entry.row_key),
            decision,
            &oracle,
        );
    }
}

fn assert_evaluation(name: &str, evaluation: &Evaluation, expect: &Value) {
    assert_eq!(
        evaluation.status.name(),
        expect["status"]
            .as_str()
            .unwrap_or_else(|| panic!("the case {name} pins the status")),
        "the status changed for {name}"
    );
    assert_eq!(
        evaluation.reason.text(),
        expect["reason"]
            .as_str()
            .unwrap_or_else(|| panic!("the case {name} pins the reason")),
        "the reason changed for {name}"
    );
    assert_eq!(
        evaluation.path,
        support::expect_path(expect),
        "the evaluated path changed for {name}"
    );
    assert_eq!(
        evaluation.steps_used,
        expect["steps_used"]
            .as_u64()
            .unwrap_or_else(|| panic!("the case {name} pins the step count")),
        "the step count changed for {name}"
    );
    assert_eq!(
        evaluation.depth_used,
        u32::try_from(
            expect["depth_used"]
                .as_u64()
                .unwrap_or_else(|| panic!("the case {name} pins the depth"))
        )
        .expect("the depth fits u32"),
        "the depth changed for {name}"
    );
    assert_eq!(
        evaluation.arena_used,
        expect["arena_used"]
            .as_u64()
            .unwrap_or_else(|| panic!("the case {name} pins the arena")),
        "the arena changed for {name}"
    );
    assert_eq!(
        support::hex(&evaluation.canonical_bytes()),
        expect["explanation_hex"]
            .as_str()
            .unwrap_or_else(|| panic!("the case {name} pins the explanation bytes")),
        "the explanation bytes changed for {name}"
    );
    match support::expect_budget(expect) {
        Some((kind, limit, used, depth)) => {
            let failure = evaluation
                .budget_failure()
                .unwrap_or_else(|| panic!("the case {name} expects a budget failure"));
            assert_eq!(failure.budget, kind, "the budget kind changed for {name}");
            assert_eq!(failure.limit, limit, "the budget limit changed for {name}");
            assert_eq!(failure.used, used, "the budget use changed for {name}");
            assert_eq!(failure.depth, depth, "the budget depth changed for {name}");
            assert!(
                !evaluation.is_allow(),
                "the case {name} must not allow after exhaustion"
            );
            assert_eq!(
                evaluation.status.receipt_state(),
                Some(oxkv_contracts::receipt::ReceiptState::Rejected),
                "the case {name} must map exhaustion to Rejected"
            );
        }
        None => assert!(
            evaluation.budget_failure().is_none(),
            "the case {name} must not report a budget failure"
        ),
    }
}

fn assert_oracle(name: &str, evaluation: &Evaluation, oracle: &support::oracle::OracleOutcome) {
    assert_eq!(
        oracle.verdict,
        evaluation.verdict(),
        "the oracle verdict differs for {name}"
    );
    assert_eq!(
        oracle.reason, evaluation.reason,
        "the oracle reason differs for {name}"
    );
    assert_eq!(
        oracle.path, evaluation.path,
        "the oracle path differs for {name}"
    );
    assert_eq!(
        oracle.budget,
        evaluation.budget_failure(),
        "the oracle budget differs for {name}"
    );
    assert_eq!(
        oracle.steps, evaluation.steps_used,
        "the oracle step count differs for {name}"
    );
    assert_eq!(
        oracle.depth, evaluation.depth_used,
        "the oracle depth differs for {name}"
    );
    assert_eq!(
        oracle.arena, evaluation.arena_used,
        "the oracle arena differs for {name}"
    );
}

#[test]
fn explanation_bytes_are_stable_across_targets() {
    let file = load_vectors();
    let case = file
        .cases
        .iter()
        .find(|case| case.name == "native_wasm_stability")
        .expect("the corpus has a stability vector");
    let policy = Policy::new(
        RuleId::new(case.rule.clone()).expect("the rule identity is valid"),
        support::budget(&case.budget),
        support::expr(&case.expr),
    )
    .expect("the policy is valid");
    let row = support::row(&case.before);
    let evaluation = policy.evaluate_read(&support::unhex(&case.row_key), &row);
    let expect = case.expect.as_ref().expect("the case is executable");
    assert_eq!(
        support::hex(&evaluation.canonical_bytes()),
        expect["explanation_hex"]
            .as_str()
            .expect("the case pins the explanation bytes")
    );
}

#[test]
fn canonical_bytes_reject_corrupt_input() {
    let file = load_vectors();
    let case = file
        .cases
        .iter()
        .find(|case| case.name == "allow_owner_read")
        .expect("the corpus has an allow vector");
    let policy = Policy::new(
        RuleId::new(case.rule.clone()).expect("the rule identity is valid"),
        support::budget(&case.budget),
        support::expr(&case.expr),
    )
    .expect("the policy is valid");
    let mut trailing = policy.canonical_bytes();
    trailing.push(0);
    let error =
        Policy::from_canonical_bytes(&trailing).expect_err("trailing data is not canonical");
    assert_eq!(error.code(), "policy.parse_error");
    let mut magic = policy.canonical_bytes();
    magic[0] ^= 0xff;
    let error = Policy::from_canonical_bytes(&magic).expect_err("the magic is checked");
    assert_eq!(error.code(), "policy.parse_error");
}

#[test]
fn membership_order_does_not_change_the_policy_version() {
    let rule = || RuleId::new("accounts.membership").expect("the rule identity is valid");
    let forward = Policy::new(
        rule(),
        oxkv_policy::Budget::standard(),
        Expr::In {
            value: Box::new(Expr::path(PathRoot::Before, "owner")),
            set: vec![
                Literal::Text("alice".to_owned()),
                Literal::Text("bob".to_owned()),
            ],
        },
    )
    .expect("the policy is valid");
    let reverse = Policy::new(
        rule(),
        oxkv_policy::Budget::standard(),
        Expr::In {
            value: Box::new(Expr::path(PathRoot::Before, "owner")),
            set: vec![
                Literal::Text("bob".to_owned()),
                Literal::Text("alice".to_owned()),
            ],
        },
    )
    .expect("the policy is valid");
    assert_eq!(forward.version(), reverse.version());
    assert_eq!(forward.canonical_bytes(), reverse.canonical_bytes());
}
