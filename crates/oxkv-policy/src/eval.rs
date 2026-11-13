//! The one production evaluator for read scope and write validation.
//!
//! Read scope binds `before` and `after` to the same row. Write validation
//! binds `before` to the stored value and `after` to the proposed value.
//! Evaluation is per row. A batch verdict is the conjunction of the row
//! verdicts.

use std::collections::BTreeMap;

use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::PolicyVersion;

use crate::ast::{CompareOp, Expr, Literal, Path, PathRoot, Policy, RuleId};
use crate::error::{BudgetFailure, BudgetKind, ReasonCode};

/// One flat row of declared fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    fields: BTreeMap<String, Literal>,
}

impl Row {
    /// Creates an empty row.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one field and returns the row.
    #[must_use]
    pub fn with_field(mut self, name: impl Into<String>, value: Literal) -> Self {
        self.insert(name, value);
        self
    }

    /// Adds or replaces one field.
    pub fn insert(&mut self, name: impl Into<String>, value: Literal) {
        self.fields.insert(name.into(), value);
    }

    /// Returns one field value.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Literal> {
        self.fields.get(name)
    }

    /// Returns the number of fields.
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns true when the row has no fields.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// The values that one evaluation reads.
///
/// [`EvalContext::read`] binds both sides to the same row.
/// [`EvalContext::write`] binds the two sides to different values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalContext<'a> {
    /// The row key.
    pub row_key: &'a [u8],
    /// The stored value.
    pub before: &'a Row,
    /// The proposed value.
    pub after: &'a Row,
}

impl<'a> EvalContext<'a> {
    /// Creates a read scope context. Both sides bind to the same row.
    #[must_use]
    pub const fn read(row_key: &'a [u8], row: &'a Row) -> Self {
        Self {
            row_key,
            before: row,
            after: row,
        }
    }

    /// Creates a write validation context.
    #[must_use]
    pub const fn write(row_key: &'a [u8], before: &'a Row, after: &'a Row) -> Self {
        Self {
            row_key,
            before,
            after,
        }
    }
}

/// The boolean verdict of one evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// The expression is true.
    Allow,
    /// The expression is false or the evaluation failed closed.
    Deny,
}

impl Verdict {
    /// Returns the stable verdict name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    /// Returns true for an allow verdict.
    #[must_use]
    pub const fn is_allow(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// The terminal status of one evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationStatus {
    /// The expression is true.
    Allow,
    /// The expression is false or the input is invalid.
    Deny,
    /// A budget stopped the evaluation. This status maps to `Rejected`.
    BudgetExhausted(BudgetFailure),
}

impl EvaluationStatus {
    /// Returns the stable status name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::BudgetExhausted(_) => "budget_exhausted",
        }
    }

    /// Returns the verdict. A budget failure maps to deny, never to allow.
    #[must_use]
    pub const fn verdict(self) -> Verdict {
        match self {
            Self::Allow => Verdict::Allow,
            Self::Deny | Self::BudgetExhausted(_) => Verdict::Deny,
        }
    }

    /// Returns the budget failure when one is present.
    #[must_use]
    pub const fn budget_failure(self) -> Option<BudgetFailure> {
        match self {
            Self::BudgetExhausted(failure) => Some(failure),
            Self::Allow | Self::Deny => None,
        }
    }

    /// Returns the write receipt state for this status.
    ///
    /// An allow verdict has no rejection receipt. A deny verdict and a budget
    /// failure both map to `Rejected`.
    #[must_use]
    pub const fn receipt_state(self) -> Option<ReceiptState> {
        match self {
            Self::Allow => None,
            Self::Deny | Self::BudgetExhausted(_) => Some(ReceiptState::Rejected),
        }
    }
}

/// The structured result of one row evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    /// The row key.
    pub row_key: Vec<u8>,
    /// The rule identity.
    pub rule: RuleId,
    /// The terminal status.
    pub status: EvaluationStatus,
    /// The stable reason code.
    pub reason: ReasonCode,
    /// The last evaluated path.
    pub path: Option<Path>,
    /// The policy version that produced the result.
    pub policy_version: PolicyVersion,
    /// The number of steps used.
    pub steps_used: u64,
    /// The maximum expression depth reached.
    pub depth_used: u32,
    /// The number of arena bytes used.
    pub arena_used: u64,
}

