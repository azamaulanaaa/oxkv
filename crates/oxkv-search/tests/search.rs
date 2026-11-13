//! Integration tests for snapshot-consistent search.
//!
//! Each test pins one version vector and checks one contract rule.

mod support;

use oxkv_contracts::cursor::{SearchCursor, SortTuple, SortValue};
use oxkv_contracts::version::{CommitVersion, ContentDigest, TokenizerVersion, TokenizerVersions};
use oxkv_search::{
    CursorInvalidReason, PinRetention, QueryBudget, QueryFingerprint, QueryIdentity, QueryOrder,
    ResultMode, RowVersion, SearchError, SearchHit, SearchPage, SearchPin, SearchRequest,
    ShardResponse, TokenBudget, decode_cursor, execute, get_at, get_value_at,
};
use support::{hit, pin, shard, vector};

fn keys(page: &SearchPage) -> Vec<&[u8]> {
    page.hits().iter().map(SearchHit::canonical_key).collect()
}

#[test]
fn deliberate_ties_return_one_stable_order() {
    let search_pin = pin(&[(1, 10), (2, 10), (3, 10)], 7);
    let request = SearchRequest::new(search_pin, [shard(1), shard(2), shard(3)]).with_limit(10);
    let responses = vec![
        ShardResponse::served(
            shard(1),
            CommitVersion::new(10),
            vec![hit(1, b"a", 5, 4), hit(1, b"d", 5, 3)],
        ),
        ShardResponse::served(
            shard(2),
            CommitVersion::new(10),
            vec![hit(2, b"b", 5, 6), hit(2, b"f", 5, 5)],
        ),
        ShardResponse::served(
            shard(3),
            CommitVersion::new(10),
            vec![hit(3, b"c", 5, 8), hit(3, b"e", 5, 7)],
        ),
    ];

    let page = execute(&request, &responses).expect("the pin is readable");
    assert_eq!(
        keys(&page),
        vec![
            b"a".as_slice(),
            b"b".as_slice(),
            b"c".as_slice(),
            b"d".as_slice(),
            b"e".as_slice(),
            b"f".as_slice()
        ]
    );

    let mut shuffled = responses;
    shuffled.reverse();
    let shuffled_page = execute(&request, &shuffled).expect("the pin is readable");
    assert_eq!(page, shuffled_page);
}

#[test]
fn exact_read_with_one_unavailable_shard_returns_error_and_no_data() {
    let search_pin = pin(&[(1, 10), (2, 10)], 7);
    let request = SearchRequest::new(search_pin, [shard(1), shard(2)]);
    let responses = vec![
        ShardResponse::served(shard(1), CommitVersion::new(10), vec![hit(1, b"a", 1, 1)]),
        ShardResponse::unavailable(shard(2)),
    ];

    let error = execute(&request, &responses).expect_err("the exact read must fail");
    assert_eq!(error.code(), "scope.shard_unreachable");

    let expired = vec![
        ShardResponse::served(shard(1), CommitVersion::new(10), vec![hit(1, b"a", 1, 1)]),
        ShardResponse::pin_expired(shard(2), CommitVersion::new(20)),
    ];
    let error = execute(&request, &expired).expect_err("the exact read must fail");
    assert_eq!(error.code(), "search.pin_expired");
    match error {
        SearchError::PinExpired {
            requested,
            oldest_readable,
        } => {
            assert_eq!(requested.get(&shard(2)), Some(CommitVersion::new(10)));
            assert_eq!(oldest_readable.get(&shard(2)), Some(CommitVersion::new(20)));
        }
        other => panic!("unexpected error {other:?}"),
    }
}

