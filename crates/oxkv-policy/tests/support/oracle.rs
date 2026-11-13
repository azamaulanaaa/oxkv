#![allow(missing_docs)]
#![allow(unreachable_pub)]

//! A test-only differential oracle.
//!
//! The oracle is an independent tree walk. It recomputes the step model from
//! the AST. The production API cannot reach it: the module lives in the test
//! tree only.

use oxkv_policy::{
    BudgetFailure, BudgetKind, CompareOp, EvalContext, Expr, Literal, Path, PathRoot, Policy,
    ReasonCode, Row, Verdict,
};

/// The oracle result for one row.
pub struct OracleOutcome {
    pub verdict: Verdict,
    pub reason: ReasonCode,
    pub path: Option<Path>,
    pub budget: Option<BudgetFailure>,
    pub steps: u64,
    pub depth: u32,
    pub arena: u64,
}

/// Evaluates one row with the independent oracle.
pub fn evaluate(policy: &Policy, context: &EvalContext<'_>) -> OracleOutcome {
    let mut oracle = Oracle {
        context,
        budget: policy.budget(),
        steps: 0,
        arena: 0,
        depth: 0,
        path: None,
    };
    let result = oracle.eval(policy.expr(), 0).and_then(|value| match value {
        Literal::Bool(value) => Ok(value),
        _ => Err(Fault::Runtime(ReasonCode::NotTotal)),
    });
    let (verdict, reason, budget) = match result {
        Ok(true) => (Verdict::Allow, ReasonCode::Allow, None),
        Ok(false) => (Verdict::Deny, ReasonCode::Deny, None),
        Err(Fault::Budget(failure)) => (Verdict::Deny, ReasonCode::BudgetExhausted, Some(failure)),
        Err(Fault::Runtime(reason)) => (Verdict::Deny, reason, None),
    };
    OracleOutcome {
        verdict,
        reason,
        path: oracle.path,
        budget,
        steps: oracle.steps,
        depth: oracle.depth,
        arena: oracle.arena,
    }
}

enum Fault {
    Budget(BudgetFailure),
    Runtime(ReasonCode),
}

struct Oracle<'a> {
    context: &'a EvalContext<'a>,
    budget: oxkv_policy::Budget,
    steps: u64,
    arena: u64,
    depth: u32,
    path: Option<Path>,
}