impl Evaluation {
    /// Returns the verdict.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        self.status.verdict()
    }

    /// Returns true for an allow verdict.
    #[must_use]
    pub const fn is_allow(&self) -> bool {
        self.verdict().is_allow()
    }

    /// Returns the budget failure when one is present.
    #[must_use]
    pub const fn budget_failure(&self) -> Option<BudgetFailure> {
        self.status.budget_failure()
    }
}

/// The result of a batch evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    /// The conjunction of the row verdicts.
    pub verdict: Verdict,
    /// One decision per row, in input order.
    pub decisions: Vec<Evaluation>,
}

impl BatchOutcome {
    /// Returns true when every row allows the operation.
    #[must_use]
    pub const fn may_commit(&self) -> bool {
        self.verdict.is_allow()
    }

    /// Returns the first budget failure in the decision list.
    #[must_use]
    pub fn budget_failure(&self) -> Option<BudgetFailure> {
        self.decisions
            .iter()
            .find_map(|decision| decision.status.budget_failure())
    }
}

impl Policy {
    /// Evaluates one row with the production evaluator.
    #[must_use]
    pub fn evaluate(&self, context: &EvalContext<'_>) -> Evaluation {
        let mut evaluator = Evaluator::new(self, context);
        let result = evaluator
            .eval(self.expr(), 0)
            .and_then(|value| match value {
                Literal::Bool(value) => Ok(value),
                _ => Err(EvalFault::Runtime(ReasonCode::NotTotal)),
            });
        let (status, reason) = match result {
            Ok(true) => (EvaluationStatus::Allow, ReasonCode::Allow),
            Ok(false) => (EvaluationStatus::Deny, ReasonCode::Deny),
            Err(EvalFault::Budget(failure)) => (
                EvaluationStatus::BudgetExhausted(failure),
                ReasonCode::BudgetExhausted,
            ),
            Err(EvalFault::Runtime(reason)) => (EvaluationStatus::Deny, reason),
        };
        Evaluation {
            row_key: context.row_key.to_vec(),
            rule: self.rule().clone(),
            status,
            reason,
            path: evaluator.last_path,
            policy_version: self.version(),
            steps_used: evaluator.steps,
            depth_used: evaluator.max_depth,
            arena_used: evaluator.arena,
        }
    }

    /// Evaluates one read scope row. Both sides bind to the same row.
    #[must_use]
    pub fn evaluate_read(&self, row_key: &[u8], row: &Row) -> Evaluation {
        self.evaluate(&EvalContext::read(row_key, row))
    }

    /// Evaluates one write validation row.
    #[must_use]
    pub fn evaluate_write(&self, row_key: &[u8], before: &Row, after: &Row) -> Evaluation {
        self.evaluate(&EvalContext::write(row_key, before, after))
    }

    /// Evaluates a batch and returns the conjunction of the row verdicts.
    ///
    /// The function evaluates every row. A budget failure maps to deny, so a
    /// batch never commits after exhaustion.
    #[must_use]
    pub fn evaluate_batch(&self, contexts: &[EvalContext<'_>]) -> BatchOutcome {
        let mut decisions = Vec::with_capacity(contexts.len());
        let mut verdict = Verdict::Allow;
        for context in contexts {
            let decision = self.evaluate(context);
            if !decision.is_allow() {
                verdict = Verdict::Deny;
            }
            decisions.push(decision);
        }
        BatchOutcome { verdict, decisions }
    }
}

enum EvalFault {
    Budget(BudgetFailure),
    Runtime(ReasonCode),
}

struct Evaluator<'a, 'p> {
    policy: &'p Policy,
    context: &'a EvalContext<'a>,
    steps: u64,
    arena: u64,
    max_depth: u32,
    last_path: Option<Path>,
}

impl<'a, 'p> Evaluator<'a, 'p> {
    const fn new(policy: &'p Policy, context: &'a EvalContext<'a>) -> Self {
        Self {
            policy,
            context,
            steps: 0,
            arena: 0,
            max_depth: 0,
            last_path: None,
        }
    }

