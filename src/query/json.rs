//! JSON document matcher for [`Query`] ASTs.
//!
//! A query is lowered once into a [`CompiledQuery`] (regexes compiled, range
//! bounds parsed) and then evaluated against any [`serde_json::Value`].
//!
//! # Evaluation semantics
//!
//! - **Explicit operators**: `AND` binds tighter than `OR`; the query is
//!   evaluated as an OR-of-AND-clauses. Items without an explicit operator
//!   default to `OR`.
//! - **Occurrence mode** (no explicit operators anywhere in the group):
//!   Lucene-style semantics — all `+required` items must match, no
//!   `-prohibited`/`NOT` item may match, and at least one optional item must
//!   match if any are present. A group of only required/prohibited items
//!   matches when its constraints hold.
//! - **Unscoped terms** match any leaf value anywhere in the document;
//!   **field-scoped terms** resolve dot-separated paths, fanning out across
//!   arrays (`tags:kv` matches `["rust", "kv"]`, `a.b:2` matches nested
//!   objects). A backslash escapes the next character in a field name:
//!   `a\\.b:1` addresses a JSON key literally named `a.b`, while `a.b:1`
//!   descends into `{"a": {"b": 1}}`.
//! - **Terms**: unquoted terms match case-insensitively and support `*`/`?`
//!   wildcards; quoted terms are exact and case-sensitive; `/regex/` terms use
//!   the regex crate; fuzzy `term~N` uses Levenshtein distance; boosts are
//!   accepted by the parser but do not affect boolean matching.

use regex::Regex;
use serde_json::Value;
use strsim::levenshtein;

use super::{BinaryOp, Expression, Prefix, Query, TermExpr};

/// Evaluates a [`Query`] AST against a JSON document.
///
/// Boosts and proximity slop on phrases do not influence the boolean result.
#[must_use]
pub fn eval(query: &Query, doc: &Value) -> bool {
    compile_query(query).matches(doc)
}

struct CompiledQuery {
    items: Vec<CompiledItem>,
}

struct CompiledItem {
    prefix: Option<Prefix>,
    op: Option<BinaryOp>,
    expr: CompiledExpr,
}

enum CompiledExpr {
    Term(CompiledTerm),
    Field {
        path: Vec<String>,
        expr: Box<CompiledExpr>,
    },
    Range {
        start: Bound,
        end: Bound,
        inclusive: bool,
    },
    SubQuery(Box<CompiledQuery>),
}

enum Bound {
    Num(f64),
    Str(String),
}

struct CompiledTerm {
    kind: TermKind,
}

enum TermKind {
    /// Quoted phrase: exact, case-sensitive whole-value equality.
    Exact(String),
    /// Unquoted word: case-insensitive whole-value equality.
    IgnoreCase(String),
    /// Unquoted term with `*`/`?`: anchored, case-insensitive glob.
    Glob(Option<Regex>),
    /// `/regex/` term: user-provided pattern, case-sensitive.
    Pattern(Option<Regex>),
    /// Fuzzy term: Levenshtein distance within `slop`.
    Fuzzy { target: String, slop: usize },
}

fn compile_query(query: &Query) -> CompiledQuery {
    let Query::Group(items) = query;
    CompiledQuery {
        items: items.iter().map(compile_item).collect(),
    }
}

fn compile_item(item: &super::QueryItem) -> CompiledItem {
    CompiledItem {
        prefix: item.prefix.clone(),
        op: item.op.clone(),
        expr: compile_expr(&item.expr),
    }
}

fn compile_expr(expr: &Expression) -> CompiledExpr {
    match expr {
        Expression::Term(term) => CompiledExpr::Term(compile_term(term)),
        Expression::Field { field, expr } => CompiledExpr::Field {
            path: split_field_path(field),
            expr: Box::new(compile_expr(expr)),
        },
        Expression::Range {
            start,
            end,
            inclusive,
            ..
        } => CompiledExpr::Range {
            start: parse_bound(start),
            end: parse_bound(end),
            inclusive: *inclusive,
        },
        Expression::SubQuery { query, .. } => {
            CompiledExpr::SubQuery(Box::new(compile_query(query)))
        }
    }
}

