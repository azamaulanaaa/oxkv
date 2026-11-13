//! Golden vector runner for write enforcement.

mod support;

use serde_json::Value;

/// The scenario order in the golden file.
const SCENARIOS: &[&str] = &[
    "committed_write",
    "rejected_policy_write",
    "stale_base_policy_violation",
    "non_strict_over_bound_timeout",
    "strict_over_bound",
    "audit_append_failure",
    "per_row_decision_list",
    "batch_conjunction",
    "audit_chain_verifies",
    "receipt_names_audit",
    "budget_exhaustion",
    "projection_checksum_failure",
    "projection_client_write",
    "cross_shard_write",
    "idempotency_retry",
    "idempotency_key_reuse",
    "stale_base_version",
    "missing_base_version",
];

/// One golden case.
struct GoldenCase {
    name: String,
    expected: Value,
}

/// Loads one golden file.
fn load(text: &str) -> (String, Vec<GoldenCase>) {
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    let contract = value["contract"]
        .as_str()
        .expect("the golden file names a contract")
        .to_owned();
    let cases = value["cases"]
        .as_array()
        .expect("the golden file has a case list")
        .iter()
        .map(|case| GoldenCase {
            name: case["name"]
                .as_str()
                .expect("the case names a scenario")
                .to_owned(),
            expected: case["expected"].clone(),
        })
        .collect();
    (contract, cases)
}

#[test]
fn the_enforcement_golden_vectors_are_pinned() {
    let (contract, cases) = load(include_str!("vectors/enforcement.json"));
    assert_eq!(contract, "write-enforcement/v1");
    assert_eq!(cases.len(), SCENARIOS.len());
    for (case, name) in cases.iter().zip(SCENARIOS) {
        assert_eq!(&case.name, name, "the case order changed");
        let actual = support::run(&case.name).to_json();
        let expected = case
            .expected
            .as_object()
            .expect("the case has an expected object");
        assert!(
            !expected.is_empty(),
            "the case {} has no expectations",
            case.name
        );
        for (field, value) in expected {
            assert_eq!(
                &actual[field], value,
                "the case {} changed the field {}",
                case.name, field
            );
        }
    }
}
