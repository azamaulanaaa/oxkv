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
//! - **Unscoped terms** search every leaf value in the document, Google
//!   style: bare terms match fuzzily against word tokens of text values
//!   (Levenshtein distance <= 2 by default, so `carrs` still finds `cars`),
//!   and quoted phrases match as case-insensitive substrings (`"born on"`
//!   finds `"i am born on 2000"`).
//!   **field-scoped terms** resolve dot-separated paths, fanning out across
//!   arrays (`tags:kv` matches `["rust", "kv"]`, `a.b:2` matches nested
//!   objects). A backslash escapes the next character in a field name:
//!   `a\\.b:1` addresses a JSON key literally named `a.b`, while `a.b:1`
//!   descends into `{"a": {"b": 1}}`.
//! - **Terms**: whole-value comparison applies to field-scoped terms —
//!   unquoted are case-insensitive with `*`/`?` wildcard support; quoted are
//!   exact and case-sensitive; `/regex/` terms use the regex crate; fuzzy
//!   `term~N` uses Levenshtein distance; boosts are accepted by the parser
//!   but do not affect boolean matching.
//! - **Calendar dates**: bounds or plain terms shaped like ISO-8601 dates
//!   (`2025`, `2025-03`, `2025-03-08`, full timestamps with optional `Z` or
//!   `±HH:MM`) compare as UTC calendar intervals instead of text. Granularity
//!   comes from the literal's precision, so `created:[2025 TO 2026]` matches
//!   every timestamp inside those years and `created:2025-03-08` matches any
//!   instant on that day. Naive datetimes (no offset) are read as UTC.
//!   Non-date-shaped strings keep the classic comparison behavior.

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
    /// Calendar-aware interval comparison for ISO-8601-shaped operands.
    Date(DateInterval),
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
    /// Unscoped quoted phrase: case-insensitive containment anywhere in the
    /// value.
    Contains(String),
    /// Unscoped bare term: any word token of the value within Levenshtein
    /// `slop` of the lowercased target.
    FuzzyToken { target: String, slop: usize },
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
        expr: compile_expr(&item.expr, false),
    }
}

fn compile_expr(expr: &Expression, scoped: bool) -> CompiledExpr {
    match expr {
        Expression::Term(term) => {
            if let Some(interval) = date_term(term) {
                CompiledExpr::Date(interval)
            } else if scoped {
                CompiledExpr::Term(compile_term(term))
            } else {
                CompiledExpr::Term(compile_unscoped_term(term))
            }
        }
        Expression::Field { field, expr } => CompiledExpr::Field {
            path: split_field_path(field),
            expr: Box::new(compile_expr(expr, true)),
        },
        Expression::Range {
            start,
            end,
            inclusive,
            ..
        } => {
            if let Some(interval) = date_interval(start, end, *inclusive) {
                CompiledExpr::Date(interval)
            } else {
                CompiledExpr::Range {
                    start: parse_bound(start),
                    end: parse_bound(end),
                    inclusive: *inclusive,
                }
            }
        }
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
    raw.parse::<f64>()
        .map_or_else(|_| Bound::Str(raw.to_string()), Bound::Num)
}

/// Compiles terms appearing outside any field scope. These power the
/// "search that just works" layer: bare terms become token-level fuzzy
/// matches with default slop 2, and quoted phrases become case-insensitive
/// substring predicates. Regex and wildcard kinds keep their whole-value
/// behavior; date-shaped literals are routed to calendar intervals before
/// this runs.
fn compile_unscoped_term(term: &TermExpr) -> CompiledTerm {
    let kind = if term.is_regex {
        TermKind::Pattern(Regex::new(&term.value).ok())
    } else if term.is_quoted {
        TermKind::Contains(term.value.to_lowercase())
    } else if term.value.contains('*') || term.value.contains('?') {
        TermKind::Glob(glob_regex(&term.value))
    } else {
        TermKind::FuzzyToken {
            target: term.value.to_lowercase(),
            slop: usize::from(term.fuzzy_slop.unwrap_or(2)),
        }
    };
    CompiledTerm { kind }
}

/// Splits text into lowercase alphanumeric word tokens for unscoped matching.
fn tokens_ci(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
}

/// Granularity carried by an ISO-8601 date literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatePrecision {
    Year,
    Month,
    Day,
    Minute,
    Second,
}

