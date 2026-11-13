//! Policy and search compile guards.
//!
//! Policy evaluation uses named step budgets. The 1.0 policy and search
//! languages do not include regular expressions.

use core::fmt;

use crate::error::SecurityError;
use crate::secret::SecretReference;

/// A bounded policy step budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepBudget {
    limit: u64,
    used: u64,
}

impl StepBudget {
    /// Creates a step budget.
    #[must_use]
    pub const fn new(limit: u64) -> Self {
        Self { limit, used: 0 }
    }

    /// Returns the step limit.
    #[must_use]
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the number of charged steps.
    #[must_use]
    pub const fn used(self) -> u64 {
        self.used
    }

    /// Returns the number of steps that remain.
    #[must_use]
    pub const fn remaining(self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    /// Charges one number of steps.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::PolicyBudgetExhausted`] when the charge passes
    /// the limit. The used count then equals the limit.
    pub fn charge(&mut self, steps: u64) -> Result<(), SecurityError> {
        let next = self
            .used
            .checked_add(steps)
            .ok_or(SecurityError::PolicyBudgetExhausted)?;
        if next > self.limit {
            self.used = self.limit;
            return Err(SecurityError::PolicyBudgetExhausted);
        }
        self.used = next;
        Ok(())
    }
}

/// The named policy limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PolicyLimits {
    /// The step budget for one evaluation.
    pub step_budget: u64,
    /// The depth budget for one evaluation.
    pub depth_budget: u64,
    /// The fixed memory arena size in bytes.
    pub memory_arena: u64,
}

impl PolicyLimits {
    /// Creates the named policy limits.
    #[must_use]
    pub const fn new(step_budget: u64, depth_budget: u64, memory_arena: u64) -> Self {
        Self {
            step_budget,
            depth_budget,
            memory_arena,
        }
    }
}

/// One node of the policy AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyNode {
    /// A constant predicate.
    Constant(bool),
    /// A row field path.
    RowField(String),
    /// A reference to a key identifier.
    SecretReference(SecretReference),
    /// A regular expression. The 1.0 compiler rejects it.
    Regex(String),
}

/// A policy AST.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyAst {
    /// The AST nodes.
    pub nodes: Vec<PolicyNode>,
}

impl PolicyAst {
    /// Creates an empty AST.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// A compiled policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPolicy {
    ast: PolicyAst,
    limits: PolicyLimits,
}

impl CompiledPolicy {
    /// Returns the policy AST.
    #[must_use]
    pub const fn ast(&self) -> &PolicyAst {
        &self.ast
    }

    /// Returns the policy limits.
    #[must_use]
    pub const fn limits(&self) -> PolicyLimits {
        self.limits
    }

    /// Evaluates the policy against a step budget.
    ///
    /// The function charges one step per node. It returns the conjunction of
    /// the constant nodes.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::PolicyBudgetExhausted`] when the budget cannot
    /// pay for every node.
    pub fn evaluate(&self, budget: &mut StepBudget) -> Result<bool, SecurityError> {
        let mut verdict = true;
        for node in &self.ast.nodes {
            budget.charge(1)?;
            if let PolicyNode::Constant(value) = node {
                verdict &= *value;
            }
        }
        Ok(verdict)
    }
}

/// Compiles one policy AST.
///
/// # Errors
///
/// Returns [`SecurityError::RegexNotSupported`] when a node uses a regular
/// expression. Returns [`SecurityError::PolicyBudgetExhausted`] when the AST
/// passes the depth budget.
pub fn compile_policy(
    ast: &PolicyAst,
    limits: PolicyLimits,
) -> Result<CompiledPolicy, SecurityError> {
    if ast
        .nodes
        .iter()
        .any(|node| matches!(node, PolicyNode::Regex(_)))
    {
        return Err(SecurityError::RegexNotSupported);
    }
    let depth = u64::try_from(ast.nodes.len()).unwrap_or(u64::MAX);
    if depth > limits.depth_budget {
        return Err(SecurityError::PolicyBudgetExhausted);
    }
    Ok(CompiledPolicy {
        ast: ast.clone(),
        limits,
    })
}

/// One node of a 1.0 search query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchNode {
    /// A term.
    Term(String),
    /// A key prefix.
    Prefix(String),
    /// A phrase.
    Phrase(String),
    /// A regular expression. The 1.0 compiler rejects it.
    Regex(String),
}

/// A search query.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    /// The query nodes.
    pub nodes: Vec<SearchNode>,
}

impl SearchQuery {
    /// Creates an empty query.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// A compiled search query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledSearch {
    query: SearchQuery,
}

impl CompiledSearch {
    /// Returns the search query.
    #[must_use]
    pub const fn query(&self) -> &SearchQuery {
        &self.query
    }
}

/// Compiles one search query.
///
/// # Errors
///
/// Returns [`SecurityError::RegexNotSupported`] when a node uses a regular
/// expression.
pub fn compile_search(query: &SearchQuery) -> Result<CompiledSearch, SecurityError> {
    if query
        .nodes
        .iter()
        .any(|node| matches!(node, SearchNode::Regex(_)))
    {
        return Err(SecurityError::RegexNotSupported);
    }
    Ok(CompiledSearch {
        query: query.clone(),
    })
}

impl fmt::Display for StepBudget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} of {}", self.used, self.limit)
    }
}