#[test]
fn coverage_read_returns_a_descriptor() {
    let search_pin = pin(&[(1, 10), (2, 10)], 7);
    let request =
        SearchRequest::new(search_pin, [shard(1), shard(2)]).with_mode(ResultMode::Coverage);
    let responses = vec![
        ShardResponse::served(shard(1), CommitVersion::new(10), vec![hit(1, b"a", 1, 1)]),
        ShardResponse::unavailable(shard(2)),
    ];

    let page = execute(&request, &responses).expect("coverage mode returns a page");
    assert!(page.is_partial());
    assert_eq!(page.mode(), ResultMode::Coverage);
    assert!(page.next_cursor().is_none());
    assert_eq!(keys(&page), vec![b"a".as_slice()]);

    let coverage = page.coverage().expect("coverage mode has a descriptor");
    assert_eq!(coverage.answered_count(), 1);
    assert_eq!(coverage.missing_shards().to_vec(), vec![shard(2)]);
    assert_eq!(
        coverage.served_versions().get(&shard(1)),
        Some(CommitVersion::new(10))
    );
    assert_eq!(coverage.served_versions().get(&shard(2)), None);
    assert!(coverage.is_partial());

    let complete = vec![
        ShardResponse::served(shard(1), CommitVersion::new(10), vec![hit(1, b"a", 1, 1)]),
        ShardResponse::served(shard(2), CommitVersion::new(10), vec![hit(2, b"b", 1, 2)]),
    ];
    let page = execute(&request, &complete).expect("coverage mode returns a page");
    assert!(page.is_partial());
    assert!(page.coverage().expect("descriptor").is_complete());
}

#[test]
fn coverage_page_does_not_advance_an_exact_cursor() {
    let search_pin = pin(&[(1, 10)], 7);
    let responses = vec![ShardResponse::served(
        shard(1),
        CommitVersion::new(10),
        vec![hit(1, b"a", 2, 1), hit(1, b"b", 1, 2)],
    )];
    let exact_request = SearchRequest::new(search_pin.clone(), [shard(1)]).with_limit(1);
    let first = execute(&exact_request, &responses).expect("the pin is readable");
    let cursor = first.next_cursor().expect("the page has a cursor").clone();

    let coverage_request = SearchRequest::new(search_pin.clone(), [shard(1)])
        .with_mode(ResultMode::Coverage)
        .with_cursor(cursor.clone());
    let error = execute(&coverage_request, &responses).expect_err("coverage rejects a cursor");
    assert_eq!(error.code(), "search.cursor_invalid");
    assert_eq!(
        error,
        SearchError::CursorInvalid {
            reason: CursorInvalidReason::CoverageCursor
        }
    );

    let coverage_page = execute(
        &SearchRequest::new(search_pin.clone(), [shard(1)]).with_mode(ResultMode::Coverage),
        &responses,
    )
    .expect("coverage mode returns a page");
    assert!(coverage_page.next_cursor().is_none());

    let second = execute(
        &SearchRequest::new(search_pin, [shard(1)])
            .with_limit(1)
            .with_cursor(cursor),
        &responses,
    )
    .expect("the pin is readable");
    assert_eq!(keys(&second), vec![b"b".as_slice()]);
    assert!(second.next_cursor().is_none());
}

