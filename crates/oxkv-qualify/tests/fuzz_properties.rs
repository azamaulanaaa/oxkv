//! Deterministic corpus-based property tests for every decoder.
//!
//! The tests check totality, determinism, the step budget, and the memory
//! bound. They do not use a network and they do not use operating-system
//! randomness.

use std::collections::BTreeMap;

use oxkv_qualify::fuzz::{
    PARITY_DIGEST, fuzz_artifact, fuzz_cursor_token, fuzz_error_code, fuzz_frame, fuzz_policy,
    fuzz_receipt, fuzz_table, run_corpus,
};
use oxkv_qualify::policy::{EvalBudget, FactValue, evaluate};
use oxkv_qualify::rng::FixedSeedRng;

#[test]
fn the_corpus_passes_and_is_deterministic() {
    let first = run_corpus();
    let second = run_corpus();
    assert!(first.passed(), "{:?}", first.failures);
    assert_eq!(first, second);
    assert_eq!(first.digest, PARITY_DIGEST);
    assert!(first.cases > 200);
}

#[test]
fn every_decoder_is_total_on_arbitrary_bytes() {
    let mut rng = FixedSeedRng::new(9);
    for index in 0..256_u64 {
        let length = usize::try_from(rng.next_bounded(97)).unwrap_or(0);
        let mut bytes = Vec::with_capacity(length);
        for _ in 0..length {
            bytes.push(u8::try_from(rng.next_bounded(256)).unwrap_or(0));
        }
        for tag in 0..=9_u8 {
            let _ = fuzz_cursor_token(tag, &bytes);
        }
        let _ = fuzz_frame(&bytes);
        let _ = fuzz_artifact(&bytes);
        let _ = fuzz_policy(&bytes);
        let _ = fuzz_table(&bytes);
        let _ = fuzz_error_code(&bytes);
        let _ = fuzz_receipt(u8::try_from(index % 256).unwrap_or(0));
    }
}

#[test]
fn the_policy_step_counter_is_monotonic_and_bounded() {
    let mut facts = BTreeMap::new();
    facts.insert("tier".to_owned(), FactValue::Text("gold".to_owned()));
    let budget = EvalBudget::default();
    let mut previous = 0_u32;
    for length in 1..=64_usize {
        let expression = format!("and({})", vec!["allow"; length].join(","));
        let evaluation = evaluate(&expression, &facts, budget).expect("the chain evaluates");
        assert!(evaluation.stats.steps >= previous);
        assert!(evaluation.stats.steps <= budget.max_steps);
        assert!(evaluation.stats.arena <= budget.max_arena);
        assert!(evaluation.stats.depth <= budget.max_depth);
        previous = evaluation.stats.steps;
    }
}

#[test]
fn a_budget_exhaustion_is_a_named_outcome() {
    let mut facts = BTreeMap::new();
    facts.insert("tier".to_owned(), FactValue::Text("gold".to_owned()));
    let budget = EvalBudget {
        max_steps: 2,
        ..EvalBudget::default()
    };
    let error =
        evaluate("and(allow,allow,allow)", &facts, budget).expect_err("the budget is too small");
    assert_eq!(error.code(), "policy.budget_exhausted");
}
