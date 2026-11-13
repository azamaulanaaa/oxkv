//! The canonical policy AST v1, its budget, and its content address.
//!
//! The AST is the execution surface. A source syntax may map to this AST, but
//! the evaluator never reads source text.
//!
//! The canonical bytes use fixed little-endian integers. A membership set is
//! sorted and deduplicated, so equal sets produce equal bytes. The policy
//! version is the domain-separated `BLAKE3` hash of those bytes.

use std::collections::BTreeSet;

use oxkv_contracts::version::{DigestDomain, Hash32};

pub use oxkv_contracts::version::{PolicyAstVersion, PolicyVersion};

use crate::error::PolicyError;

/// The current policy AST revision.
pub const POLICY_AST_VERSION: PolicyAstVersion = PolicyAstVersion::new(1);

/// The maximum number of expression nodes in one policy.
pub const MAX_EXPR_NODES: u64 = 4096;

/// The maximum expression depth in one policy.
pub const MAX_EXPR_DEPTH: u64 = 128;

const POLICY_MAGIC: &[u8] = b"oxkv/policy-ast/v1\x00";

/// A literal value. Policy v1 has no floating point.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Literal {
    /// A boolean value.
    Bool(bool),
    /// A signed 64-bit integer.
    I64(i64),
    /// An unsigned 64-bit integer.
    U64(u64),
    /// A UTF-8 text value.
    Text(String),
    /// An opaque byte value.
    Bytes(Vec<u8>),
}

impl Literal {
    /// Returns the stable type name.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::I64(_) => "i64",
            Self::U64(_) => "u64",
            Self::Text(_) => "text",
            Self::Bytes(_) => "bytes",
        }
    }

    /// Returns the step weight of the value.
    #[must_use]
    pub fn weight(&self) -> u64 {
        match self {
            Self::Bool(_) | Self::I64(_) | Self::U64(_) => 1,
            Self::Text(value) => len_u64(value.len()),
            Self::Bytes(value) => len_u64(value.len()),
        }
    }

    /// Returns the stable encoding tag.
    const fn tag(&self) -> u8 {
        match self {
            Self::Bool(_) => 0,
            Self::I64(_) => 1,
            Self::U64(_) => 2,
            Self::Text(_) => 3,
            Self::Bytes(_) => 4,
        }
    }
}

/// The row side that a path reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PathRoot {
    /// The stored value. Read scope binds it to the same row.
    Before,
    /// The proposed value. Read scope binds it to the same row.
    After,
}

impl PathRoot {
    /// Returns the stable root name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }

    /// Returns the stable encoding tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Before => 0,
            Self::After => 1,
        }
    }
}

/// A path from one row side to one declared field.
///
/// Policy v1 facts are flat. A dotted field identity is one declared field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Path {
    root: PathRoot,
    segments: Vec<String>,
}

impl Path {
    /// Creates a path with one segment.
    #[must_use]
    pub fn field(root: PathRoot, name: impl Into<String>) -> Self {
        Self {
            root,
            segments: vec![name.into()],
        }
    }

    /// Creates a path with one or more segments.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::NotTotal`] when `segments` is empty.
    pub fn new(root: PathRoot, segments: Vec<String>) -> Result<Self, PolicyError> {
        if segments.is_empty() {
            return Err(PolicyError::NotTotal {
                detail: "A path needs at least one segment.",
            });
        }
        Ok(Self { root, segments })
    }

    /// Returns the row side.
    #[must_use]
    pub const fn root(&self) -> PathRoot {
        self.root
    }

    /// Returns the path segments.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Returns the joined field identity.
    #[must_use]
    pub fn dotted(&self) -> String {
        self.segments.join(".")
    }

    /// Returns the full dotted path with the row side.
    #[must_use]
    pub fn display_path(&self) -> String {
        format!("{}.{}", self.root.name(), self.dotted())
    }

    /// Returns the step weight of the path.
    #[must_use]
    pub fn weight(&self) -> u64 {
        self.segments.iter().fold(0_u64, |total, segment| {
            total.saturating_add(len_u64(segment.len()))
        })
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.root.tag());
        write_count(out, len_u64(self.segments.len()));
        for segment in &self.segments {
            write_text(out, segment);
        }
    }
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CompareOp {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Less than.
    Lt,
    /// Less than or equal.
    Le,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Ge,
}