fn compile_term(term: &TermExpr) -> CompiledTerm {
    let kind = if term.is_regex {
        TermKind::Pattern(Regex::new(&term.value).ok())
    } else if let Some(slop) = term.fuzzy_slop {
        TermKind::Fuzzy {
            target: term.value.clone(),
            slop: usize::from(slop),
        }
    } else if !term.is_quoted && (term.value.contains('*') || term.value.contains('?')) {
        TermKind::Glob(glob_regex(&term.value))
    } else if term.is_quoted {
        TermKind::Exact(term.value.clone())
    } else {
        TermKind::IgnoreCase(term.value.to_lowercase())
    };
    CompiledTerm { kind }
}

fn parse_bound(raw: &str) -> Bound {
    raw.parse::<f64>().map_or_else(|_| Bound::Str(raw.to_string()), Bound::Num)
}

/// Lowers a `*`/`?` glob to an anchored, case-insensitive regex.
fn glob_regex(pattern: &str) -> Option<Regex> {
    let mut source = String::from("(?is)^");
    for c in pattern.chars() {
        match c {
            '*' => source.push_str(".*"),
            '?' => source.push('.'),
            other => source.push_str(&regex::escape(&other.to_string())),
        }
    }
    source.push('$');
    Regex::new(&source).ok()
}

/// Splits a raw field name into path segments on unescaped dots and unescapes
/// each segment (`\.` becomes a literal `.`, `\\` a literal backslash).
fn split_field_path(field: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = field.chars();

    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            '.' => segments.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    segments.push(current);
    segments
}

impl CompiledQuery {
    fn matches(&self, doc: &Value) -> bool {
        self.matches_in(doc, &[])
    }

    fn matches_in(&self, doc: &Value, current_path: &[String]) -> bool {
        if self.items.is_empty() {
            return true;
        }

        let has_explicit_ops = self.items.iter().any(|item| item.op.is_some());
        if has_explicit_ops {
            eval_boolean(&self.items, doc, current_path)
        } else {
            eval_occurrences(&self.items, doc, current_path)
        }
    }
}

/// Evaluates a group with explicit operators as an OR-of-AND-clauses.
fn eval_boolean(items: &[CompiledItem], doc: &Value, current_path: &[String]) -> bool {
    let mut clauses: Vec<Vec<&CompiledItem>> = Vec::new();
    let mut clause = Vec::new();

    for item in items {
        clause.push(item);
        // The operator stored on an item connects it to the *next* item, so a
        // new clause starts right after an Or.
        if matches!(item.op, Some(BinaryOp::Or)) {
            clauses.push(std::mem::take(&mut clause));
        }
    }
    clauses.push(clause);

    clauses
        .iter()
        .any(|clause| clause.iter().all(|item| eval_item(item, doc, current_path)))
}

/// Evaluates a group without explicit operators using Lucene occurrence rules.
fn eval_occurrences(items: &[CompiledItem], doc: &Value, current_path: &[String]) -> bool {
    let mut has_optional = false;
    let mut optional_matched = false;

    for item in items {
        let matched = eval_expr(&item.expr, doc, current_path);
        match item.prefix {
            Some(Prefix::Must) => {
                if !matched {
                    return false;
                }
            }
            Some(Prefix::MustNot | Prefix::Not) => {
                if matched {
                    return false;
                }
            }
            None => {
                has_optional = true;
                optional_matched |= matched;
            }
        }
    }

    !has_optional || optional_matched
}

fn eval_item(item: &CompiledItem, doc: &Value, current_path: &[String]) -> bool {
    let matched = eval_expr(&item.expr, doc, current_path);
    match item.prefix {
        Some(Prefix::MustNot | Prefix::Not) => !matched,
        Some(Prefix::Must) | None => matched,
    }
}

