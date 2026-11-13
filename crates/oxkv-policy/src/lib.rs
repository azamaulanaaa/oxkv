//! Canonical policy AST v1 for oxkv.
//!
//! The crate has four parts:
//!
//! - [`ast`] defines the canonical AST, the budget, and the policy version.
//! - [`error`] defines the policy error, reason, and budget types.
//! - [`eval`] defines the one production evaluator and the batch result.
//! - [`explain`] defines the stable explanation bytes.
//!
//! The evaluator is pure. It does not read a clock, draw randomness, perform
//! I/O, inspect the host, or keep state. It uses no floating point, no regular
//! expression engine, and no `unsafe` code. It compiles for native and
//! `wasm32-unknown-unknown`.
//!
//! # Example
//!
//! ```
//! use oxkv_policy::{
//!     Budget, CompareOp, EvalContext, Expr, Literal, Path, PathRoot, Policy, Row, RuleId,
//!     Verdict,
//! };
//!
//! let rule = RuleId::new("accounts.owner").expect("the rule identity is valid");
//! let expr = Expr::Compare {
//!     op: CompareOp::Eq,
//!     left: Box::new(Expr::Path(Path::field(PathRoot::Before, "owner"))),
//!     right: Box::new(Expr::Literal(Literal::Text("alice".to_owned()))),
//! };
//! let policy = Policy::new(rule, Budget::standard(), expr).expect("the policy is bounded");
//! let row = Row::new().with_field("owner", Literal::Text("alice".to_owned()));
//! let evaluation = policy.evaluate_read(b"row-1", &row);
//! assert_eq!(evaluation.verdict(), Verdict::Allow);
//! ```

#![forbid(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

pub mod ast;
pub mod error;
pub mod eval;
pub mod explain;

pub use ast::{
    Budget, CompareOp, Expr, FieldSchema, Literal, MAX_EXPR_DEPTH, MAX_EXPR_NODES,
    POLICY_AST_VERSION, Path, PathRoot, Policy, PolicyAstVersion, PolicyVersion, RuleId,
};
pub use error::{BudgetFailure, BudgetKind, PolicyError, ReasonCode};
pub use eval::{BatchOutcome, EvalContext, Evaluation, EvaluationStatus, Row, Verdict};