impl CompareOp {
    /// Returns the stable operator name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
        }
    }

    /// Returns the stable encoding tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Eq => 0,
            Self::Ne => 1,
            Self::Lt => 2,
            Self::Le => 3,
            Self::Gt => 4,
            Self::Ge => 5,
        }
    }
}

/// One canonical policy expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    /// A literal value.
    Literal(Literal),
    /// A field value.
    Path(Path),
    /// True when the field exists.
    Exists(Path),
    /// A comparison of two expressions.
    Compare {
        /// The comparison operator.
        op: CompareOp,
        /// The left operand.
        left: Box<Self>,
        /// The right operand.
        right: Box<Self>,
    },
    /// Logical and. Evaluation is left to right with short circuit.
    And(Box<Self>, Box<Self>),
    /// Logical or. Evaluation is left to right with short circuit.
    Or(Box<Self>, Box<Self>),
    /// Logical not.
    Not(Box<Self>),
    /// Membership in a literal set.
    In {
        /// The tested expression.
        value: Box<Self>,
        /// The literal set. Order does not change the canonical bytes.
        set: Vec<Literal>,
    },
    /// A regex pattern. Policy v1 rejects this node.
    Regex {
        /// The rejected pattern.
        pattern: String,
    },
}

impl Expr {
    /// Creates a path expression.
    #[must_use]
    pub fn path(root: PathRoot, field: impl Into<String>) -> Self {
        Self::Path(Path::field(root, field))
    }

    /// Returns the step cost of one node.
    ///
    /// The cost is one step plus the node payload weight. A membership set
    /// adds one step per member.
    #[must_use]
    pub fn step_cost(&self) -> u64 {
        match self {
            Self::Literal(literal) => 1_u64.saturating_add(literal.weight()),
            Self::Path(path) | Self::Exists(path) => 1_u64.saturating_add(path.weight()),
            Self::Compare { .. } | Self::And(..) | Self::Or(..) | Self::Not(..) => 1,
            Self::In { set, .. } => set.iter().fold(
                1_u64.saturating_add(len_u64(set.len())),
                |total, literal| total.saturating_add(literal.weight()),
            ),
            Self::Regex { pattern } => 1_u64.saturating_add(len_u64(pattern.len())),
        }
    }

    /// Returns the first regex pattern in the tree.
    #[must_use]
    pub fn first_regex(&self) -> Option<&str> {
        let mut stack = vec![self];
        while let Some(expr) = stack.pop() {
            match expr {
                Self::Regex { pattern } => return Some(pattern),
                Self::Compare { left, right, .. }
                | Self::And(left, right)
                | Self::Or(left, right) => {
                    stack.push(left);
                    stack.push(right);
                }
                Self::Not(inner) | Self::In { value: inner, .. } => stack.push(inner),
                Self::Literal(_) | Self::Path(_) | Self::Exists(_) => {}
            }
        }
        None
    }

    /// Returns the node count and depth of the tree.
    #[must_use]
    pub fn measure(&self) -> (u64, u64) {
        let mut nodes = 0_u64;
        let mut max_depth = 0_u64;
        let mut stack = vec![(self, 1_u64)];
        while let Some((expr, depth)) = stack.pop() {
            nodes = nodes.saturating_add(1);
            max_depth = max_depth.max(depth);
            let child_depth = depth.saturating_add(1);
            match expr {
                Self::Literal(_) | Self::Path(_) | Self::Exists(_) | Self::Regex { .. } => {}
                Self::Compare { left, right, .. }
                | Self::And(left, right)
                | Self::Or(left, right) => {
                    stack.push((left, child_depth));
                    stack.push((right, child_depth));
                }
                Self::Not(inner) => stack.push((inner, child_depth)),
                Self::In { value, set } => {
                    stack.push((value, child_depth));
                    nodes = nodes.saturating_add(len_u64(set.len()));
                    max_depth = max_depth.max(child_depth);
                }
            }
        }
        (nodes, max_depth)
    }

