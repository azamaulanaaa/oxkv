//! Golden vector runner for the tokenizer contract.
//!
//! Every vector file is append-only for one contract version. A later essay
//! adds a case to the matching file. A released case does not change.

mod support;

use oxkv_index::tokenizer::{TokenBudget, TokenizerId, TokenizerIdentity, tokenize_text};

#[test]
fn tokenizer_identity_golden_vector_is_pinned() {
    let file = support::load(include_str!("vectors/tokenizer.json"));
    assert_eq!(file.contract, "tokenizer/v1");
    let identity = &file.value["identity"];
    for id in TokenizerId::ALL {
        let case = &identity[id.as_str()];
        let version = case["version"]
            .as_str()
            .expect("the identity names a version");
        assert_eq!(version, "tokenizer/v1");
        let unicode_version = case["unicode_version"]
            .as_str()
            .expect("the identity names a Unicode data version");
        let tokenizer = TokenizerIdentity::v1(*id);
        assert_eq!(tokenizer.unicode_version(), unicode_version);
        assert_eq!(
            tokenizer.normalization(),
            case["normalization"]
                .as_str()
                .expect("the normalization is text")
        );
        assert_eq!(
            tokenizer.case_folding(),
            case["case_folding"]
                .as_str()
                .expect("the case folding is text")
        );
        assert_eq!(
            tokenizer.segmentation(),
            case["segmentation"]
                .as_str()
                .expect("the segmentation is text")
        );
        assert_eq!(
            tokenizer.stop_words(),
            support::text_array(&case["stop_words"])
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            tokenizer.fingerprint().to_hex(),
            case["fingerprint"]
                .as_str()
                .expect("the fingerprint is text")
        );
    }
}

#[test]
fn tokenizer_golden_vectors_are_pinned() {
    let file = support::load(include_str!("vectors/tokenizer.json"));
    let cases = file.value["cases"]
        .as_array()
        .expect("the golden file has a case list");
    for case in cases {
        let name = case["name"].as_str().expect("the case has a name");
        let id = TokenizerId::from_name(
            case["tokenizer"]
                .as_str()
                .expect("the case names a tokenizer"),
        )
        .expect("the tokenizer is registered");
        let text = case["text"].as_str().expect("the case has text");
        let tokens = tokenize_text(TokenizerIdentity::v1(id), text, TokenBudget::DEFAULT)
            .unwrap_or_else(|error| panic!("the case {name} failed: {error}"));
        let actual_tokens: Vec<String> =
            tokens.iter().map(|token| token.text().to_owned()).collect();
        assert_eq!(
            actual_tokens,
            support::text_array(&case["tokens"]),
            "the tokens changed for {name}"
        );
        let actual_positions: Vec<u64> = tokens
            .iter()
            .map(|token| u64::from(token.position()))
            .collect();
        assert_eq!(
            actual_positions,
            support::integer_array(&case["positions"]),
            "the positions changed for {name}"
        );
    }
}

#[test]
fn index_golden_vector_is_pinned() {
    let file = support::load(include_str!("vectors/index.json"));
    assert_eq!(file.contract, "index/v1");
    let index = support::sample_index();
    assert_eq!(
        support::hex(&index.segments()[0].encode()),
        file.value["segment_hex"]
            .as_str()
            .expect("the segment hex is text"),
        "the segment bytes changed"
    );
    assert_eq!(
        support::hex(&index.encode()),
        file.value["index_hex"]
            .as_str()
            .expect("the index hex is text"),
        "the index bytes changed"
    );
    assert_eq!(
        index.digest().to_hex(),
        file.value["index_digest"]
            .as_str()
            .expect("the digest is text"),
        "the index digest changed"
    );
    let result = index
        .search(&support::sample_search(), &oxkv_index::AllScope)
        .expect("the sample search succeeds");
    let expected = file.value["hits"]
        .as_array()
        .expect("the vector lists the hits");
    assert_eq!(result.hits.len(), expected.len());
    for (hit, expected) in result.hits.iter().zip(expected) {
        assert_eq!(
            support::hex(&hit.key),
            expected["key_hex"].as_str().expect("the key is text")
        );
        assert_eq!(
            hit.score,
            expected["score"].as_u64().expect("the score is an integer")
        );
    }
}

#[test]
fn index_build_is_byte_identical_on_every_target() {
    // This test is built for native and for wasm32. The pinned bytes are the
    // target-independent answer.
    let file = support::load(include_str!("vectors/index.json"));
    let index = support::sample_index();
    assert_eq!(
        support::hex(&index.encode()),
        file.value["index_hex"]
            .as_str()
            .expect("the index hex is text")
    );
}