#[test]
fn changed_query_scope_sort_or_pin_returns_cursor_invalid() {
    let identity = QueryIdentity::new(
        b"q".to_vec(),
        b"scope".to_vec(),
        QueryOrder::Relevance,
        b"score".to_vec(),
    );
    let search_pin = SearchPin::from_identity(vector(&[(1, 10)]), &identity);
    let responses = vec![ShardResponse::served(
        shard(1),
        CommitVersion::new(10),
        vec![hit(1, b"a", 2, 1), hit(1, b"b", 1, 2)],
    )];
    let first = execute(
        &SearchRequest::new(search_pin.clone(), [shard(1)]).with_limit(1),
        &responses,
    )
    .expect("the pin is readable");
    let cursor = first.next_cursor().expect("the page has a cursor").clone();

    let query_changed = QueryIdentity::new(
        b"q2".to_vec(),
        b"scope".to_vec(),
        QueryOrder::Relevance,
        b"score".to_vec(),
    );
    let scope_changed = QueryIdentity::new(
        b"q".to_vec(),
        b"scope2".to_vec(),
        QueryOrder::Relevance,
        b"score".to_vec(),
    );
    let sort_changed = QueryIdentity::new(
        b"q".to_vec(),
        b"scope".to_vec(),
        QueryOrder::Relevance,
        b"title".to_vec(),
    );
    let order_changed = QueryIdentity::new(
        b"q".to_vec(),
        b"scope".to_vec(),
        QueryOrder::Declared,
        b"score".to_vec(),
    );
    for changed in [query_changed, scope_changed, sort_changed, order_changed] {
        assert_ne!(identity.fingerprint(), changed.fingerprint());
        let pin = SearchPin::from_identity(vector(&[(1, 10)]), &changed);
        let error = execute(
            &SearchRequest::new(pin, [shard(1)]).with_cursor(cursor.clone()),
            &responses,
        )
        .expect_err("the changed query must fail");
        assert_eq!(error.code(), "search.cursor_invalid");
    }

    let pin_changed = SearchPin::new(
        vector(&[(1, 11)]),
        QueryFingerprint::from_bytes(*identity.fingerprint().as_bytes()),
    );
    let error = execute(
        &SearchRequest::new(pin_changed, [shard(1)]).with_cursor(cursor.clone()),
        &responses,
    )
    .expect_err("the changed pin must fail");
    assert_eq!(error.code(), "search.cursor_invalid");

    let bad_cursor = SearchCursor::new(
        vector(&[(1, 10)]),
        identity.fingerprint(),
        SortTuple::new(vec![SortValue::Text(String::from("x"))]),
    );
    let error = execute(
        &SearchRequest::new(search_pin, [shard(1)]).with_cursor(bad_cursor),
        &responses,
    )
    .expect_err("the malformed sort tuple must fail");
    assert_eq!(
        error,
        SearchError::CursorInvalid {
            reason: CursorInvalidReason::SortTupleMismatch
        }
    );
}

#[test]
fn concurrent_writes_do_not_repeat_or_skip_a_hit() {
    let search_pin = pin(&[(1, 10)], 7);
    let original = vec![
        hit(1, b"a", 10, 1),
        hit(1, b"b", 10, 2),
        hit(1, b"c", 10, 3),
        hit(1, b"d", 10, 4),
        hit(1, b"e", 10, 5),
    ];
    let page_one = execute(
        &SearchRequest::new(search_pin.clone(), [shard(1)]).with_limit(2),
        &[ShardResponse::served(
            shard(1),
            CommitVersion::new(10),
            original.clone(),
        )],
    )
    .expect("the pin is readable");
    assert_eq!(keys(&page_one), vec![b"a".as_slice(), b"b".as_slice()]);

    let mut after_write = original.clone();
    after_write.push(hit(1, b"f", 10, 11));
    let response = [ShardResponse::served(
        shard(1),
        CommitVersion::new(10),
        after_write,
    )];
    let page_two = execute(
        &SearchRequest::new(search_pin.clone(), [shard(1)])
            .with_limit(2)
            .with_cursor(page_one.next_cursor().expect("cursor").clone()),
        &response,
    )
    .expect("the pin is readable");
    let page_three = execute(
        &SearchRequest::new(search_pin.clone(), [shard(1)])
            .with_limit(2)
            .with_cursor(page_two.next_cursor().expect("cursor").clone()),
        &response,
    )
    .expect("the pin is readable");
    assert!(page_three.next_cursor().is_none());

    let mut seen = Vec::new();
    seen.extend(keys(&page_one));
    seen.extend(keys(&page_two));
    seen.extend(keys(&page_three));
    assert_eq!(
        seen,
        vec![
            b"a".as_slice(),
            b"b".as_slice(),
            b"c".as_slice(),
            b"d".as_slice(),
            b"e".as_slice()
        ]
    );
    assert!(!seen.contains(&b"f".as_slice()));
}