    /// Returns true when the tree declares a field that the schema lacks.
    fn validate_fields(&self, schema: &FieldSchema) -> Result<(), PolicyError> {
        let mut stack = vec![self];
        while let Some(expr) = stack.pop() {
            match expr {
                Self::Path(path) | Self::Exists(path) => {
                    if !schema.contains(&path.dotted()) {
                        return Err(PolicyError::UnknownFieldPath {
                            path: path.display_path(),
                        });
                    }
                }
                Self::Compare { left, right, .. }
                | Self::And(left, right)
                | Self::Or(left, right) => {
                    stack.push(left);
                    stack.push(right);
                }
                Self::Not(inner) | Self::In { value: inner, .. } => stack.push(inner),
                Self::Literal(_) | Self::Regex { .. } => {}
            }
        }
        Ok(())
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Literal(literal) => {
                out.push(0);
                encode_literal(literal, out);
            }
            Self::Path(path) => {
                out.push(1);
                path.encode(out);
            }
            Self::Exists(path) => {
                out.push(2);
                path.encode(out);
            }
            Self::Compare { op, left, right } => {
                out.push(3);
                out.push(op.tag());
                left.encode(out);
                right.encode(out);
            }
            Self::And(left, right) => {
                out.push(4);
                left.encode(out);
                right.encode(out);
            }
            Self::Or(left, right) => {
                out.push(5);
                left.encode(out);
                right.encode(out);
            }
            Self::Not(inner) => {
                out.push(6);
                inner.encode(out);
            }
            Self::In { value, set } => {
                out.push(7);
                value.encode(out);
                let mut sorted: Vec<&Literal> = set.iter().collect();
                sorted.sort_unstable();
                sorted.dedup();
                write_count(out, len_u64(sorted.len()));
                for literal in sorted {
                    encode_literal(literal, out);
                }
            }
            Self::Regex { pattern } => {
                out.push(8);
                write_text(out, pattern);
            }
        }
    }
}

/// The identity of one rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleId(String);

impl RuleId {
    /// Creates a rule identity.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::NotTotal`] when `value` is empty.
    pub fn new(value: impl Into<String>) -> Result<Self, PolicyError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PolicyError::NotTotal {
                detail: "A rule identity must not be empty.",
            });
        }
        Ok(Self(value))
    }

    /// Returns the rule identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The budgets for one policy evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Budget {
    steps: u64,
    depth: u32,
    arena_bytes: u64,
}

impl Budget {
    /// Creates a budget.
    #[must_use]
    pub const fn new(steps: u64, depth: u32, arena_bytes: u64) -> Self {
        Self {
            steps,
            depth,
            arena_bytes,
        }
    }

    /// Returns the standard budget.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            steps: 65_536,
            depth: 64,
            arena_bytes: 1_048_576,
        }
    }

    /// Returns the step limit.
    #[must_use]
    pub const fn steps(self) -> u64 {
        self.steps
    }

    /// Returns the depth limit.
    #[must_use]
    pub const fn depth(self) -> u32 {
        self.depth
    }

    /// Returns the memory arena limit in bytes.
    #[must_use]
    pub const fn arena_bytes(self) -> u64 {
        self.arena_bytes
    }
}

/// The declared fields of one table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldSchema {
    fields: BTreeSet<String>,
}

impl FieldSchema {
    /// Creates an empty schema.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a schema from declared field names.
    #[must_use]
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut schema = Self::new();
        for name in names {
            schema.declare(name);
        }
        schema
    }

    /// Declares one field.
    pub fn declare(&mut self, name: impl Into<String>) {
        self.fields.insert(name.into());
    }

    /// Returns true when the schema declares the field.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.fields.contains(name)
    }
}

/// One canonical policy. It holds an AST, a rule identity, and a cost model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    ast_version: PolicyAstVersion,
    rule: RuleId,
    budget: Budget,
    expr: Expr,
    version: PolicyVersion,
}

impl Policy {
    /// Creates a policy and computes its content address.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::RegexNotSupported`] when the tree has a regex
    /// node. Returns [`PolicyError::Unbounded`] when the tree exceeds the
    /// static node or depth bound.
    pub fn new(rule: RuleId, budget: Budget, expr: Expr) -> Result<Self, PolicyError> {
        let (nodes, depth) = expr.measure();
        if nodes > MAX_EXPR_NODES || depth > MAX_EXPR_DEPTH {
            return Err(PolicyError::Unbounded {
                nodes,
                depth,
                max_nodes: MAX_EXPR_NODES,
                max_depth: MAX_EXPR_DEPTH,
            });
        }
        if let Some(pattern) = expr.first_regex() {
            return Err(PolicyError::RegexNotSupported {
                pattern: pattern.to_owned(),
            });
        }
        let bytes = encode_policy(&rule, budget, &expr);
        let hash = Hash32::of_domain(DigestDomain::Policy, &bytes);
        Ok(Self {
            ast_version: POLICY_AST_VERSION,
            rule,
            budget,
            expr,
            version: PolicyVersion::from_bytes(*hash.as_bytes()),
        })
    }

