//! Integration tests for the inverted index.
//!
//! The tests cover the named mismatch errors, the atomic write, the budget
//! limits, the versioned visibility, and the authority and replica parity.

mod support;

use oxkv_contracts::version::{CommitVersion, PrefixBounds};
use oxkv_index::{
    DocumentField, FieldSpec, IndexDocument, IndexManifest, ScorerSpec, SearchRequest, ShardIndex,
    TokenBudget, TokenizerIdentity, build_scope_index,
};

fn authority_index() -> ShardIndex {
    let mut index = ShardIndex::new(support::sample_manifest(), support::sample_bounds())
        .expect("the manifest and bounds are valid");
    let mut transaction = index
        .begin_write(CommitVersion::new(1))
        .expect("the version advances");
    for document in support::sample_documents() {
        transaction
            .put(&document)
            .expect("the sample document is valid");
    }
    transaction.commit().expect("the commit succeeds");
    index
}

#[test]
fn the_index_encoding_round_trips() {
    let index = support::sample_index();
    let decoded = ShardIndex::decode(
        &index.encode(),
        support::sample_manifest(),
        support::sample_bounds(),
    )
    .expect("the index decodes");
    assert_eq!(decoded, index);
}

#[test]
fn a_flipped_index_byte_returns_index_mismatch() {
    let index = support::sample_index();
    let bytes = index.encode();
    let positions = [
        0,
        4,
        8,
        40,
        bytes.len() / 2,
        bytes.len() - 33,
        bytes.len() - 1,
    ];
    for position in positions {
        let mut tampered = bytes.clone();
        tampered[position] ^= 0x01;
        let error = ShardIndex::decode(
            &tampered,
            support::sample_manifest(),
            support::sample_bounds(),
        )
        .expect_err("the flipped byte is detected");
        assert_eq!(
            error.code(),
            "search.index_mismatch",
            "the byte at {position} was not rejected as an index mismatch"
        );
    }
}

#[test]
fn a_tokenizer_mismatch_returns_search_tokenizer_mismatch() {
    let index = support::sample_index();
    let request = support::sample_search().with_tokenizer(TokenizerIdentity::ascii_word());
    let error = index
        .search(&request, &oxkv_index::AllScope)
        .expect_err("the tokenizer does not match");
    assert_eq!(error.code(), "search.tokenizer_mismatch");
}

#[test]
fn an_unregistered_tokenizer_returns_search_tokenizer_unknown() {
    let error = TokenizerIdentity::from_parts("unknown-word", 1, "none")
        .expect_err("the tokenizer is not registered");
    assert_eq!(error.code(), "search.tokenizer_unknown");
}

#[test]
fn ranking_is_identical_on_authority_and_replica() {
    let authority = authority_index();
    let replica = support::sample_index();
    let request = support::sample_search();
    let authoritative = authority
        .search(&request, &oxkv_index::AllScope)
        .expect("the authority search succeeds");
    let replicated = replica
        .search(&request, &oxkv_index::AllScope)
        .expect("the replica search succeeds");
    assert_eq!(authoritative, replicated);
}

#[test]
fn no_out_of_scope_posting_is_built_or_returned() {
    let scope = PrefixBounds::new(b"a".to_vec(), b"c".to_vec());
    let index = build_scope_index(
        &support::sample_manifest(),
        &support::sample_bounds(),
        &scope,
        support::sample_documents(),
        CommitVersion::new(1),
    )
    .expect("the scoped build succeeds");
    assert_eq!(
        index.postings_for("title", "café"),
        [] as [&oxkv_index::Posting; 0]
    );
    assert_ne!(
        index.postings_for("title", "rust"),
        [] as [&oxkv_index::Posting; 0]
    );
    let request = SearchRequest::new("café", CommitVersion::new(1));
    let result = index
        .search(&request, &scope)
        .expect("the scoped search succeeds");
    assert_eq!(result.total_hits, 0);
    let visible = index
        .documents_at(CommitVersion::new(1))
        .expect("the pin is valid");
    assert!(!visible.contains_key(b"c".as_slice()));
}