fn eval_expr(expr: &CompiledExpr, doc: &Value, current_path: &[String]) -> bool {
    match expr {
        CompiledExpr::Term(term) => {
            match_leaf_value(doc, current_path, &|val| match_term(&term.kind, val))
        }
        CompiledExpr::Field { path, expr } => {
            let full_path: Vec<String> = current_path.iter().chain(path).cloned().collect();
            eval_expr(expr, doc, &full_path)
        }
        CompiledExpr::Range {
            start,
            end,
            inclusive,
        } => match_leaf_value(doc, current_path, &|val| {
            range_matches(start, end, *inclusive, val)
        }),
        CompiledExpr::SubQuery(query) => query.matches_in(doc, current_path),
    }
}

/// Applies a predicate to the leaf values selected by a field path.
fn match_leaf_value<F>(doc: &Value, field_path: &[String], predicate: &F) -> bool
where
    F: Fn(&Value) -> bool,
{
    if field_path.is_empty() {
        scan_anywhere(doc, predicate)
    } else {
        resolve_path(doc, field_path, predicate)
    }
}

/// Resolves a key path, fanning out across arrays at every step.
fn resolve_path<F>(current: &Value, path: &[String], predicate: &F) -> bool
where
    F: Fn(&Value) -> bool,
{
    match path.split_first() {
        None => check_array_or_leaf(current, predicate),
        Some((head, tail)) => match current {
            Value::Object(map) => {
                map.get(head.as_str())
                    .is_some_and(|next| resolve_path(next, tail, predicate))
            }
            Value::Array(arr) => arr.iter().any(|item| resolve_path(item, path, predicate)),
            _ => false,
        },
    }
}

fn check_array_or_leaf<F>(val: &Value, predicate: &F) -> bool
where
    F: Fn(&Value) -> bool,
{
    match val {
        Value::Array(arr) => arr.iter().any(predicate),
        _ => predicate(val),
    }
}

/// Scans every leaf value in the document (used by unscoped terms).
fn scan_anywhere<F>(doc: &Value, predicate: &F) -> bool
where
    F: Fn(&Value) -> bool,
{
    match doc {
        Value::Object(map) => map.values().any(|v| scan_anywhere(v, predicate)),
        Value::Array(arr) => arr.iter().any(|v| scan_anywhere(v, predicate)),
        _ => predicate(doc),
    }
}

fn match_term(kind: &TermKind, val: &Value) -> bool {
    match val {
        Value::String(text) => match_string_kind(kind, text),
        Value::Number(number) => number_matches(kind, number.as_f64(), &number.to_string()),
        Value::Bool(flag) => number_matches(kind, None, &flag.to_string()),
        _ => false,
    }
}

/// Numbers compare numerically when the term is numeric, else as formatted text.
fn number_matches(kind: &TermKind, value: Option<f64>, rendered: &str) -> bool {
    if let Some(value) = value
        && let Some(term_num) = numeric_literal(kind)
    {
        return (value - term_num).abs() < f64::EPSILON;
    }
    match_string_kind(kind, rendered)
}

fn numeric_literal(kind: &TermKind) -> Option<f64> {
    match kind {
        TermKind::Exact(text) | TermKind::IgnoreCase(text) => text.parse::<f64>().ok(),
        _ => None,
    }
}

fn match_string_kind(kind: &TermKind, text: &str) -> bool {
    match kind {
        TermKind::Exact(expected) => text == expected,
        TermKind::IgnoreCase(expected) => text.to_lowercase() == *expected,
        TermKind::Glob(Some(re)) | TermKind::Pattern(Some(re)) => re.is_match(text),
        TermKind::Glob(None) | TermKind::Pattern(None) => false,
        TermKind::Fuzzy { target, slop } => {
            target.chars().count().abs_diff(text.chars().count()) <= *slop
                && levenshtein(target, text) <= *slop
        }
    }
}