    /// Decodes and checks canonical policy bytes.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::Parse`] when the bytes are malformed or not
    /// canonical. Returns the errors of [`Policy::new`] for a rejected tree.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, PolicyError> {
        let mut decoder = Decoder::new(bytes);
        decoder.expect_magic()?;
        let revision = decoder.read_u16()?;
        if revision != POLICY_AST_VERSION.revision() {
            return Err(PolicyError::AstUnsupported { version: revision });
        }
        let rule = RuleId::new(decoder.read_text()?)?;
        let steps = decoder.read_u64()?;
        let depth = decoder.read_u32()?;
        let arena_bytes = decoder.read_u64()?;
        let expr = decoder.read_expr(1)?;
        if !decoder.is_empty() {
            return Err(decoder.error("The policy bytes have trailing data."));
        }
        let policy = Self::new(rule, Budget::new(steps, depth, arena_bytes), expr)?;
        if policy.canonical_bytes() != bytes {
            return Err(PolicyError::Parse {
                offset: 0,
                detail: "The policy bytes are not canonical.",
            });
        }
        Ok(policy)
    }

    /// Returns the AST revision.
    #[must_use]
    pub const fn ast_version(&self) -> PolicyAstVersion {
        self.ast_version
    }

    /// Returns the rule identity.
    #[must_use]
    pub const fn rule(&self) -> &RuleId {
        &self.rule
    }

    /// Returns the budget.
    #[must_use]
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    /// Returns the expression tree.
    #[must_use]
    pub const fn expr(&self) -> &Expr {
        &self.expr
    }

    /// Returns the policy version.
    #[must_use]
    pub const fn version(&self) -> PolicyVersion {
        self.version
    }

    /// Returns the canonical AST bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        encode_policy(&self.rule, self.budget, &self.expr)
    }

    /// Checks every path against a declared field schema.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError::UnknownFieldPath`] for the first undeclared
    /// path.
    pub fn validate_fields(&self, schema: &FieldSchema) -> Result<(), PolicyError> {
        self.expr.validate_fields(schema)
    }
}

fn encode_policy(rule: &RuleId, budget: Budget, expr: &Expr) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(POLICY_MAGIC);
    out.extend_from_slice(&POLICY_AST_VERSION.revision().to_le_bytes());
    write_text(&mut out, rule.as_str());
    write_count(&mut out, budget.steps());
    out.extend_from_slice(&budget.depth().to_le_bytes());
    write_count(&mut out, budget.arena_bytes());
    expr.encode(&mut out);
    out
}

fn encode_literal(literal: &Literal, out: &mut Vec<u8>) {
    out.push(literal.tag());
    match literal {
        Literal::Bool(value) => out.push(u8::from(*value)),
        Literal::I64(value) => out.extend_from_slice(&value.to_le_bytes()),
        Literal::U64(value) => out.extend_from_slice(&value.to_le_bytes()),
        Literal::Text(value) => write_text(out, value),
        Literal::Bytes(value) => write_bytes(out, value),
    }
}

