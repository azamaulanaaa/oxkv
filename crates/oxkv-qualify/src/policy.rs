//! A small deterministic policy evaluator.
//!
//! The evaluator is a wave-0 sample of the `policy-ast/v1` surface. It reads a
//! declared fact map. It never reads a clock, entropy, or a locale. Every
//! evaluation terminates inside a step budget, a depth budget, and an arena
//! budget.
//!
//! The grammar is a subset of the frozen language:
//!
//! ```text
//! expr := "allow" | "deny" | "has(" ident ")"
//!       | "eq(" ident "," literal ")" | "lt(" ident "," literal ")"
//!       | "not(" expr ")"
//!       | "and(" expr ("," expr)* ")" | "or(" expr ("," expr)* ")"
//! ```
//!
//! A float is not a literal.

use std::collections::BTreeMap;

/// A fact value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactValue {
    /// A signed integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// A text value.
    Text(String),
    /// An opaque byte string.
    Bytes(Vec<u8>),
}

/// A literal in an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// A signed integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// A text value.
    Text(String),
}

/// A parsed policy expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// The expression always allows.
    Allow,
    /// The expression always denies.
    Deny,
    /// The expression allows when the fact exists.
    Has(String),
    /// The expression allows when the fact equals the literal.
    Eq(String, Literal),
    /// The expression allows when the integer fact is less than the literal.
    Lt(String, Literal),
    /// The expression allows when every child allows.
    And(Vec<Self>),
    /// The expression allows when one child allows.
    Or(Vec<Self>),
    /// The expression inverts its child.
    Not(Box<Self>),
}

/// A policy error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// The text does not parse.
    ParseError,
    /// A fact path is not in the fact set.
    UnknownField(String),
    /// An evaluation budget is exhausted.
    BudgetExhausted,
    /// The expression is not total for the fact set.
    NotTotal,
}

impl PolicyError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ParseError => "policy.parse_error",
            Self::UnknownField(_) => "policy.unknown_field",
            Self::BudgetExhausted => "policy.budget_exhausted",
            Self::NotTotal => "policy.not_total",
        }
    }
}

impl core::fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.code())
    }
}

/// A policy verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The request is allowed.
    Allow,
    /// The request is denied.
    Deny,
}

impl Verdict {
    /// Returns the canonical verdict text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Allow => "Allow",
            Self::Deny => "Deny",
        }
    }
}

/// The limits of one evaluation.
///
/// The three limits use the `max_` prefix on purpose. A reader must not
/// confuse a limit with the measured cost in [`EvalStats`].
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalBudget {
    /// The maximum number of visited nodes.
    pub max_steps: u32,
    /// The maximum expression depth.
    pub max_depth: u32,
    /// The maximum arena units.
    pub max_arena: u32,
}

impl Default for EvalBudget {
    fn default() -> Self {
        Self {
            max_steps: 1024,
            max_depth: 32,
            max_arena: 65_536,
        }
    }
}

/// The measured cost of one evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalStats {
    /// The number of visited nodes.
    pub steps: u32,
    /// The maximum observed depth.
    pub depth: u32,
    /// The arena units that the evaluation used.
    pub arena: u32,
}

/// The result of one evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evaluation {
    /// The verdict.
    pub verdict: Verdict,
    /// The measured cost.
    pub stats: EvalStats,
}

/// Parses one expression.
///
/// # Errors
///
/// Returns [`PolicyError::ParseError`] for invalid text.
/// Returns [`PolicyError::BudgetExhausted`] when the text exceeds the depth
/// limit or the byte limit.
pub fn parse(text: &str) -> Result<Expr, PolicyError> {
    if text.len() > 4096 {
        return Err(PolicyError::BudgetExhausted);
    }
    let mut parser = Parser {
        bytes: text.as_bytes(),
        position: 0,
    };
    let expression = parser.expr(0)?;
    parser.skip_space();
    if parser.position == parser.bytes.len() {
        Ok(expression)
    } else {
        Err(PolicyError::ParseError)
    }
}

