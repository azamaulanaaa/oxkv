//! Essay freeze index and the freeze statement gate.

use oxkv_freeze::{
    ContractId, EssayFreezeClaim, GateError, REGISTER, check_claim, check_claim_coverage,
    check_claims, check_freeze_statement,
};
use serde_json::Value;

fn golden_index() -> Value {
    serde_json::from_str(include_str!("vectors/essay-freeze-index.json"))
        .expect("the vector file is JSON")
}

fn claims_from_golden() -> Vec<EssayFreezeClaim> {
    golden_index()["claims"]
        .as_array()
        .expect("the index has claims")
        .iter()
        .map(|claim| {
            let rows = claim["rows"]
                .as_array()
                .expect("the claim has rows")
                .iter()
                .map(|row| {
                    ContractId::parse(row.as_str().expect("the row is a string"))
                        .expect("the row is registered")
                })
                .collect();
            EssayFreezeClaim::new(
                claim["essay"].as_str().expect("the claim has an essay"),
                claim["statement"]
                    .as_str()
                    .expect("the claim has a statement"),
                rows,
            )
        })
        .collect()
}

#[test]
fn the_checked_in_essay_freeze_index_names_a_row_for_every_claim() {
    let file = golden_index();
    assert_eq!(file["contract"], "essay-freeze-index/v1");
    let claims = claims_from_golden();
    check_claims(&claims, REGISTER).expect("every claim names a register row");
}

#[test]
fn the_checked_in_essay_freeze_index_covers_every_register_row() {
    let claims = claims_from_golden();
    check_claim_coverage(&claims, REGISTER).expect("every row has a claim");
    for row in REGISTER {
        assert!(
            claims.iter().any(|claim| claim.rows.contains(&row.id)),
            "no claim names {id}",
            id = row.id
        );
    }
}

#[test]
fn a_freeze_without_a_row_fails() {
    for statement in [
        "This surface freezes the bytes.",
        "The format is frozen for 1.x.",
    ] {
        assert_eq!(
            check_freeze_statement(statement, REGISTER),
            Err(GateError::MissingRowReference {
                essay: "inline".to_owned()
            })
        );
    }
}

#[test]
fn a_freeze_with_a_row_passes() {
    assert_eq!(
        check_freeze_statement("Contract A2 freezes the tokenizer bytes.", REGISTER),
        Ok(())
    );
}

#[test]
fn a_claim_that_names_an_unknown_row_fails() {
    let claim = EssayFreezeClaim::new(
        "99.md",
        "Contract C1 and annex A12 freeze the bytes.",
        vec![ContractId::C1, ContractId::A12],
    );
    assert_eq!(
        check_claim(&claim, &REGISTER[..1]),
        Err(GateError::UnknownRow {
            essay: "99.md".to_owned(),
            id: ContractId::A12
        })
    );
}

#[test]
fn a_claim_that_omits_the_row_text_fails() {
    let claim = EssayFreezeClaim::new(
        "99.md",
        "The manifest freezes the bytes.",
        vec![ContractId::C1],
    );
    assert_eq!(
        check_claim(&claim, REGISTER),
        Err(GateError::RowNotNamed {
            essay: "99.md".to_owned(),
            id: ContractId::C1
        })
    );
}

#[test]
fn an_empty_claim_fails() {
    let claim = EssayFreezeClaim::new("99.md", "The bytes freeze.", Vec::new());
    assert_eq!(
        check_claim(&claim, REGISTER),
        Err(GateError::MissingRowReference {
            essay: "99.md".to_owned()
        })
    );
}
