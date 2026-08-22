use pest::iterators::{Pair, Pairs};
use pest::Parser;

mod parser;
pub use parser::{LuceneParser, Rule};

/// A parsed Lucene-style query.
#[derive(Debug, PartialEq, Clone)]
pub enum Query {
    /// A group of query items combined with optional boolean operators.
    Group(Vec<QueryItem>),
}

/// A single query expression with an optional prefix and trailing operator.
#[derive(Debug, PartialEq, Clone)]
pub struct QueryItem {
    /// Optional occurrence prefix (`+`, `-`, or `NOT`).
    pub prefix: Option<Prefix>,
    /// The expression itself.
    pub expr: Expression,
    /// Optional boolean operator connecting this item to the next one.
    pub op: Option<BinaryOp>,
}

/// Occurrence prefix modifiers for a query item.
#[derive(Debug, PartialEq, Clone)]
pub enum Prefix {
    /// Required term (`+`).
    Must,
    /// Prohibited term (`-`).
    MustNot,
    /// Negated term (`NOT` or `!`).
    Not,
}

/// Boolean operators between query items.
#[derive(Debug, PartialEq, Clone)]
pub enum BinaryOp {
    /// Conjunction (`AND` or `&&`).
    And,
    /// Disjunction (`OR` or `||`).
    Or,
}

/// An individual query expression.
#[derive(Debug, PartialEq, Clone)]
pub enum Expression {
    /// A bare term, optionally fuzzy or boosted.
    Term(TermExpr),
    /// A field-scoped expression (`field:expr`).
    Field {
        /// The field name before the colon.
        field: String,
        /// The scoped sub-expression.
        expr: Box<Expression>,
    },
    /// An inclusive `[a TO b]` or exclusive `{a TO b}` range.
    Range {
        /// Range lower bound.
        start: String,
        /// Range upper bound.
        end: String,
        /// Whether the bounds are inclusive (`[`/`]`) or exclusive (`{`/`}`).
        inclusive: bool,
        /// Optional boost factor applied to the range.
        boost: Option<f32>,
    },
    /// A parenthesized sub-query, optionally boosted.
    SubQuery {
        /// The nested query.
        query: Query,
        /// Optional boost factor applied to the whole sub-query.
        boost: Option<f32>,
    },
}

/// A term expression with optional phrase, regex, fuzzy, and boost modifiers.
#[derive(Debug, PartialEq, Clone)]
pub struct TermExpr {
    /// The raw term text (unescaped content for quoted/regex terms).
    pub value: String,
    /// Whether the term was a quoted phrase.
    pub is_quoted: bool,
    /// Whether the term was a `/regex/` expression.
    pub is_regex: bool,
    /// Fuzzy/proximity slop from a trailing `~N` (defaults to 2 for bare `~`).
    pub fuzzy_slop: Option<u8>,
    /// Boost factor from a trailing `^N`.
    pub boost: Option<f32>,
}

/// Alias for fallible AST construction helpers.
type AstResult<T> = std::result::Result<T, String>;

/// Parses a Lucene-style query string into a [`Query`] AST.
///
/// # Errors
/// Returns an error if the input is not a syntactically valid query.
pub fn parse(input: &str) -> AstResult<Query> {
    let pairs = LuceneParser::parse(Rule::main, input).map_err(|e| e.to_string())?;
    build_ast(pairs)
}

fn invalid(rule: Rule) -> String {
    format!("malformed parse tree: expected a `{rule:?}` node")
}

/// Extracts the numeric factor from a `boost` pair (`^2`, `^1.5`).
fn boost_of(pair: &Pair<'_, Rule>) -> Option<f32> {
    pair.as_str().trim_start_matches('^').parse::<f32>().ok()
}

/// Unescapes backslash sequences (`\\+` becomes `+`) in captured term text.
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(escaped) => out.push(escaped),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Builds a [`Query`] AST from parsed pest pairs.
///
/// # Errors
/// Returns an error if the pair tree does not match the expected grammar shape.
pub fn build_ast(pairs: Pairs<Rule>) -> AstResult<Query> {
    for pair in pairs {
        if pair.as_rule() == Rule::query {
            return parse_query(pair);
        }
    }
    Ok(Query::Group(Vec::new()))
}

