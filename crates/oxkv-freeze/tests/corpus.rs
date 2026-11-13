//! Qualification corpus coverage.

use oxkv_freeze::{ContractId, CorpusEntry, CoverageError, REGISTER, VectorCorpus, check_corpus};
use serde_json::Value;

fn golden_corpus() -> Value {
    serde_json::from_str(include_str!("vectors/corpus.json")).expect("the vector file is JSON")
}

fn corpus_from_golden() -> VectorCorpus {
    let file = golden_corpus();
    assert_eq!(file["contract"], "freeze-corpus/v1");
    let entries = file["entries"]
        .as_array()
        .expect("the vector has entries")
        .iter()
        .map(|entry| CorpusEntry {
            contract: ContractId::parse(
                entry["contract"]
                    .as_str()
                    .expect("the entry has a contract"),
            )
            .expect("the entry contract is registered"),
            vector: entry["vector"]
                .as_str()
                .expect("the entry has a vector")
                .to_owned(),
        })
        .collect();
    VectorCorpus::new(entries)
}

#[test]
fn a_corpus_that_covers_every_row_passes() {
    let corpus = corpus_from_golden();
    assert_eq!(corpus.len(), ContractId::ALL.len());
    assert_eq!(check_corpus(&corpus, REGISTER), Ok(()));
    assert_eq!(corpus.missing(REGISTER), Vec::<ContractId>::new());
}

#[test]
fn a_corpus_missing_a_row_fails() {
    let ids = ContractId::ALL
        .iter()
        .copied()
        .filter(|id| *id != ContractId::A12);
    let corpus = VectorCorpus::from_ids(ids);
    assert_eq!(
        corpus.check(REGISTER),
        Err(CoverageError::MissingRow {
            id: ContractId::A12
        })
    );
    assert_eq!(corpus.missing(REGISTER), vec![ContractId::A12]);
}

#[test]
fn an_empty_corpus_fails_on_the_first_row() {
    let corpus = VectorCorpus::default();
    assert_eq!(
        check_corpus(&corpus, REGISTER),
        Err(CoverageError::MissingRow { id: ContractId::C1 })
    );
}

#[test]
fn every_register_row_has_a_vector_reference() {
    for row in REGISTER {
        assert_ne!(row.vectors, "");
        assert!(
            row.vectors
                .contains(row.id.as_str().to_ascii_lowercase().as_str()),
            "the vector reference for {id} does not name it: {vectors}",
            id = row.id,
            vectors = row.vectors
        );
    }
}