impl DatePrecision {
    /// Whether the literal identifies at least a whole calendar month.
    /// Year-only literals stay on the text path so numeric-looking leaves
    /// keep their existing textual/numeric matching behavior.
    fn at_least_month(self) -> bool {
        !matches!(self, Self::Year)
    }
}

/// A successfully parsed ISO-8601 date literal.
struct DateLit {
    /// UTC seconds at the literal's start instant.
    ts: i64,
    precision: DatePrecision,
    year: i32,
    month: u32,
}

const SECS_PER_DAY: i64 = 86_400;

fn digit(b: &[u8], i: usize) -> Option<u32> {
    match b.get(i) {
        Some(&c) if c.is_ascii_digit() => Some(u32::from(c - b'0')),
        _ => None,
    }
}

fn digits(b: &[u8], i: usize, n: usize) -> Option<u32> {
    (0..n).try_fold(0u32, |acc, k| Some(acc * 10 + digit(b, i + k)?))
}

fn expect(b: &[u8], i: usize, c: u8) -> Option<()> {
    (b.get(i) == Some(&c)).then_some(())
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(if m <= 2 { y - 1 } else { y });
    let m = i64::from(m);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

impl DateLit {
    /// UTC seconds at the start of the unit following this literal.
    fn next_start(&self) -> i64 {
        match self.precision {
            DatePrecision::Year => days_from_civil(self.year + 1, 1, 1) * SECS_PER_DAY,
            DatePrecision::Month => {
                let (y, m) = if self.month == 12 {
                    (self.year + 1, 1)
                } else {
                    (self.year, self.month + 1)
                };
                days_from_civil(y, m, 1) * SECS_PER_DAY
            }
            DatePrecision::Day => self.ts + SECS_PER_DAY,
            DatePrecision::Minute => self.ts + 60,
            DatePrecision::Second => self.ts + 1,
        }
    }
}

/// Parses the accepted ISO-8601 shapes: `YYYY`, `YYYY-MM`, `YYYY-MM-DD`,
/// and `YYYY-MM-DD[T| ]HH:MM[:SS[.fff]]` with optional `Z` or `±HH:MM`.
/// Naive datetimes are read as UTC.
///
/// Day-of-month is validated loosely (`01-31`); impossible dates such as
/// February 30th roll forward harmlessly inside the interval arithmetic.
/// Returns `None` for anything else — including plain numbers — so callers
/// keep ordinary string/number comparison untouched when this fails.
fn parse_date_literal(raw: &str) -> Option<DateLit> {
    let b = raw.as_bytes();
    let (year, month, day) = parse_ymd(b)?;
    let mut lit = DateLit {
        ts: days_from_civil(year, month, day) * SECS_PER_DAY,
        precision: match b.len() {
            0..=4 => DatePrecision::Year,
            5..=7 => DatePrecision::Month,
            _ => DatePrecision::Day,
        },
        year,
        month,
    };
    if b.len() > 10 {
        let (secs, precision) = parse_time_zone(b)?;
        lit.ts += secs;
        lit.precision = precision;
    }
    Some(lit)
}

/// Parses the date part: `YYYY`, `YYYY-MM`, or `YYYY-MM-DD`. Longer inputs
/// must still be a valid date prefix. Missing parts default to the first of
/// the unit (`2025-03` means March 1st).
fn parse_ymd(b: &[u8]) -> Option<(i32, u32, u32)> {
    if b.len() < 4 {
        return None;
    }
    let year = i32::try_from(digits(b, 0, 4)?).ok()?;
    if b.len() == 4 {
        return Some((year, 1, 1));
    }

    expect(b, 4, b'-')?;
    let month = digits(b, 5, 2)?;
    if !(1..=12).contains(&month) {
        return None;
    }
    if b.len() == 7 {
        return Some((year, month, 1));
    }

    expect(b, 7, b'-')?;
    let day = digits(b, 8, 2)?;
    if !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

/// Parses the time-and-zone suffix starting at byte 10
/// (`T|t|<space>HH:MM[:SS[.fff]]` with optional trailing `Z` or `±HH:MM`).
/// Returns the seconds to add to local midnight and the resulting precision.
fn parse_time_zone(b: &[u8]) -> Option<(i64, DatePrecision)> {
    match b[10] {
        b'T' | b't' | b' ' => {}
        _ => return None,
    }
    let hour = digits(b, 11, 2)?;
    if hour > 23 || b.get(13) != Some(&b':') {
        return None;
    }
    let minute = digits(b, 14, 2)?;
    if minute > 59 {
        return None;
    }
    let mut secs = i64::from(hour) * 3600 + i64::from(minute) * 60;
    let mut precision = DatePrecision::Minute;

    let mut i = 16;
    if b.get(i) == Some(&b':') {
        let second = digits(b, 17, 2)?;
        if second > 59 {
            return None;
        }
        secs += i64::from(second);
        precision = DatePrecision::Second;
        i = 19;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }

    match b.get(i) {
        None => {}
        Some(&z) if (z == b'Z' || z == b'z') && i + 1 == b.len() => {}
        Some(&sign) if sign == b'+' || sign == b'-' => {
            let oh = digits(b, i + 1, 2)?;
            if oh > 23 || b.get(i + 3) != Some(&b':') {
                return None;
            }
            let om = digits(b, i + 4, 2)?;
            if om > 59 || i + 6 != b.len() {
                return None;
            }
            let mag = i64::from(oh) * 3600 + i64::from(om) * 60;
            secs -= if sign == b'-' { -mag } else { mag };
        }
        _ => return None,
    }

    Some((secs, precision))
}

/// Half-open UTC-second interval `[start, end)` derived from a pair of
/// ISO-8601-shaped bounds.
struct DateInterval {
    start: i64,
    end: i64,
}

/// Builds a calendar interval from range bounds when both parse as ISO-8601
/// dates; `None` keeps the classic string/number comparison path.
fn date_interval(start: &str, end: &str, inclusive: bool) -> Option<DateInterval> {
    let lo = parse_date_literal(start)?;
    let hi = parse_date_literal(end)?;
    Some(DateInterval {
        start: if inclusive { lo.ts } else { lo.next_start() },
        end: if inclusive { hi.next_start() } else { hi.ts },
    })
}

/// Routes plain (unquoted, non-regex, non-fuzzy) terms whose value carries at
/// least month precision to calendar equality over that period.
fn date_term(term: &TermExpr) -> Option<DateInterval> {
    if term.is_quoted || term.is_regex || term.fuzzy_slop.is_some() {
        return None;
    }
    let lit = parse_date_literal(&term.value)?;
    lit.precision.at_least_month().then_some(DateInterval {
        start: lit.ts,
        end: lit.next_start(),
    })
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
        CompiledExpr::Date(interval) => {
            match_leaf_value(doc, current_path, &|val| date_leaf_matches(val, interval))
        }
        CompiledExpr::SubQuery(query) => query.matches_in(doc, current_path),
    }
}

/// A leaf matches a calendar interval when it parses as an ISO-8601 instant
/// falling inside it. Non-date leaves (including plain numbers) never match.
fn date_leaf_matches(val: &Value, interval: &DateInterval) -> bool {
    match val {
        Value::String(text) => parse_date_literal(text)
            .is_some_and(|lit| lit.ts >= interval.start && lit.ts < interval.end),
        _ => false,
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
            Value::Object(map) => map
                .get(head.as_str())
                .is_some_and(|next| resolve_path(next, tail, predicate)),
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
        TermKind::Contains(needle) => text.to_lowercase().contains(needle),
        TermKind::FuzzyToken { target, slop } => tokens_ci(text)
            .any(|token| levenshtein(target, token) <= *slop),
    }
}

fn range_matches(start: &Bound, end: &Bound, inclusive: bool, val: &Value) -> bool {
    if let (Bound::Num(low), Bound::Num(high)) = (start, end) {
        // Numeric bounds also accept string leaves that parse as numbers
        // (e.g. a ZIP code stored as a string).
        let v = val
            .as_f64()
            .or_else(|| val.as_str().and_then(|s| s.parse::<f64>().ok()));
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
    fn test_scoped_quoted_terms_remain_exact_and_case_sensitive() {
        let doc = sample_doc();
        assert!(eval_q("name:\"Rust Programming\"", &doc));
        assert!(!eval_q("name:\"rust programming\"", &doc));
        assert!(!eval_q("name:\"Rust\"", &doc));
    }

    #[test]
    fn test_unscoped_quoted_phrases_match_substrings() {
        let doc = json!({ "bio": "I ride my motorbike daily" });
        assert!(eval_q("\"motorbike\"", &doc));
        assert!(eval_q("\"ride my motorbike\"", &doc));
        assert!(eval_q("\"MOTORBIKE\"", &doc)); // containment is case-insensitive
        assert!(!eval_q("\"motorbike daily!\"", &doc));
    }

    #[test]
    fn test_unscoped_bare_terms_fuzzy_match_tokens_in_prose() {
        let doc = json!({ "bio": "i am born on 2000 somewhere" });
        assert!(eval_q("born", &doc));
        assert!(eval_q("boren", &doc)); // typo within default slop 2
        assert!(eval_q("2000", &doc));
        assert!(eval_q("2055", &doc)); // two substitutions away
        assert!(!eval_q("xyzzyq", &doc));
    }

    #[test]
    fn test_field_scoped_terms_keep_whole_value_semantics() {
        let doc = json!({ "bio": "i am born on 2000" });
        assert!(!eval_q("bio:born", &doc));
        assert!(eval_q("bio:\"i am born on 2000\"", &doc));
        assert!(eval_q("bio:*born*", &doc)); // wildcard idiom still works
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

    // --- Calendar dates (ISO-8601 shaped) ---

    #[test]
    fn test_date_ranges_use_calendar_intervals() {
        let doc = json!({ "at": "2025-03-08T09:30:00Z" });
        assert!(eval_q("at:2025-03-08", &doc));
        assert!(eval_q("at:2025-03", &doc));
        assert!(eval_q("at:[2025-01-01 TO 2025-12-31]", &doc));
        assert!(eval_q("at:[2025-03 TO 2025-03]", &doc));
        assert!(!eval_q("at:2025-03-09", &doc));
        assert!(!eval_q("at:2025-04", &doc));
        assert!(eval_q("at:{2025-03-01 TO 2025-03-09}", &doc));
        assert!(!eval_q("at:{2025-03-08 TO 2025-03-10}", &doc));
    }

    #[test]
    fn test_date_offsets_normalize_to_utc() {
        let doc = json!({ "at": "2025-03-08T00:30:00+02:00" }); // 2025-03-07T22:30Z
        assert!(eval_q("at:2025-03-07", &doc));
        assert!(!eval_q("at:2025-03-08", &doc));
    }

    #[test]
    fn test_naive_datetimes_read_as_utc() {
        let doc = json!({ "at": "2025-03-08 22:30" });
        assert!(eval_q("at:2025-03-08", &doc));
        assert!(!eval_q("at:2025-03-09", &doc));
    }

    #[test]
    fn test_non_date_strings_keep_lexicographic_ranges() {
        let doc = sample_doc();
        assert!(eval_q("address.city:[A TO Zurich]", &doc));
        assert!(!eval_q("name:[A TO B]", &doc));
    }

    #[test]
    fn test_malformed_dates_fall_back_to_text_comparison() {
        let doc = json!({ "v": "2025-3-8" });
        assert!(eval_q("v:\"2025-3-8\"", &doc));
        assert!(!eval_q("v:[2025-01-01 TO 2025-12-31]", &doc));
    }

    #[test]
    fn test_year_only_terms_keep_text_matching() {
        let doc = json!({ "v": "2025", "w": 2025 });
        assert!(eval_q("v:2025", &doc));
        assert!(eval_q("w:2025", &doc));
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