fn parse_query(pair: Pair<Rule>) -> AstResult<Query> {
    let mut items = Vec::new();
    let mut current_prefix = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::not_op => current_prefix = Some(Prefix::Not),
            Rule::single_query => {
                let (prefix, expr) = parse_single_query(inner)?;
                items.push(QueryItem {
                    prefix: current_prefix.or(prefix),
                    expr,
                    op: None,
                });
                current_prefix = None;
            }
            Rule::and_op => {
                if let Some(last) = items.last_mut() {
                    last.op = Some(BinaryOp::And);
                }
            }
            Rule::or_op => {
                if let Some(last) = items.last_mut() {
                    last.op = Some(BinaryOp::Or);
                }
            }
            _ => {}
        }
    }

    ensure_no_dangling_operator(&items)?;
    Ok(Query::Group(items))
}

fn ensure_no_dangling_operator(items: &[QueryItem]) -> AstResult<()> {
    if let Some(op) = items.last().and_then(|last| last.op.as_ref()) {
        return Err(format!(
            "dangling boolean operator `{op:?}` with no following expression"
        ));
    }
    Ok(())
}

fn parse_single_query(pair: Pair<Rule>) -> AstResult<(Option<Prefix>, Expression)> {
    let nodes: Vec<_> = pair.into_inner().collect();
    let mut prefix = None;
    let mut expr = None;

    for (idx, node) in nodes.iter().enumerate() {
        match node.as_rule() {
            Rule::must => prefix = Some(Prefix::Must),
            Rule::must_not => prefix = Some(Prefix::MustNot),
            Rule::field_query => expr = Some(parse_field_query(node)?),
            Rule::modified_term => expr = Some(Expression::Term(parse_modified_term(node))),
            Rule::sub_query => expr = Some(parse_sub_query(node)?),
            Rule::inc_range => {
                expr = Some(range_parts(node, sibling_boost(&nodes, idx + 1), true));
            }
            Rule::exc_range => {
                expr = Some(range_parts(node, sibling_boost(&nodes, idx + 1), false));
            }
            _ => {}
        }
    }

    let expr = expr.ok_or_else(|| invalid(Rule::single_query))?;
    Ok((prefix, expr))
}

fn parse_field_query(pair: &Pair<'_, Rule>) -> AstResult<Expression> {
    let nodes: Vec<_> = pair.clone().into_inner().collect();
    let field = nodes
        .first()
        .filter(|p| p.as_rule() == Rule::field_name)
        .ok_or_else(|| invalid(Rule::field_name))?
        .as_str()
        .to_string();
    let val_pair = nodes.get(1).ok_or_else(|| invalid(Rule::field_query))?;

    let target_expr = match val_pair.as_rule() {
        Rule::modified_term => Expression::Term(parse_modified_term(val_pair)),
        Rule::inc_range => range_parts(val_pair, sibling_boost(&nodes, 2), true),
        Rule::exc_range => range_parts(val_pair, sibling_boost(&nodes, 2), false),
        Rule::sub_query => parse_sub_query(val_pair)?,
        _ => return Err(invalid(Rule::field_query)),
    };

    Ok(Expression::Field {
        field,
        expr: Box::new(target_expr),
    })
}