#[test]
fn get_and_search_at_one_pin_agree() {
    let search_pin = pin(&[(1, 10)], 7);
    let rows = vec![
        RowVersion::put(
            b"a".to_vec(),
            shard(1),
            CommitVersion::new(3),
            b"old".to_vec(),
        ),
        RowVersion::put(
            b"a".to_vec(),
            shard(1),
            CommitVersion::new(8),
            b"new".to_vec(),
        ),
        RowVersion::put(
            b"b".to_vec(),
            shard(1),
            CommitVersion::new(5),
            b"b".to_vec(),
        ),
        RowVersion::put(
            b"c".to_vec(),
            shard(1),
            CommitVersion::new(12),
            b"future".to_vec(),
        ),
        RowVersion::put(
            b"d".to_vec(),
            shard(1),
            CommitVersion::new(4),
            b"d".to_vec(),
        ),
        RowVersion::delete(b"d".to_vec(), shard(1), CommitVersion::new(9)),
    ];
    let responses = [ShardResponse::served(
        shard(1),
        CommitVersion::new(10),
        vec![hit(1, b"a", 10, 8), hit(1, b"b", 9, 5), hit(1, b"c", 8, 12)],
    )];

    let page = execute(
        &SearchRequest::new(search_pin.clone(), [shard(1)]),
        &responses,
    )
    .expect("the pin is readable");
    assert_eq!(keys(&page), vec![b"a".as_slice(), b"b".as_slice()]);

    for found in page.hits() {
        let row = get_at(&search_pin, &rows, found.canonical_key())
            .expect("the hit has a visible row at the pin");
        assert_eq!(row.commit_version(), found.commit_version());
        assert_eq!(
            get_value_at(&search_pin, &rows, found.canonical_key()),
            row.value()
        );
    }
    assert_eq!(
        get_value_at(&search_pin, &rows, b"a"),
        Some(b"new".as_slice())
    );
    assert_eq!(get_value_at(&search_pin, &rows, b"d"), None);
    assert_eq!(get_value_at(&search_pin, &rows, b"c"), None);
}

#[test]
fn an_expired_pin_returns_both_versions() {
    let search_pin = pin(&[(1, 10)], 7);
    let request = SearchRequest::new(search_pin, [shard(1)])
        .with_retention(PinRetention::new(vector(&[(1, 20)])));
    let error = execute(&request, &[]).expect_err("the pin expired");
    assert_eq!(error.code(), "search.pin_expired");
    match error {
        SearchError::PinExpired {
            requested,
            oldest_readable,
        } => {
            assert_eq!(requested.get(&shard(1)), Some(CommitVersion::new(10)));
            assert_eq!(oldest_readable.get(&shard(1)), Some(CommitVersion::new(20)));
        }
        other => panic!("unexpected error {other:?}"),
    }
}

#[test]
fn fanout_above_the_bound_fails() {
    let search_pin = pin(&[(1, 10), (2, 10), (3, 10)], 7);
    let request =
        SearchRequest::new(search_pin, [shard(1), shard(2), shard(3)]).with_fanout_limit(2);
    let error = execute(&request, &[]).expect_err("the fan-out is too large");
    assert_eq!(error.code(), "search.fanout_exceeded");
}

#[test]
fn query_budget_exhaustion_returns_a_named_error() {
    let search_pin = pin(&[(1, 10)], 7);
    let responses = [ShardResponse::served(
        shard(1),
        CommitVersion::new(10),
        vec![hit(1, b"a", 3, 1), hit(1, b"b", 2, 2), hit(1, b"c", 1, 3)],
    )];
    let request =
        SearchRequest::new(search_pin.clone(), [shard(1)]).with_budget(QueryBudget::new(2));
    let error = execute(&request, &responses).expect_err("the budget is too small");
    assert_eq!(error.code(), "query.budget_exceeded");
    assert!(error.coverage().is_none());

    let page = execute(
        &SearchRequest::new(search_pin.clone(), [shard(1)]).with_budget(QueryBudget::new(10)),
        &responses,
    )
    .expect("the budget has room");
    assert_eq!(page.budget().used(), 3);
    assert_eq!(page.budget().frontier(&shard(1)), Some(3));

    let coverage = SearchRequest::new(search_pin, [shard(1)])
        .with_mode(ResultMode::Coverage)
        .with_budget(QueryBudget::new(2));
    let error = execute(&coverage, &responses).expect_err("the budget is too small");
    assert_eq!(error.code(), "query.budget_exceeded");
    assert!(error.coverage().is_some());

    let mut budget = QueryBudget::new(10);
    budget.charge(&shard(1), 3).expect("the budget has room");
    assert_eq!(budget.used(), 3);
    assert_eq!(budget.remaining(), 7);
    assert_eq!(budget.frontier(&shard(1)), Some(3));
}

