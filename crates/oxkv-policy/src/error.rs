//! Policy errors, reason codes, and budget failures.
//!
//! Every error names one contract code. A budget failure always names
//! `policy.budget_exhausted`. A budget failure never becomes a default allow.

use core::fmt;
use oxkv_contracts::error::ErrorCode;

/// The budget that stopped an evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BudgetKind {
    /// The step budget.
    Step,
    /// The depth budget.
    Depth,
    /// The fixed memory arena.
    Memory,
}

impl BudgetKind {
    /// Returns the stable budget name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Step => "step",
            Self::Depth => "depth",
            Self::Memory => "memory",
        }
    }

    /// Returns the stable encoding tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Step => 0,
            Self::Depth => 1,
            Self::Memory => 2,
        }
    }
}

impl fmt::Display for BudgetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One named budget failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BudgetFailure {
    /// The budget that failed.
    pub budget: BudgetKind,
    /// The declared limit.
    pub limit: u64,
    /// The value that exceeded the limit.
    pub used: u64,
    /// The expression depth at the failure.
    pub depth: u32,
}

impl BudgetFailure {
    /// Creates a budget failure.
    #[must_use]
    pub const fn new(budget: BudgetKind, limit: u64, used: u64, depth: u32) -> Self {
        Self {
            budget,
            limit,
            used,
            depth,
        }
    }

    /// Returns the contract error code.
    #[must_use]
    pub const fn code() -> &'static str {
        "policy.budget_exhausted"
    }
}

impl fmt::Display for BudgetFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "The {} budget is exhausted: limit={}, used={}, depth={}.",
            self.budget, self.limit, self.used, self.depth
        )
    }
}

/// The stable reason attached to one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReasonCode {
    /// The expression is true.
    Allow,
    /// The expression is false.
    Deny,
    /// A path names a field that the row does not bind.
    UnknownField,
    /// An operand has the wrong type for the operator.
    NotTotal,
    /// A budget stopped the evaluation.
    BudgetExhausted,
    /// The expression contains a regex node.
    RegexNotSupported,
}

impl ReasonCode {
    /// Returns the stable reason text.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Allow => "policy.allow",
            Self::Deny => "policy.deny",
            Self::UnknownField => "policy.unknown_field",
            Self::NotTotal => "policy.not_total",
            Self::BudgetExhausted => "policy.budget_exhausted",
            Self::RegexNotSupported => "policy.regex_not_supported",
        }
    }

    /// Returns the stable encoding tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::Deny => 1,
            Self::UnknownField => 2,
            Self::NotTotal => 3,
            Self::BudgetExhausted => 4,
            Self::RegexNotSupported => 5,
        }
    }
}

impl fmt::Display for ReasonCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.text())
    }
}

/// An error from policy construction, validation, or decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// The AST contains a regex node. Policy v1 has no regex engine.
    RegexNotSupported {
        /// The rejected pattern.
        pattern: String,
    },
    /// The AST exceeds the static node or depth bound.
    Unbounded {
        /// The measured node count.
        nodes: u64,
        /// The measured depth.
        depth: u64,
        /// The maximum node count.
        max_nodes: u64,
        /// The maximum depth.
        max_depth: u64,
    },
    /// A path names a field that the schema does not declare.
    UnknownFieldPath {
        /// The undeclared path in dotted form.
        path: String,
    },
    /// The AST version is not supported.
    AstUnsupported {
        /// The unsupported revision.
        version: u16,
    },
    /// The canonical bytes are not valid for this AST version.
    Parse {
        /// The byte offset of the first error.
        offset: u64,
        /// A short error detail.
        detail: &'static str,
    },
    /// The value is not a valid policy input.
    NotTotal {
        /// A short error detail.
        detail: &'static str,
    },
}

impl PolicyError {
    /// Returns the contract error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::RegexNotSupported { .. } => "policy.regex_not_supported",
            Self::Unbounded { .. } => "manifest.rule_unbounded",
            Self::UnknownFieldPath { .. } => "manifest.rule_unknown_path",
            Self::AstUnsupported { .. } => "policy.ast_unsupported",
            Self::Parse { .. } => "policy.parse_error",
            Self::NotTotal { .. } => "policy.not_total",
        }
    }

    /// Returns the checked contract code, when the registry has it.
    #[must_use]
    pub fn contract_code(&self) -> Option<ErrorCode> {
        ErrorCode::parse(self.code())
    }
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RegexNotSupported { pattern } => {
                write!(formatter, "Policy v1 does not support regex: {pattern}.")
            }
            Self::Unbounded {
                nodes,
                depth,
                max_nodes,
                max_depth,
            } => write!(
                formatter,
                "The policy is unbounded: nodes={nodes}/{max_nodes}, depth={depth}/{max_depth}."
            ),
            Self::UnknownFieldPath { path } => {
                write!(formatter, "The policy path {path} is not declared.")
            }
            Self::AstUnsupported { version } => {
                write!(
                    formatter,
                    "The policy AST revision {version} is not supported."
                )
            }
            Self::Parse { offset, detail } => {
                write!(
                    formatter,
                    "The policy bytes are invalid at offset {offset}: {detail}"
                )
            }
            Self::NotTotal { detail } => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for PolicyError {}