/// Parses and evaluates one expression.
///
/// # Errors
///
/// Returns a [`PolicyError`] for a parse failure, a missing fact, a type
/// mismatch, or an exhausted budget.
pub fn evaluate(
    text: &str,
    facts: &BTreeMap<String, FactValue>,
    budget: EvalBudget,
) -> Result<Evaluation, PolicyError> {
    let expression = parse(text)?;
    evaluate_expr(&expression, facts, budget)
}

/// Evaluates one parsed expression.
///
/// # Errors
///
/// Returns a [`PolicyError`] for a missing fact, a type mismatch, or an
/// exhausted budget.
pub fn evaluate_expr(
    expression: &Expr,
    facts: &BTreeMap<String, FactValue>,
    budget: EvalBudget,
) -> Result<Evaluation, PolicyError> {
    let mut state = EvalState::default();
    let verdict = eval(expression, facts, budget, 0, &mut state)?;
    Ok(Evaluation {
        verdict,
        stats: EvalStats {
            steps: state.steps,
            depth: state.depth,
            arena: state.arena,
        },
    })
}

#[derive(Default)]
struct EvalState {
    steps: u32,
    depth: u32,
    arena: u32,
}

fn add_arena(state: &mut EvalState, units: usize) {
    let units = u32::try_from(units).unwrap_or(u32::MAX);
    state.arena = state.arena.saturating_add(units);
}

fn charge(
    state: &mut EvalState,
    budget: EvalBudget,
    depth: u32,
    arena: usize,
) -> Result<(), PolicyError> {
    state.steps = state.steps.saturating_add(1);
    state.depth = state.depth.max(depth);
    add_arena(state, arena);
    if state.steps > budget.max_steps
        || state.depth > budget.max_depth
        || state.arena > budget.max_arena
    {
        return Err(PolicyError::BudgetExhausted);
    }
    Ok(())
}

fn eval(
    expression: &Expr,
    facts: &BTreeMap<String, FactValue>,
    budget: EvalBudget,
    depth: u32,
    state: &mut EvalState,
) -> Result<Verdict, PolicyError> {
    charge(state, budget, depth, 1)?;
    match expression {
        Expr::Allow => Ok(Verdict::Allow),
        Expr::Deny => Ok(Verdict::Deny),
        Expr::Has(field) => {
            charge(state, budget, depth, field.len())?;
            Ok(if facts.contains_key(field) {
                Verdict::Allow
            } else {
                Verdict::Deny
            })
        }
        Expr::Eq(field, literal) => {
            compare(facts, field, literal, Compare::Equal, budget, depth, state)
        }
        Expr::Lt(field, literal) => {
            compare(facts, field, literal, Compare::Less, budget, depth, state)
        }
        Expr::And(children) => {
            for child in children {
                if eval(child, facts, budget, depth + 1, state)? == Verdict::Deny {
                    return Ok(Verdict::Deny);
                }
            }
            Ok(Verdict::Allow)
        }
        Expr::Or(children) => {
            for child in children {
                if eval(child, facts, budget, depth + 1, state)? == Verdict::Allow {
                    return Ok(Verdict::Allow);
                }
            }
            Ok(Verdict::Deny)
        }
        Expr::Not(child) => {
            let verdict = eval(child, facts, budget, depth + 1, state)?;
            Ok(match verdict {
                Verdict::Allow => Verdict::Deny,
                Verdict::Deny => Verdict::Allow,
            })
        }
    }
}

#[derive(Clone, Copy)]
enum Compare {
    Equal,
    Less,
}