    fn enter(&mut self, expr: &Expr, depth: u32) -> Result<(), EvalFault> {
        let budget = self.policy.budget();
        if depth > budget.depth() {
            return Err(EvalFault::Budget(BudgetFailure::new(
                BudgetKind::Depth,
                u64::from(budget.depth()),
                u64::from(depth),
                depth,
            )));
        }
        let cost = expr.step_cost();
        let steps = self.steps.checked_add(cost).ok_or_else(|| {
            EvalFault::Budget(BudgetFailure::new(
                BudgetKind::Step,
                budget.steps(),
                budget.steps().saturating_add(1),
                depth,
            ))
        })?;
        if steps > budget.steps() {
            return Err(EvalFault::Budget(BudgetFailure::new(
                BudgetKind::Step,
                budget.steps(),
                steps,
                depth,
            )));
        }
        self.steps = steps;
        let arena = self.arena.checked_add(cost).ok_or_else(|| {
            EvalFault::Budget(BudgetFailure::new(
                BudgetKind::Memory,
                budget.arena_bytes(),
                budget.arena_bytes().saturating_add(1),
                depth,
            ))
        })?;
        if arena > budget.arena_bytes() {
            return Err(EvalFault::Budget(BudgetFailure::new(
                BudgetKind::Memory,
                budget.arena_bytes(),
                arena,
                depth,
            )));
        }
        self.arena = arena;
        self.max_depth = self.max_depth.max(depth);
        Ok(())
    }

    fn eval(&mut self, expr: &Expr, depth: u32) -> Result<Literal, EvalFault> {
        if let Expr::Path(path) | Expr::Exists(path) = expr {
            self.observe(path);
        }
        self.enter(expr, depth)?;
        let child_depth = depth.saturating_add(1);
        match expr {
            Expr::Literal(literal) => Ok(literal.clone()),
            Expr::Path(path) => self.resolve(path),
            Expr::Exists(path) => Ok(Literal::Bool(
                self.row(path.root()).get(&path.dotted()).is_some(),
            )),
            Expr::Compare { op, left, right } => {
                let left = self.eval(left, child_depth)?;
                let right = self.eval(right, child_depth)?;
                compare(*op, &left, &right)
                    .map(Literal::Bool)
                    .ok_or(EvalFault::Runtime(ReasonCode::NotTotal))
            }
            Expr::And(left, right) => {
                if self.eval_bool(left, child_depth)? {
                    Ok(Literal::Bool(self.eval_bool(right, child_depth)?))
                } else {
                    Ok(Literal::Bool(false))
                }
            }
            Expr::Or(left, right) => {
                if self.eval_bool(left, child_depth)? {
                    Ok(Literal::Bool(true))
                } else {
                    Ok(Literal::Bool(self.eval_bool(right, child_depth)?))
                }
            }
            Expr::Not(inner) => Ok(Literal::Bool(!self.eval_bool(inner, child_depth)?)),
            Expr::In { value, set } => {
                let value = self.eval(value, child_depth)?;
                Ok(Literal::Bool(set.iter().any(|item| item == &value)))
            }
            Expr::Regex { .. } => Err(EvalFault::Runtime(ReasonCode::RegexNotSupported)),
        }
    }

    fn eval_bool(&mut self, expr: &Expr, depth: u32) -> Result<bool, EvalFault> {
        match self.eval(expr, depth)? {
            Literal::Bool(value) => Ok(value),
            _ => Err(EvalFault::Runtime(ReasonCode::NotTotal)),
        }
    }

    fn resolve(&mut self, path: &Path) -> Result<Literal, EvalFault> {
        self.row(path.root())
            .get(&path.dotted())
            .cloned()
            .ok_or(EvalFault::Runtime(ReasonCode::UnknownField))
    }

    const fn row(&self, root: PathRoot) -> &'a Row {
        match root {
            PathRoot::Before => self.context.before,
            PathRoot::After => self.context.after,
        }
    }

    fn observe(&mut self, path: &Path) {
        self.last_path = Some(path.clone());
    }
}

fn compare(op: CompareOp, left: &Literal, right: &Literal) -> Option<bool> {
    match (left, right) {
        (Literal::Bool(left), Literal::Bool(right)) => match op {
            CompareOp::Eq => Some(left == right),
            CompareOp::Ne => Some(left != right),
            CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge => None,
        },
        (Literal::I64(left), Literal::I64(right)) => Some(ordered(op, left, right)),
        (Literal::U64(left), Literal::U64(right)) => Some(ordered(op, left, right)),
        (Literal::Text(left), Literal::Text(right)) => Some(ordered(op, left, right)),
        (Literal::Bytes(left), Literal::Bytes(right)) => Some(ordered(op, left, right)),
        _ => None,
    }
}

fn ordered<T: Ord>(op: CompareOp, left: &T, right: &T) -> bool {
    match op {
        CompareOp::Eq => left == right,
        CompareOp::Ne => left != right,
        CompareOp::Lt => left < right,
        CompareOp::Le => left <= right,
        CompareOp::Gt => left > right,
        CompareOp::Ge => left >= right,
    }
}