#[test]
fn a_write_and_its_postings_commit_atomically() {
    let manifest = IndexManifest::new(
        vec![FieldSpec::new("title", TokenizerIdentity::unicode_word())],
        TokenizerIdentity::unicode_word(),
        ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
        100_000,
        TokenBudget::new(1, 64),
    )
    .expect("the manifest is valid");
    let bounds = PrefixBounds::new(Vec::new(), vec![0xFF]);
    let mut index = ShardIndex::new(manifest, bounds).expect("the index is valid");
    let before = index.encode();
    let mut transaction = index
        .begin_write(CommitVersion::new(1))
        .expect("the version advances");
    transaction
        .put(&IndexDocument::new(
            b"a".to_vec(),
            vec![DocumentField::new("title", "rust", 0, 0)],
        ))
        .expect("the first write is valid");
    let error = transaction
        .put(&IndexDocument::new(
            b"b".to_vec(),
            vec![DocumentField::new("title", "two words", 0, 0)],
        ))
        .expect_err("the second write exceeds the token budget");
    assert_eq!(error.code(), "search.token_budget_exceeded");
    drop(transaction);
    assert_eq!(index.encode(), before);
    assert_eq!(index.commit_version(), CommitVersion::new(0));

    let mut transaction = index
        .begin_write(CommitVersion::new(1))
        .expect("the version advances");
    transaction
        .put(&IndexDocument::new(
            b"a".to_vec(),
            vec![DocumentField::new("title", "rust", 0, 0)],
        ))
        .expect("the write is valid");
    transaction.commit().expect("the commit succeeds");
    assert_eq!(index.commit_version(), CommitVersion::new(1));
    assert!(
        index
            .documents_at(CommitVersion::new(1))
            .expect("the pin is valid")
            .contains_key(b"a".as_slice())
    );
    assert_ne!(
        index.postings_for("title", "rust"),
        [] as [&oxkv_index::Posting; 0]
    );
}

#[test]
fn the_overhead_budget_is_enforced() {
    let manifest = IndexManifest::new(
        vec![FieldSpec::new("title", TokenizerIdentity::unicode_word())],
        TokenizerIdentity::unicode_word(),
        ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
        8,
        TokenBudget::DEFAULT,
    )
    .expect("the manifest is valid");
    let bounds = PrefixBounds::new(Vec::new(), vec![0xFF]);
    let mut index = ShardIndex::new(manifest, bounds).expect("the index is valid");
    let before = index.encode();
    let mut transaction = index
        .begin_write(CommitVersion::new(1))
        .expect("the version advances");
    transaction
        .put(&IndexDocument::new(
            b"a".to_vec(),
            vec![DocumentField::new("title", "rust", 0, 0)],
        ))
        .expect("the document is valid");
    let error = transaction
        .commit()
        .expect_err("the index exceeds the overhead budget");
    assert_eq!(error.code(), "index.overhead_exceeded");
    assert_eq!(index.encode(), before);
}

#[test]
fn the_token_count_and_length_budgets_are_enforced() {
    let manifest = IndexManifest::new(
        vec![FieldSpec::new("title", TokenizerIdentity::unicode_word())],
        TokenizerIdentity::unicode_word(),
        ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
        100_000,
        TokenBudget::new(1, 3),
    )
    .expect("the manifest is valid");
    let bounds = PrefixBounds::new(Vec::new(), vec![0xFF]);
    let mut index = ShardIndex::new(manifest, bounds).expect("the index is valid");
    let mut transaction = index
        .begin_write(CommitVersion::new(1))
        .expect("the version advances");
    let count_error = transaction
        .put(&IndexDocument::new(
            b"a".to_vec(),
            vec![DocumentField::new("title", "two words", 0, 0)],
        ))
        .expect_err("the token count exceeds the budget");
    assert_eq!(count_error.code(), "search.token_budget_exceeded");
    let length_error = transaction
        .put(&IndexDocument::new(
            b"b".to_vec(),
            vec![DocumentField::new("title", "long", 0, 0)],
        ))
        .expect_err("the token length exceeds the budget");
    assert_eq!(length_error.code(), "search.token_budget_exceeded");
    assert_eq!(index.commit_version(), CommitVersion::new(0));
}

#[test]
fn compaction_preserves_the_current_result_and_drops_old_pins() {
    let mut index = authority_index();
    let request = support::sample_search();
    let before = index
        .search(&request, &oxkv_index::AllScope)
        .expect("the search succeeds");
    let outcome = index.compact().expect("the compaction succeeds");
    assert!(outcome.changed);
    let after = index
        .search(&request, &oxkv_index::AllScope)
        .expect("the search succeeds");
    assert_eq!(before, after);
    let old = SearchRequest::new("rust", CommitVersion::new(0));
    let error = index
        .search(&old, &oxkv_index::AllScope)
        .expect_err("the old pin is not retained");
    assert_eq!(error.code(), "search.pin_expired");
}

#[test]
fn a_pin_below_the_replica_build_version_fails() {
    let index = support::sample_index();
    let request = SearchRequest::new("rust", CommitVersion::new(0));
    let error = index
        .search(&request, &oxkv_index::AllScope)
        .expect_err("the pin is below the build version");
    assert_eq!(error.code(), "search.pin_expired");
}

#[test]
fn the_index_digest_covers_the_segments() {
    let first = support::sample_index();
    let second = support::sample_index();
    assert_eq!(first.digest(), second.digest());
    let mut other = ShardIndex::new(support::sample_manifest(), support::sample_bounds())
        .expect("the index is valid");
    let mut transaction = other
        .begin_write(CommitVersion::new(1))
        .expect("the version advances");
    transaction
        .put(&IndexDocument::new(
            b"a".to_vec(),
            vec![DocumentField::new("title", "different", 1, 0)],
        ))
        .expect("the document is valid");
    transaction.commit().expect("the commit succeeds");
    assert_ne!(first.digest(), other.digest());
}
