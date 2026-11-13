//! Golden vector runner for the rule lifecycle.
//!
//! The vector file is append-only for one contract version. A released case
//! does not change.

mod support;

use oxkv_contracts::receipt::ReceiptState;
use serde_json::Value;

struct GoldenFile {
    contract: String,
    cases: Vec<GoldenCase>,
}

struct GoldenCase {
    name: String,
    expected: Value,
}

fn load(text: &str) -> GoldenFile {
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
                .expect("the case names a vector")
                .to_owned(),
            expected: case["expected"].clone(),
        })
        .collect();
    GoldenFile { contract, cases }
}

fn expected_state(case: &GoldenCase, field: &str) -> Option<ReceiptState> {
    case.expected[field]
        .as_str()
        .map(|name| ReceiptState::from_name(name).expect("the receipt state is known"))
}

#[test]
fn the_content_address_is_pinned() {
    let fixture = support::fixture();
    assert_eq!(
        fixture.old.version().to_hex(),
        "ebf59654a56a678e368fb03a5208ff94fa5ff1631151a69b3cc95deb37c70254"
    );
    assert_eq!(
        fixture.manifest.digest().to_hex(),
        "d578a5e534f378b979811ed60264deab2386fc8e622021b35ecb00cb7228a130"
    );
}

#[test]
fn the_rule_lifecycle_golden_vector_is_pinned() {
    let file = load(include_str!("vectors/rule-lifecycle.json"));
    assert_eq!(file.contract, "rule-lifecycle/v1");
    assert!(!file.cases.is_empty());
    for case in &file.cases {
        let result = support::run_case(&case.name);
        if let Some(expected) = expected_state(case, "receipt") {
            assert_eq!(
                result.receipt,
                Some(expected),
                "the receipt changed for {}",
                case.name
            );
        }
        if let Some(expected) = case.expected["reason"].as_str() {
            assert_eq!(
                result.reason,
                Some(expected),
                "the reason changed for {}",
                case.name
            );
        }
        if let Some(facts) = case.expected.as_object() {
            for (name, value) in facts {
                if name == "receipt" || name == "reason" {
                    continue;
                }
                let expected = value.as_bool().expect("a golden fact is a boolean");
                assert_eq!(
                    result.facts.get(name.as_str()),
                    Some(&expected),
                    "the fact {name} changed for {}",
                    case.name
                );
            }
        }
    }
}

#[test]
fn every_golden_case_names_a_unique_vector() {
    let file = load(include_str!("vectors/rule-lifecycle.json"));
    let mut names: Vec<&str> = file.cases.iter().map(|case| case.name.as_str()).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count, "a golden case name repeats");
}