fn write_count(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_text(out: &mut Vec<u8>, value: &str) {
    write_bytes(out, value.as_bytes());
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) {
    write_count(out, len_u64(value.len()));
    out.extend_from_slice(value);
}

fn len_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    const fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn error(&self, detail: &'static str) -> PolicyError {
        PolicyError::Parse {
            offset: len_u64(self.position),
            detail,
        }
    }

    fn expect_magic(&mut self) -> Result<(), PolicyError> {
        if self.bytes.len() < POLICY_MAGIC.len()
            || &self.bytes[..POLICY_MAGIC.len()] != POLICY_MAGIC
        {
            return Err(self.error("The policy magic is missing."));
        }
        self.position = POLICY_MAGIC.len();
        Ok(())
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], PolicyError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| self.error("The policy bytes overflow."))?;
        if end > self.bytes.len() {
            return Err(self.error("The policy bytes end early."));
        }
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, PolicyError> {
        let slice = self.take(1)?;
        Ok(slice[0])
    }

    fn read_u16(&mut self) -> Result<u16, PolicyError> {
        let slice = self.take(2)?;
        let mut bytes = [0_u8; 2];
        bytes.copy_from_slice(slice);
        Ok(u16::from_le_bytes(bytes))
    }

    fn read_u32(&mut self) -> Result<u32, PolicyError> {
        let slice = self.take(4)?;
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(slice);
        Ok(u32::from_le_bytes(bytes))
    }

    fn read_u64(&mut self) -> Result<u64, PolicyError> {
        let slice = self.take(8)?;
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(slice);
        Ok(u64::from_le_bytes(bytes))
    }

    fn read_length(&mut self) -> Result<usize, PolicyError> {
        let value = self.read_u64()?;
        let length =
            usize::try_from(value).map_err(|_| self.error("The length is out of range."))?;
        if length > self.remaining() {
            return Err(self.error("The length exceeds the remaining bytes."));
        }
        Ok(length)
    }

    fn read_bytes(&mut self) -> Result<Vec<u8>, PolicyError> {
        let length = self.read_length()?;
        Ok(self.take(length)?.to_vec())
    }

    fn read_text(&mut self) -> Result<String, PolicyError> {
        let bytes = self.read_bytes()?;
        String::from_utf8(bytes).map_err(|_| self.error("The text is not valid UTF-8."))
    }

    fn read_path(&mut self) -> Result<Path, PolicyError> {
        let root = match self.read_u8()? {
            0 => PathRoot::Before,
            1 => PathRoot::After,
            _ => return Err(self.error("The path root tag is unknown.")),
        };
        let count = self.read_length()?;
        if count == 0 {
            return Err(self.error("A path needs at least one segment."));
        }
        let mut segments = Vec::with_capacity(count);
        for _ in 0..count {
            segments.push(self.read_text()?);
        }
        Path::new(root, segments)
    }

    fn read_literal(&mut self) -> Result<Literal, PolicyError> {
        match self.read_u8()? {
            0 => match self.read_u8()? {
                0 => Ok(Literal::Bool(false)),
                1 => Ok(Literal::Bool(true)),
                _ => Err(self.error("The boolean literal is invalid.")),
            },
            1 => Ok(Literal::I64(i64::from_le_bytes(self.read_array()?))),
            2 => Ok(Literal::U64(u64::from_le_bytes(self.read_array()?))),
            3 => Ok(Literal::Text(self.read_text()?)),
            4 => Ok(Literal::Bytes(self.read_bytes()?)),
            _ => Err(self.error("The literal tag is unknown.")),
        }
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], PolicyError> {
        let slice = self.take(N)?;
        let mut bytes = [0_u8; N];
        bytes.copy_from_slice(slice);
        Ok(bytes)
    }

    fn read_expr(&mut self, depth: u64) -> Result<Expr, PolicyError> {
        if depth > MAX_EXPR_DEPTH {
            return Err(self.error("The expression depth is too large."));
        }
        let child_depth = depth.saturating_add(1);
        match self.read_u8()? {
            0 => Ok(Expr::Literal(self.read_literal()?)),
            1 => Ok(Expr::Path(self.read_path()?)),
            2 => Ok(Expr::Exists(self.read_path()?)),
            3 => {
                let tag = self.read_u8()?;
                let op = match tag {
                    0 => CompareOp::Eq,
                    1 => CompareOp::Ne,
                    2 => CompareOp::Lt,
                    3 => CompareOp::Le,
                    4 => CompareOp::Gt,
                    5 => CompareOp::Ge,
                    _ => return Err(self.error("The comparison tag is unknown.")),
                };
                let left = Box::new(self.read_expr(child_depth)?);
                let right = Box::new(self.read_expr(child_depth)?);
                Ok(Expr::Compare { op, left, right })
            }
            4 => Ok(Expr::And(
                Box::new(self.read_expr(child_depth)?),
                Box::new(self.read_expr(child_depth)?),
            )),
            5 => Ok(Expr::Or(
                Box::new(self.read_expr(child_depth)?),
                Box::new(self.read_expr(child_depth)?),
            )),
            6 => Ok(Expr::Not(Box::new(self.read_expr(child_depth)?))),
            7 => {
                let value = Box::new(self.read_expr(child_depth)?);
                let count = self.read_length()?;
                if len_u64(count) > MAX_EXPR_NODES {
                    return Err(self.error("The membership set is too large."));
                }
                let mut set = Vec::with_capacity(count);
                for _ in 0..count {
                    set.push(self.read_literal()?);
                }
                Ok(Expr::In { value, set })
            }
            8 => Ok(Expr::Regex {
                pattern: self.read_text()?,
            }),
            _ => Err(self.error("The expression tag is unknown.")),
        }
    }
}