fn range_matches(start: &Bound, end: &Bound, inclusive: bool, val: &Value) -> bool {
    if let (Bound::Num(low), Bound::Num(high)) = (start, end) {
        // Numeric bounds also accept string leaves that parse as numbers
        // (e.g. a ZIP code stored as a string).
        let v = val.as_f64().or_else(|| val.as_str().and_then(|s| s.parse::<f64>().ok()));
        if let Some(v) = v {
            return if inclusive {
                v >= *low && v <= *high
            } else {
                v > *low && v < *high
            };
        }
    }

    if let (Bound::Str(low), Bound::Str(high), Some(v)) = (start, end, val.as_str()) {
        return if inclusive {
            v >= low.as_str() && v <= high.as_str()
        } else {
            v > low.as_str() && v < high.as_str()
        };
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use serde_json::json;

    fn eval_q(query: &str, doc: &Value) -> bool {
        let ast = parse(query).expect("query should parse");
        eval(&ast, doc)
    }

    fn sample_doc() -> Value {
        json!({
            "name": "Rust Programming",
            "lang": "rust",
            "age": 15,
            "score": 9.5,
            "active": true,
            "deprecated": false,
            "tags": ["systems", "kv"],
            "address": { "city": "Berlin", "zip": "10115" },
            "people": [
                { "name": "Ada", "age": 36 },
                { "name": "Alan", "age": 41 }
            ]
        })
    }

    // --- Term matching ---

    #[test]
    fn test_unscoped_term_matches_anywhere_case_insensitive() {
        let doc = sample_doc();
        assert!(eval_q("berlin", &doc));
        assert!(eval_q("BERLIN", &doc));
        assert!(eval_q("systems", &doc));
        assert!(!eval_q("paris", &doc));
    }

    #[test]
    fn test_quoted_term_is_exact_and_case_sensitive() {
        let doc = sample_doc();
        assert!(eval_q("\"Rust Programming\"", &doc));
        assert!(!eval_q("\"rust programming\"", &doc));
        assert!(!eval_q("\"Rust\"", &doc));
    }

    #[test]
    fn test_unquoted_term_is_not_substring_match() {
        let doc = sample_doc();
        assert!(!eval_q("program", &doc));
    }

    #[test]
    fn test_wildcard_terms() {
        let doc = sample_doc();
        assert!(eval_q("lang:r*", &doc));
        assert!(eval_q("lang:?ust", &doc));
        assert!(eval_q("sys*", &doc));
        assert!(!eval_q("lang:j*va", &doc));
        assert!(!eval_q("lang:p?t", &doc));
    }

    #[test]
    fn test_regex_terms() {
        let doc = sample_doc();
        assert!(eval_q("lang:/^rust$/", &doc));
        assert!(eval_q("name:/^Rust/", &doc));
        assert!(!eval_q("lang:/^Rust$/", &doc));
    }

    #[test]
    fn test_invalid_regex_never_matches_without_panicking() {
        let doc = sample_doc();
        assert!(!eval_q("lang:/([/", &doc));
    }

    #[test]
    fn test_fuzzy_terms() {
        let doc = sample_doc();
        assert!(eval_q("lang:Rust~1", &doc));
        assert!(eval_q("lang:Rust~2", &doc));
        assert!(!eval_q("lang:Rust~0", &doc));
        assert!(!eval_q("lang:python~2", &doc));
    }

    #[test]
    fn test_boosts_do_not_change_matching() {
        let doc = sample_doc();
        assert!(eval_q("lang:rust^3", &doc));
        assert!(eval_q("(lang:rust)^3.5", &doc));
    }

    #[test]
    fn test_number_terms_compare_numerically() {
        let doc = sample_doc();
        assert!(eval_q("age:15", &doc));
        assert!(eval_q("age:15.0", &doc));
        assert!(!eval_q("age:16", &doc));
        assert!(eval_q("score:9.5", &doc));
    }

    #[test]
    fn test_bool_terms() {
        let doc = sample_doc();
        assert!(eval_q("active:true", &doc));
        assert!(eval_q("active:TRUE", &doc));
        assert!(eval_q("deprecated:false", &doc));
        assert!(!eval_q("active:false", &doc));
    }

    #[test]
    fn test_null_values_never_match_terms() {
        let doc = json!({ "missing": null });
        assert!(!eval_q("null", &doc));
    }

    #[test]
    fn test_empty_group_matches_everything() {
        let doc = sample_doc();
        assert!(eval(&Query::Group(Vec::new()), &doc));
    }

    // --- Ranges ---

    #[test]
    fn test_numeric_inclusive_and_exclusive_ranges() {
        let doc = sample_doc();
        assert!(eval_q("age:[10 TO 20]", &doc));
        assert!(eval_q("age:{14 TO 16}", &doc));
        assert!(!eval_q("age:{15 TO 20}", &doc));
        assert!(!eval_q("age:[16 TO 20]", &doc));
        assert!(eval_q("score:[9 TO 10]", &doc));
    }

    #[test]
    fn test_lexicographic_string_ranges() {
        let doc = sample_doc();
        assert!(eval_q("address.city:[A TO Z]", &doc));
        assert!(eval_q("address.city:{A TO Zurich}", &doc));
        assert!(!eval_q("address.city:{Berlin TO Zurich}", &doc));
        assert!(!eval_q("address.city:{Berlin TO Copenhagen}", &doc));
    }

    #[test]
    fn test_numeric_range_accepts_string_leaves_that_parse() {
        let doc = sample_doc();
        assert!(eval_q("address.zip:[10000 TO 10200]", &doc));
        assert!(!eval_q("address.zip:[10200 TO 10500]", &doc));
    }

    #[test]
    fn test_mixed_bound_types_never_match() {
        let doc = sample_doc();
        assert!(!eval_q("address.city:[10 TO 20]", &doc));
        assert!(!eval_q("age:[a TO z]", &doc));
    }

    #[test]
    fn test_top_level_range_scans_anywhere() {
        let doc = sample_doc();
        assert!(eval_q("[30 TO 45]", &doc));
    }

    // --- Field paths, arrays, scoping ---

    #[test]
    fn test_nested_field_paths() {
        let doc = sample_doc();
        assert!(eval_q("address.city:Berlin", &doc));
        assert!(!eval_q("address.city:Paris", &doc));
        assert!(!eval_q("city:Berlin", &doc));
    }

    #[test]
    fn test_field_names_with_digits_and_spaces() {
        let doc = json!({ "2nd place": { "score": 7 }, "zip2": "x" });
        assert!(eval_q(r"zip2:x", &doc));
        assert!(eval_q(r"\2nd\ place.score:7", &doc));

        // An unescaped space acts as a token separator, so this parses as
        // two separate items instead of one dotted field name.
        let ast = parse(r"2nd place.score:7");
        assert!(ast.is_ok());
    }

    #[test]
    fn test_escaped_dot_addresses_literal_dotted_key() {
        let dotted = json!({ "a.b": 1 });
        assert!(eval_q("a\\.b:1", &dotted));
        assert!(!eval_q("a.b:1", &dotted));

        let nested = json!({ "a": { "b": 1 } });
        assert!(eval_q("a.b:1", &nested));
        assert!(!eval_q("a\\.b:1", &nested));
    }

    #[test]
    fn test_path_through_scalar_leaf_fails_gracefully() {
        let doc = json!({ "name": "Rust" });
        assert!(!eval_q("name.sub:Rust", &doc));

        let arr = json!({ "items": [ { "v": 1 }, { "v": 2 } ] });
        assert!(eval_q("items.v:2", &arr));
    }

    #[test]
    fn test_array_fan_out_on_field_paths() {
        let doc = sample_doc();
        assert!(eval_q("tags:kv", &doc));
        assert!(!eval_q("tags:nosql", &doc));
        assert!(eval_q("people.name:Ada", &doc));
        assert!(eval_q("people.age:[40 TO 45]", &doc));
        assert!(!eval_q("people.age:[50 TO 60]", &doc));
    }

    // --- Boolean operators and precedence ---

    #[test]
    fn test_and_binds_tighter_than_or() {
        let doc = json!({ "x": "1", "y": "2" });
        assert!(eval_q("x:1 OR y:2 AND z:9", &doc));
        assert!(!eval_q("x:9 OR y:2 AND z:9", &doc));
        assert!(eval_q("x:1 AND y:2 OR z:9", &doc));
    }

    #[test]
    fn test_missing_operator_defaults_to_or() {
        let doc = json!({ "x": "1", "y": "2" });
        assert!(eval_q("x:1 y:2", &doc));
        assert!(eval_q("x:1 y:9", &doc));
        assert!(!eval_q("x:9 y:8", &doc));
    }

    #[test]
    fn test_operator_symbol_forms_match_keyword_forms() {
        let doc = json!({ "x": "1", "y": "2" });
        for query in ["x:1 AND y:2", "x:1 && y:2"] {
            assert!(eval_q(query, &doc), "{query}");
        }
        for query in ["x:9 OR y:2", "x:9 || y:2"] {
            assert!(eval_q(query, &doc), "{query}");
        }
    }

    #[test]
    fn test_prefixes_negate_inside_explicit_boolean_groups() {
        let doc = json!({ "x": "1" });
        assert!(eval_q("-z:1 AND x:1", &doc));
        assert!(!eval_q("-x:1 AND z:1", &doc));
    }

    #[test]
    fn test_sub_query_grouping_respects_parens() {
        let doc = json!({ "x": "1", "y": "2", "z": "3" });
        assert!(eval_q("(x:9 OR y:2) AND z:3", &doc));
        assert!(!eval_q("x:9 OR y:2 AND z:9", &doc));
    }

    // --- Occurrence mode (no explicit operators) ---

    #[test]
    fn test_required_prohibited_and_optional_clauses() {
        let doc = sample_doc();

        // required ok + prohibited absent + optional present
        assert!(eval_q("+lang:rust -lang:python tags:kv", &doc));

        // required ok + prohibited absent + optional missing -> no match
        assert!(!eval_q("+lang:rust -lang:python tags:nosql", &doc));

        // a required clause fails
        assert!(!eval_q("+lang:rust +lang:go tags:kv", &doc));

        // a prohibited clause matches -> excluded even with other matches
        assert!(!eval_q("+lang:rust -lang:rust name:Rust", &doc));
    }

    #[test]
    fn test_only_required_and_prohibited_can_match_alone() {
        let doc = sample_doc();
        assert!(eval_q("+lang:rust -lang:python", &doc));
        assert!(!eval_q("+lang:python -lang:go", &doc));
        assert!(eval_q("NOT lang:python", &doc));
        assert!(!eval_q("-lang:rust", &doc));
        assert!(!eval_q("!lang:rust", &doc));
    }

    #[test]
    fn test_field_scoped_sub_queries() {
        let doc = sample_doc();
        assert!(eval_q("tags:(+kv +systems)", &doc));
        assert!(!eval_q("tags:(+kv +nosql)", &doc));

        // Optional clauses inside a scoped sub-query behave like OR
        assert!(eval_q("address:(city:Berlin zip:10115)", &doc));
        assert!(eval_q("address:(city:Paris zip:10115)", &doc));
        assert!(!eval_q("address:(+city:Paris +zip:10115)", &doc));
        assert!(!eval_q("address:(city:Paris city:Lyon)", &doc));
    }
}