/// Reads a `boost` sibling that follows a silent `range_query` match.
fn sibling_boost(nodes: &[Pair<'_, Rule>], idx: usize) -> Option<f32> {
    nodes
        .get(idx)
        .filter(|p| p.as_rule() == Rule::boost)
        .and_then(boost_of)
}

fn range_parts(pair: &Pair<'_, Rule>, boost: Option<f32>, inclusive: bool) -> Expression {
    let mut inner = pair.clone().into_inner();
    let start = inner.next().map_or_else(String::new, |p| unescape(p.as_str()));
    let end = inner.next().map_or_else(String::new, |p| unescape(p.as_str()));
    Expression::Range {
        start,
        end,
        inclusive,
        boost,
    }
}

fn parse_modified_term(pair: &Pair<'_, Rule>) -> TermExpr {
    let mut value = String::new();
    let mut is_quoted = false;
    let mut is_regex = false;
    let mut fuzzy_slop = None;
    let mut boost = None;

    for inner in pair.clone().into_inner() {
        match inner.as_rule() {
            Rule::unquoted_term => value = unescape(inner.as_str()),
            Rule::quoted_term => {
                is_quoted = true;
                value = inner
                    .into_inner()
                    .next()
                    .map_or_else(String::new, |p| unescape(p.as_str()));
            }
            Rule::regex_term => {
                is_regex = true;
                value = inner
                    .into_inner()
                    .next()
                    .map_or_else(String::new, |p| unescape(p.as_str()));
            }
            Rule::fuzzy_slop => {
                let slop_str = inner.as_str().trim_start_matches('~');
                fuzzy_slop = if slop_str.is_empty() {
                    Some(2) // Default edit distance per spec
                } else {
                    slop_str.parse::<u8>().ok()
                };
            }
            Rule::boost => boost = boost_of(&inner),
            _ => {}
        }
    }

    TermExpr {
        value,
        is_quoted,
        is_regex,
        fuzzy_slop,
        boost,
    }
}

fn parse_sub_query(pair: &Pair<'_, Rule>) -> AstResult<Expression> {
    let nodes: Vec<_> = pair.clone().into_inner().collect();
    let query_pair = nodes.first().ok_or_else(|| invalid(Rule::query))?;
    let boost = nodes
        .get(1)
        .filter(|p| p.as_rule() == Rule::boost)
        .and_then(boost_of);

    Ok(Expression::SubQuery {
        query: parse_query(query_pair.clone())?,
        boost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pest::Parser;

    fn parse_to_ast(input: &str) -> Query {
        parse(input).expect("Failed to parse query")
    }

    fn parse_err(input: &str) {
        assert!(
            LuceneParser::parse(Rule::main, input).is_err(),
            "expected parse error for `{input}`"
        );
    }

    fn term(
        value: &str,
        is_quoted: bool,
        is_regex: bool,
        fuzzy_slop: Option<u8>,
        boost: Option<f32>,
    ) -> Expression {
        Expression::Term(TermExpr {
            value: value.to_string(),
            is_quoted,
            is_regex,
            fuzzy_slop,
            boost,
        })
    }

    fn plain_term(value: &str) -> Expression {
        term(value, false, false, None, None)
    }

    fn group(items: Vec<(&Option<Prefix>, Expression, &Option<BinaryOp>)>) -> Query {
        Query::Group(
            items
                .into_iter()
                .map(|(prefix, expr, op)| QueryItem {
                    prefix: prefix.clone(),
                    expr,
                    op: op.clone(),
                })
                .collect(),
        )
    }

    #[test]
    fn test_simple_term() {
        let ast = parse_to_ast("rust");
        let expected = group(vec![(&None, plain_term("rust"), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_wildcard_terms() {
        let ast = parse_to_ast("rus* j?va");
        let expected = group(vec![
            (&None, plain_term("rus*"), &None),
            (&None, plain_term("j?va"), &None),
        ]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_invalid_error_message() {
        assert_eq!(
            invalid(Rule::field_query),
            "malformed parse tree: expected a `field_query` node"
        );
    }

    #[test]
    fn test_unescape_edge_cases() {
        assert_eq!(unescape(""), "");
        assert_eq!(unescape("plain"), "plain");
        assert_eq!(unescape(r"a\+b"), "a+b");
        assert_eq!(unescape(r"\\"), "\\");
        assert_eq!(unescape(r"trailing\"), "trailing\\");
    }

    #[test]
    fn test_build_ast_without_query_pair_returns_empty_group() {
        let pairs = LuceneParser::parse(Rule::single_query, "rust").expect("parse single query");
        assert_eq!(build_ast(pairs), Ok(Query::Group(Vec::new())));
    }

    #[test]
    fn test_escaped_special_chars() {
        let ast = parse_to_ast(r"hello\+\(world\)");
        let expected = group(vec![(&None, plain_term("hello+(world)"), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_escaped_colon_is_not_a_field() {
        let ast = parse_to_ast(r"a\:b");
        let expected = group(vec![(&None, plain_term("a:b"), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_quoted_phrase() {
        let ast = parse_to_ast("\"exact phrase\"");
        let expected = group(vec![(&None, term("exact phrase", true, false, None, None), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_quoted_phrase_with_escaped_quote() {
        let ast = parse_to_ast(r#""say \"hi\"""#);
        let expected = group(vec![(&None, term("say \"hi\"", true, false, None, None), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_regex_term() {
        let ast = parse_to_ast("/rus.*/");
        let expected = group(vec![(&None, term("rus.*", false, true, None, None), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_regex_with_escaped_slash() {
        let ast = parse_to_ast("/a\\/b/");
        let expected = group(vec![(&None, term("a/b", false, true, None, None), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_fuzzy_default_edit_distance() {
        let ast = parse_to_ast("roam~");
        let expected = group(vec![(&None, term("roam", false, false, Some(2), None), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_fuzzy_explicit_edit_distance() {
        let ast = parse_to_ast("roam~1");
        let expected = group(vec![(&None, term("roam", false, false, Some(1), None), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_fuzzy_distance_overflowing_u8_is_dropped() {
        let ast = parse_to_ast("roam~999");
        let expected = group(vec![(&None, term("roam", false, false, None, None), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_boost_integer_and_float() {
        let ast = parse_to_ast("rust^2 go^2.5");
        let expected = group(vec![
            (&None, term("rust", false, false, None, Some(2.0)), &None),
            (&None, term("go", false, false, None, Some(2.5)), &None),
        ]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_quoted_phrase_with_proximity() {
        let ast = parse_to_ast(r#""rust go"~4"#);
        let expected = group(vec![(&None, term("rust go", true, false, Some(4), None), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_fuzzy_plus_boost_combined() {
        let ast = parse_to_ast("roam~1^3");
        let expected = group(vec![(
            &None,
            term("roam", false, false, Some(1), Some(3.0)),
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_must_prefix() {
        let ast = parse_to_ast("+rust");
        let expected = group(vec![(&Some(Prefix::Must), plain_term("rust"), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_must_not_prefix() {
        let ast = parse_to_ast("-draft");
        let expected = group(vec![(&Some(Prefix::MustNot), plain_term("draft"), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_not_keyword_prefix() {
        let ast = parse_to_ast("NOT draft");
        let expected = group(vec![(&Some(Prefix::Not), plain_term("draft"), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_not_bang_prefix() {
        let ast = parse_to_ast("!draft");
        let expected = group(vec![(&Some(Prefix::Not), plain_term("draft"), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_outer_not_wins_over_inner_prefix() {
        let ast = parse_to_ast("NOT +draft");
        let expected = group(vec![(&Some(Prefix::Not), plain_term("draft"), &None)]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_prefixes_on_consecutive_items() {
        let ast = parse_to_ast("+a -b");
        let expected = group(vec![
            (&Some(Prefix::Must), plain_term("a"), &None),
            (&Some(Prefix::MustNot), plain_term("b"), &None),
        ]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_and_operator_forms() {
        for q in ["a AND b", "a && b"] {
            let ast = parse_to_ast(q);
            let expected = group(vec![
                (&None, plain_term("a"), &Some(BinaryOp::And)),
                (&None, plain_term("b"), &None),
            ]);
            assert_eq!(ast, expected, "query: {q}");
        }
    }

    #[test]
    fn test_or_operator_forms() {
        for q in ["a OR b", "a || b"] {
            let ast = parse_to_ast(q);
            let expected = group(vec![
                (&None, plain_term("a"), &Some(BinaryOp::Or)),
                (&None, plain_term("b"), &None),
            ]);
            assert_eq!(ast, expected, "query: {q}");
        }
    }

    #[test]
    fn test_implicit_operator_between_terms() {
        let ast = parse_to_ast("a b c");
        let expected = group(vec![
            (&None, plain_term("a"), &None),
            (&None, plain_term("b"), &None),
            (&None, plain_term("c"), &None),
        ]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_mixed_operator_chain() {
        let ast = parse_to_ast("a AND b OR c");
        let expected = group(vec![
            (&None, plain_term("a"), &Some(BinaryOp::And)),
            (&None, plain_term("b"), &Some(BinaryOp::Or)),
            (&None, plain_term("c"), &None),
        ]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_lowercase_keywords_are_plain_terms() {
        let ast = parse_to_ast("and or not");
        let expected = group(vec![
            (&None, plain_term("and"), &None),
            (&None, plain_term("or"), &None),
            (&None, plain_term("not"), &None),
        ]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_ideographic_space_separates_items() {
        let ast = parse_to_ast("foo\u{3000}bar");
        let expected = group(vec![
            (&None, plain_term("foo"), &None),
            (&None, plain_term("bar"), &None),
        ]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_field_with_plain_term() {
        let ast = parse_to_ast("title:rust");
        let expected = group(vec![(
            &None,
            Expression::Field {
                field: "title".to_string(),
                expr: Box::new(plain_term("rust")),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_field_with_quoted_phrase() {
        let ast = parse_to_ast(r#"title:"exact phrase""#);
        let expected = group(vec![(
            &None,
            Expression::Field {
                field: "title".to_string(),
                expr: Box::new(term("exact phrase", true, false, None, None)),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_field_with_regex() {
        let ast = parse_to_ast("title:/rus.*/");
        let expected = group(vec![(
            &None,
            Expression::Field {
                field: "title".to_string(),
                expr: Box::new(term("rus.*", false, true, None, None)),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_field_with_inclusive_range() {
        let ast = parse_to_ast("status:[200 TO 299]");
        let expected = group(vec![(
            &None,
            Expression::Field {
                field: "status".to_string(),
                expr: Box::new(Expression::Range {
                    start: "200".to_string(),
                    end: "299".to_string(),
                    inclusive: true,
                    boost: None,
                }),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_field_with_exclusive_range() {
        let ast = parse_to_ast("date:{2020 TO 2021}");
        let expected = group(vec![(
            &None,
            Expression::Field {
                field: "date".to_string(),
                expr: Box::new(Expression::Range {
                    start: "2020".to_string(),
                    end: "2021".to_string(),
                    inclusive: false,
                    boost: None,
                }),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_field_with_sub_query() {
        let ast = parse_to_ast("tags:(rust OR go)");
        let expected = group(vec![(
            &None,
            Expression::Field {
                field: "tags".to_string(),
                expr: Box::new(Expression::SubQuery {
                    query: group(vec![
                        (&None, plain_term("rust"), &Some(BinaryOp::Or)),
                        (&None, plain_term("go"), &None),
                    ]),
                    boost: None,
                }),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_top_level_inclusive_range() {
        let ast = parse_to_ast("[alpha TO omega]");
        let expected = group(vec![(
            &None,
            Expression::Range {
                start: "alpha".to_string(),
                end: "omega".to_string(),
                inclusive: true,
                boost: None,
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_top_level_exclusive_range() {
        let ast = parse_to_ast("{alpha TO omega}");
        let expected = group(vec![(
            &None,
            Expression::Range {
                start: "alpha".to_string(),
                end: "omega".to_string(),
                inclusive: false,
                boost: None,
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_boosted_range() {
        let ast = parse_to_ast("[alpha TO omega]^2.5");
        let expected = group(vec![(
            &None,
            Expression::Range {
                start: "alpha".to_string(),
                end: "omega".to_string(),
                inclusive: true,
                boost: Some(2.5),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_field_with_boosted_range() {
        let ast = parse_to_ast("status:[200 TO 299]^0.5");
        let expected = group(vec![(
            &None,
            Expression::Field {
                field: "status".to_string(),
                expr: Box::new(Expression::Range {
                    start: "200".to_string(),
                    end: "299".to_string(),
                    inclusive: true,
                    boost: Some(0.5),
                }),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_boosted_sub_query() {
        let ast = parse_to_ast("(rust OR go)^1.5");
        let expected = group(vec![(
            &None,
            Expression::SubQuery {
                query: group(vec![
                    (&None, plain_term("rust"), &Some(BinaryOp::Or)),
                    (&None, plain_term("go"), &None),
                ]),
                boost: Some(1.5),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_simple_sub_query() {
        let ast = parse_to_ast("(a AND b)");
        let expected = group(vec![(
            &None,
            Expression::SubQuery {
                query: group(vec![
                    (&None, plain_term("a"), &Some(BinaryOp::And)),
                    (&None, plain_term("b"), &None),
                ]),
                boost: None,
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_nested_sub_query() {
        let ast = parse_to_ast("(x OR (y AND z))");
        let expected = group(vec![(
            &None,
            Expression::SubQuery {
                query: group(vec![
                    (&None, plain_term("x"), &Some(BinaryOp::Or)),
                    (
                        &None,
                        Expression::SubQuery {
                            query: group(vec![
                                (&None, plain_term("y"), &Some(BinaryOp::And)),
                                (&None, plain_term("z"), &None),
                            ]),
                            boost: None,
                        },
                        &None,
                    ),
                ]),
                boost: None,
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_complex_combined_query() {
        let ast = parse_to_ast("+title:rust AND body:\"memory safe\" -archived");

        let expected = group(vec![
            (
                &Some(Prefix::Must),
                Expression::Field {
                    field: "title".to_string(),
                    expr: Box::new(plain_term("rust")),
                },
                &Some(BinaryOp::And),
            ),
            (
                &None,
                Expression::Field {
                    field: "body".to_string(),
                    expr: Box::new(term("memory safe", true, false, None, None)),
                },
                &None,
            ),
            (&Some(Prefix::MustNot), plain_term("archived"), &None),
        ]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_range_bounds_are_unescaped() {
        let ast = parse_to_ast(r"field:[\(alpha\) TO \[omega\]]");
        let expected = group(vec![(
            &None,
            Expression::Field {
                field: "field".to_string(),
                expr: Box::new(Expression::Range {
                    start: "(alpha)".to_string(),
                    end: "[omega]".to_string(),
                    inclusive: true,
                    boost: None,
                }),
            },
            &None,
        )]);
        assert_eq!(ast, expected);
    }

    #[test]
    fn test_dangling_and_operator_fails_ast_validation() {
        let items = vec![QueryItem {
            prefix: None,
            expr: plain_term("a"),
            op: Some(BinaryOp::And),
        }];

        let err = ensure_no_dangling_operator(&items).expect_err("expected dangling operator");
        assert!(err.contains("dangling boolean operator"), "{err}");
        assert!(err.contains("And"), "{err}");
    }

    #[test]
    fn test_dangling_or_operator_fails_ast_validation() {
        let items = vec![QueryItem {
            prefix: None,
            expr: plain_term("a"),
            op: Some(BinaryOp::Or),
        }];

        let err = ensure_no_dangling_operator(&items).expect_err("expected dangling operator");
        assert!(err.contains("dangling boolean operator"), "{err}");
        assert!(err.contains("Or"), "{err}");
    }

    #[test]
    fn test_well_formed_items_pass_ast_validation() {
        let items = vec![
            QueryItem {
                prefix: None,
                expr: plain_term("a"),
                op: Some(BinaryOp::And),
            },
            QueryItem {
                prefix: None,
                expr: plain_term("b"),
                op: None,
            },
        ];

        assert_eq!(ensure_no_dangling_operator(&items), Ok(()));
    }

    #[test]
    fn test_empty_items_pass_ast_validation() {
        assert_eq!(ensure_no_dangling_operator(&[]), Ok(()));
    }

    #[test]
    fn test_parse_errors() {
        parse_err("");
        parse_err("   ");
        parse_err("AND");
        parse_err("a AND");
        parse_err("a OR");
        parse_err("a &&");
        parse_err("a ||");
        parse_err("(unclosed");
        parse_err("[a TO");
        parse_err("field:");
        parse_err(r#""unclosed"#);
        parse_err("/unclosed");
        parse_err("a:(");
        parse_err("field_name!:x");
    }
}

