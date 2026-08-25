#![allow(missing_docs)]

//! Benchmarks for the Lucene-style query engine in isolation: the pest-based
//! parser ([`oxkv::parse`]) and the JSON matcher ([`oxkv::eval`]). No backend
//! store is involved — documents live in a plain `Vec<Value>`.
//!
//! Run everything with `cargo bench --bench query_bench`, or filter by stage,
//! e.g. `cargo bench --bench query_bench query_match` or
//! `cargo bench --bench query_bench query_parse/fuzzy`.
//!
//! Workloads:
//! - `query_parse/{kind}` — parse one representative query string per engine
//!   feature (pest tokenization + AST construction)
//! - `query_match/{kind}/{n}docs` — evaluate a pre-parsed query over a corpus
//!   of generated JSON documents (a full linear scan)
//!
//! Corpus sizes are 1,000 and 100,000 documents. Matching is pure per-doc CPU
//! work with no I/O, so 100k already exposes scaling behavior without paying
//! multi-minute sample loops per query kind.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use oxkv::{eval, parse};
use serde_json::{Value, json};

const SMALL: usize = 1_000;
const LARGE: usize = 100_000;

const LANGS: [&str; 4] = ["rust", "go", "python", "javascript"];
const CITIES: [&str; 6] = ["Berlin", "Paris", "Tokyo", "Austin", "Oslo", "Sydney"];
const TOPICS: [&str; 5] = [
    "databases",
    "networking",
    "compilers",
    "graphics",
    "security",
];

/// Generates the deterministic corpus document for index `i`.
///
/// Fields deliberately exercise every matcher path: leaf terms, dotted nested
/// paths, array fan-out (`tags`), numeric and lexicographic ranges, booleans,
/// and free-text bodies.
fn doc(i: usize) -> Value {
    json!({
        "id": format!("doc-{i:06}"),
        "lang": LANGS[i % LANGS.len()],
        "age": 20 + i % 41,
        "active": !i.is_multiple_of(3),
        "score": f64::from(u32::try_from(i % 1_000).expect("fits u32")) / 10.0,
        "tags": [format!("t{}", i % 7), format!("cat{}", i % 5)],
        "address": {
            "city": CITIES[i % CITIES.len()],
            "zip": format!("{:05}", 10_000 + i % 89_999),
        },
        "created": format!("202{}-{:02}-{:02}", i % 4, i % 12 + 1, i % 28 + 1),
        "body": format!("article {i} about {}", TOPICS[i % TOPICS.len()]),
    })
}

fn corpus(n: usize) -> Vec<Value> {
    (0..n).map(doc).collect()
}

/// One representative query per engine feature: `(kind, query string)`.
const QUERIES: [(&str, &str); 14] = [
    ("term", "rust"),
    ("field_term", "lang:rust"),
    ("phrase", r#"id:"doc-000042""#),
    ("wildcard", "id:doc-00*"),
    ("regex", r"id:/doc-\d{3}/"),
    ("fuzzy", "lang:rust~1"),
    ("range_numeric", "age:[30 TO 40]"),
    // Bounds are deliberately not ISO-8601 shaped, so this exercises the
    // classic string-comparison path.
    ("range_lexicographic", "id:[doc-000000 TO doc-049999]"),
    // Calendar-aware comparison over the same field shape as
    // range_lexicographic; run both to quantify date-matching overhead.
    ("range_date_iso", "created:[2021-01-01 TO 2022-12-31]"),
    ("date_term_day", "created:2021-06-15"),
    ("boolean_operators", "+lang:go AND -active:false"),
    ("sub_query", "tags:(t1 OR t2)"),
    ("nested_field", "address.city:Berlin"),
    (
        "complex_combined",
        "+lang:rust age:[25 TO 35] -address.city:Paris tags:cat2",
    ),
];

fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_parse");

    for (kind, query) in QUERIES {
        group.bench_function(kind, |b| {
            b.iter(|| black_box(parse(black_box(query)).expect("valid query")));
        });
    }

    group.finish();
}

fn bench_match(c: &mut Criterion) {
    for &n in &[SMALL, LARGE] {
        let docs = corpus(n);
        let mut group = c.benchmark_group(format!("query_match/{n}docs"));
        group.throughput(Throughput::Elements(
            u64::try_from(n).expect("doc count fits u64"),
        ));

        for (kind, query) in QUERIES {
            let ast = parse(query).expect("valid query");
            group.bench_function(kind, |b| {
                b.iter(|| {
                    let mut hits = 0usize;
                    for d in &docs {
                        hits += usize::from(eval(&ast, d));
                    }
                    black_box(hits)
                });
            });
        }

        group.finish();
    }
}

fn benchmark(c: &mut Criterion) {
    bench_parse(c);
    bench_match(c);
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = benchmark
}
criterion_main!(benches);