fn compare(
    facts: &BTreeMap<String, FactValue>,
    field: &str,
    literal: &Literal,
    mode: Compare,
    budget: EvalBudget,
    depth: u32,
    state: &mut EvalState,
) -> Result<Verdict, PolicyError> {
    let fact = facts
        .get(field)
        .ok_or_else(|| PolicyError::UnknownField(field.to_owned()))?;
    charge(state, budget, depth, field.len())?;
    let result = match (fact, literal) {
        (FactValue::Bool(left), Literal::Bool(right)) if matches!(mode, Compare::Equal) => {
            *left == *right
        }
        (FactValue::Int(left), Literal::Int(right)) => match mode {
            Compare::Equal => left == right,
            Compare::Less => left < right,
        },
        (FactValue::Text(left), Literal::Text(right)) if matches!(mode, Compare::Equal) => {
            left == right
        }
        (FactValue::Bytes(left), Literal::Text(right)) if matches!(mode, Compare::Equal) => {
            left.as_slice() == right.as_bytes()
        }
        _ => return Err(PolicyError::NotTotal),
    };
    Ok(if result {
        Verdict::Allow
    } else {
        Verdict::Deny
    })
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn expr(&mut self, depth: u32) -> Result<Expr, PolicyError> {
        if depth > 32 {
            return Err(PolicyError::BudgetExhausted);
        }
        self.skip_space();
        let name = self.ident()?;
        self.skip_space();
        if !self.consume(b'(') {
            return match name.as_str() {
                "allow" => Ok(Expr::Allow),
                "deny" => Ok(Expr::Deny),
                _ => Err(PolicyError::ParseError),
            };
        }
        let result = match name.as_str() {
            "has" => {
                let field = self.ident()?;
                Expr::Has(field)
            }
            "eq" => {
                let field = self.ident()?;
                self.comma()?;
                let literal = self.literal()?;
                Expr::Eq(field, literal)
            }
            "lt" => {
                let field = self.ident()?;
                self.comma()?;
                let literal = self.literal()?;
                Expr::Lt(field, literal)
            }
            "not" => {
                let child = self.expr(depth + 1)?;
                Expr::Not(Box::new(child))
            }
            "and" => Expr::And(self.list(depth + 1)?),
            "or" => Expr::Or(self.list(depth + 1)?),
            _ => return Err(PolicyError::ParseError),
        };
        self.skip_space();
        if self.consume(b')') {
            Ok(result)
        } else {
            Err(PolicyError::ParseError)
        }
    }

    fn list(&mut self, depth: u32) -> Result<Vec<Expr>, PolicyError> {
        let mut items = Vec::new();
        items.push(self.expr(depth)?);
        loop {
            self.skip_space();
            if !self.consume(b',') {
                break;
            }
            items.push(self.expr(depth)?);
        }
        Ok(items)
    }

    fn literal(&mut self) -> Result<Literal, PolicyError> {
        self.skip_space();
        if self.consume(b'"') {
            let mut text = String::new();
            loop {
                let Some(byte) = self.peek() else {
                    return Err(PolicyError::ParseError);
                };
                self.position += 1;
                match byte {
                    b'"' => return Ok(Literal::Text(text)),
                    b'\\' => {
                        let Some(escaped) = self.peek() else {
                            return Err(PolicyError::ParseError);
                        };
                        self.position += 1;
                        match escaped {
                            b'"' => text.push('"'),
                            b'\\' => text.push('\\'),
                            _ => return Err(PolicyError::ParseError),
                        }
                    }
                    byte if byte.is_ascii_control() => return Err(PolicyError::ParseError),
                    byte => text.push(char::from(byte)),
                }
            }
        }
        let start = self.position;
        let _negative = self.consume(b'-');
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        let token = core::str::from_utf8(&self.bytes[start..self.position])
            .map_err(|_| PolicyError::ParseError)?;
        if token.is_empty() || token == "-" {
            self.position = start;
            let name = self.ident()?;
            return match name.as_str() {
                "true" => Ok(Literal::Bool(true)),
                "false" => Ok(Literal::Bool(false)),
                _ => Err(PolicyError::ParseError),
            };
        }
        token
            .parse::<i64>()
            .map(Literal::Int)
            .map_err(|_| PolicyError::ParseError)
    }

    fn ident(&mut self) -> Result<String, PolicyError> {
        self.skip_space();
        let start = self.position;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
        {
            self.position += 1;
        }
        if self.position == start {
            return Err(PolicyError::ParseError);
        }
        core::str::from_utf8(&self.bytes[start..self.position])
            .map(str::to_owned)
            .map_err(|_| PolicyError::ParseError)
    }

    fn comma(&mut self) -> Result<(), PolicyError> {
        self.skip_space();
        if self.consume(b',') {
            Ok(())
        } else {
            Err(PolicyError::ParseError)
        }
    }

    fn consume(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.position += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        EvalBudget, FactValue, Literal, PolicyError, Verdict, evaluate, evaluate_expr, parse,
    };

    fn facts() -> BTreeMap<String, FactValue> {
        let mut facts = BTreeMap::new();
        facts.insert("tier".to_owned(), FactValue::Text("gold".to_owned()));
        facts.insert("region".to_owned(), FactValue::Text("eu".to_owned()));
        facts.insert("age".to_owned(), FactValue::Int(7));
        facts.insert("active".to_owned(), FactValue::Bool(true));
        facts
    }

    #[test]
    fn a_constant_expression_has_one_verdict() {
        let budget = EvalBudget::default();
        assert_eq!(
            evaluate("allow", &facts(), budget)
                .expect("the expression is valid")
                .verdict,
            Verdict::Allow
        );
        assert_eq!(
            evaluate("deny", &facts(), budget)
                .expect("the expression is valid")
                .verdict,
            Verdict::Deny
        );
    }

    #[test]
    fn and_short_circuits_on_deny() {
        let result = evaluate("and(deny,eq(missing,1))", &facts(), EvalBudget::default())
            .expect("the first child decides");
        assert_eq!(result.verdict, Verdict::Deny);
        assert_eq!(result.stats.steps, 2);
    }

    #[test]
    fn or_short_circuits_on_allow() {
        let result = evaluate("or(allow,eq(missing,1))", &facts(), EvalBudget::default())
            .expect("the first child decides");
        assert_eq!(result.verdict, Verdict::Allow);
        assert_eq!(result.stats.steps, 2);
    }

    #[test]
    fn a_missing_fact_is_a_named_error() {
        let error = evaluate("eq(missing,1)", &facts(), EvalBudget::default())
            .expect_err("the fact is absent");
        assert_eq!(error.code(), "policy.unknown_field");
    }

    #[test]
    fn a_type_mismatch_is_not_total() {
        let error = evaluate("eq(age,\"seven\")", &facts(), EvalBudget::default())
            .expect_err("the types differ");
        assert_eq!(error.code(), "policy.not_total");
    }

    #[test]
    fn the_step_budget_is_named() {
        let budget = EvalBudget {
            max_steps: 1,
            ..EvalBudget::default()
        };
        let error =
            evaluate("and(allow,allow)", &facts(), budget).expect_err("the budget is too small");
        assert_eq!(error, PolicyError::BudgetExhausted);
    }

    #[test]
    fn a_deep_expression_is_rejected() {
        let error = parse(&"not(".repeat(40)).expect_err("the depth is too large");
        assert_eq!(error, PolicyError::BudgetExhausted);
    }

    #[test]
    fn an_integer_literal_parses() {
        let expression = parse("lt(age,-1)").expect("the expression is valid");
        assert_eq!(
            expression,
            super::Expr::Lt("age".to_owned(), Literal::Int(-1))
        );
    }

    #[test]
    fn a_parsed_expression_has_a_stable_cost() {
        let expression =
            parse("and(eq(tier,\"gold\"),eq(region,\"eu\"))").expect("the expression is valid");
        let first = evaluate_expr(&expression, &facts(), EvalBudget::default())
            .expect("the expression evaluates");
        let second = evaluate_expr(&expression, &facts(), EvalBudget::default())
            .expect("the expression evaluates");
        assert_eq!(first, second);
        assert!(first.stats.steps >= 3);
    }
}