#[test]
fn index_and_tokenizer_mismatches_are_named() {
    let search_pin = pin(&[(1, 10)], 7);
    let responses = [ShardResponse::served(
        shard(1),
        CommitVersion::new(10),
        vec![hit(1, b"a", 1, 1)],
    )];

    let request = SearchRequest::new(search_pin.clone(), [shard(1)])
        .with_index_expectation(shard(1), ContentDigest::from_bytes([1; 32]));
    let response = responses[0]
        .clone()
        .with_index_digest(ContentDigest::from_bytes([2; 32]));
    let error = execute(&request, &[response]).expect_err("the index digest differs");
    assert_eq!(error.code(), "search.index_mismatch");

    let mut expected = TokenizerVersions::new();
    expected.set("title", TokenizerVersion::new(1));
    let request = SearchRequest::new(search_pin.clone(), [shard(1)])
        .with_tokenizer_expectation(expected.clone());
    let mut found = TokenizerVersions::new();
    found.set("title", TokenizerVersion::new(2));
    let response = responses[0].clone().with_tokenizer_versions(found);
    let error = execute(&request, &[response]).expect_err("the tokenizer version differs");
    assert_eq!(error.code(), "search.tokenizer_mismatch");

    let response = responses[0]
        .clone()
        .with_tokenizer_versions(TokenizerVersions::new());
    let error = execute(&request, &[response]).expect_err("the tokenizer field is absent");
    assert_eq!(error.code(), "search.tokenizer_unknown");

    let mut budget = TokenBudget::new(2);
    let error = budget.charge(3).expect_err("the token budget is too small");
    assert_eq!(error.code(), "search.token_budget_exceeded");
}

#[test]
fn cursor_token_round_trips_and_rejects_tampering() {
    let search_pin = pin(&[(1, 10)], 7);
    let responses = [ShardResponse::served(
        shard(1),
        CommitVersion::new(10),
        vec![hit(1, b"a", 2, 1), hit(1, b"b", 1, 2)],
    )];
    let page = execute(
        &SearchRequest::new(search_pin, [shard(1)]).with_limit(1),
        &responses,
    )
    .expect("the pin is readable");
    let token = page.next_cursor_token().expect("the page has a cursor");
    let decoded = decode_cursor(&token).expect("the token decodes");
    assert_eq!(Some(&decoded), page.next_cursor());

    let mut tampered = token;
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    let error = decode_cursor(&tampered).expect_err("the token is tampered");
    assert_eq!(error.code(), "search.cursor_invalid");
}

#[test]
fn declared_order_uses_sort_value_then_key() {
    let search_pin = pin(&[(1, 10)], 7);
    let responses = [ShardResponse::served(
        shard(1),
        CommitVersion::new(10),
        vec![
            hit(1, b"a", 0, 3).with_sort_key(SortValue::Integer(1)),
            hit(1, b"c", 0, 2).with_sort_key(SortValue::Integer(1)),
            hit(1, b"a2", 0, 4).with_sort_key(SortValue::Integer(2)),
            hit(1, b"b", 0, 1).with_sort_key(SortValue::Integer(2)),
        ],
    )];
    let request = SearchRequest::new(search_pin.clone(), [shard(1)])
        .with_order(QueryOrder::Declared)
        .with_limit(2);
    let first = execute(&request, &responses).expect("the pin is readable");
    assert_eq!(keys(&first), vec![b"a".as_slice(), b"c".as_slice()]);

    let second = execute(
        &SearchRequest::new(search_pin, [shard(1)])
            .with_order(QueryOrder::Declared)
            .with_limit(2)
            .with_cursor(first.next_cursor().expect("cursor").clone()),
        &responses,
    )
    .expect("the pin is readable");
    assert_eq!(keys(&second), vec![b"a2".as_slice(), b"b".as_slice()]);
    assert!(second.next_cursor().is_none());
}