impl Oracle<'_> {
    fn enter(&mut self, expr: &Expr, depth: u32) -> Result<(), Fault> {
        if depth > self.budget.depth() {
            return Err(Fault::Budget(BudgetFailure::new(
                BudgetKind::Depth,
                u64::from(self.budget.depth()),
                u64::from(depth),
                depth,
            )));
        }
        let cost = cost(expr);
        let Some(steps) = self.steps.checked_add(cost) else {
            return Err(Fault::Budget(BudgetFailure::new(
                BudgetKind::Step,
                self.budget.steps(),
                self.budget.steps().saturating_add(1),
                depth,
            )));
        };
        if steps > self.budget.steps() {
            return Err(Fault::Budget(BudgetFailure::new(
                BudgetKind::Step,
                self.budget.steps(),
                steps,
                depth,
            )));
        }
        self.steps = steps;
        let Some(arena) = self.arena.checked_add(cost) else {
            return Err(Fault::Budget(BudgetFailure::new(
                BudgetKind::Memory,
                self.budget.arena_bytes(),
                self.budget.arena_bytes().saturating_add(1),
                depth,
            )));
        };
        if arena > self.budget.arena_bytes() {
            return Err(Fault::Budget(BudgetFailure::new(
                BudgetKind::Memory,
                self.budget.arena_bytes(),
                arena,
                depth,
            )));
        }
        self.arena = arena;
        self.depth = self.depth.max(depth);
        Ok(())
    }

    fn eval(&mut self, expr: &Expr, depth: u32) -> Result<Literal, Fault> {
        if let Expr::Path(path) | Expr::Exists(path) = expr {
            self.observe(path);
        }
        self.enter(expr, depth)?;
        let child = depth.saturating_add(1);
        match expr {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::Path(path) => self
                .lookup(path)
                .cloned()
                .ok_or(Fault::Runtime(ReasonCode::UnknownField)),
            Expr::Exists(path) => Ok(Literal::Bool(self.lookup(path).is_some())),
            Expr::Compare { op, left, right } => {
                let left = self.eval(left, child)?;
                let right = self.eval(right, child)?;
                compare(*op, &left, &right)
                    .map(Literal::Bool)
                    .ok_or(Fault::Runtime(ReasonCode::NotTotal))
            }
            Expr::And(left, right) => {
                if self.as_bool(left, child)? {
                    Ok(Literal::Bool(self.as_bool(right, child)?))
                } else {
                    Ok(Literal::Bool(false))
                }
            }
            Expr::Or(left, right) => {
                if self.as_bool(left, child)? {
                    Ok(Literal::Bool(true))
                } else {
                    Ok(Literal::Bool(self.as_bool(right, child)?))
                }
            }
            Expr::Not(inner) => Ok(Literal::Bool(!self.as_bool(inner, child)?)),
            Expr::In { value, set } => {
                let value = self.eval(value, child)?;
                Ok(Literal::Bool(set.iter().any(|item| item == &value)))
            }
            Expr::Regex { .. } => Err(Fault::Runtime(ReasonCode::RegexNotSupported)),
        }
    }

    fn as_bool(&mut self, expr: &Expr, depth: u32) -> Result<bool, Fault> {
        match self.eval(expr, depth)? {
            Literal::Bool(value) => Ok(value),
            _ => Err(Fault::Runtime(ReasonCode::NotTotal)),
        }
    }

    fn lookup(&self, path: &Path) -> Option<&Literal> {
        self.row(path.root()).get(&path.dotted())
    }

    const fn row(&self, root: PathRoot) -> &Row {
        match root {
            PathRoot::Before => self.context.before,
            PathRoot::After => self.context.after,
        }
    }

    fn observe(&mut self, path: &Path) {
        self.path = Some(path.clone());
    }
}

fn cost(expr: &Expr) -> u64 {
    match expr {
        Expr::Literal(value) => 1_u64.saturating_add(value_weight(value)),
        Expr::Path(path) | Expr::Exists(path) => 1_u64.saturating_add(path_weight(path)),
        Expr::Compare { .. } | Expr::And(..) | Expr::Or(..) | Expr::Not(..) => 1,
        Expr::In { set, .. } => set
            .iter()
            .fold(1_u64.saturating_add(as_u64(set.len())), |total, value| {
                total.saturating_add(value_weight(value))
            }),
        Expr::Regex { pattern } => 1_u64.saturating_add(as_u64(pattern.len())),
    }
}

fn value_weight(value: &Literal) -> u64 {
    match value {
        Literal::Bool(_) | Literal::I64(_) | Literal::U64(_) => 1,
        Literal::Text(text) => as_u64(text.len()),
        Literal::Bytes(bytes) => as_u64(bytes.len()),
    }
}

fn path_weight(path: &Path) -> u64 {
    path.segments().iter().fold(0_u64, |total, segment| {
        total.saturating_add(as_u64(segment.len()))
    })
}

fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn compare(op: CompareOp, left: &Literal, right: &Literal) -> Option<bool> {
    match (left, right) {
        (Literal::Bool(left), Literal::Bool(right)) => match op {
            CompareOp::Eq => Some(left == right),
            CompareOp::Ne => Some(left != right),
            CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge => None,
        },
        (Literal::I64(left), Literal::I64(right)) => Some(order(op, left, right)),
        (Literal::U64(left), Literal::U64(right)) => Some(order(op, left, right)),
        (Literal::Text(left), Literal::Text(right)) => Some(order(op, left, right)),
        (Literal::Bytes(left), Literal::Bytes(right)) => Some(order(op, left, right)),
        _ => None,
    }
}

fn order<T: Ord>(op: CompareOp, left: &T, right: &T) -> bool {
    match op {
        CompareOp::Eq => left == right,
        CompareOp::Ne => left != right,
        CompareOp::Lt => left < right,
        CompareOp::Le => left <= right,
        CompareOp::Gt => left > right,
        CompareOp::Ge => left >= right,
    }
}
