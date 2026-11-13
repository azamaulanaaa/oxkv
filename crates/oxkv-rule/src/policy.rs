//! Immutable policy content and compilation.
//!
//! One policy version covers the policy identity, the AST version, the rule
//! bodies, the rule read paths, and the cost model. The version is the
//! `BLAKE3` hash of the canonical bytes. The content has no mutation method.
//! A body change creates a new value and a new version.

use std::collections::BTreeMap;
use std::collections::btree_map::Iter;

use oxkv_contracts::version::{Hash32, PolicyAstVersion, PolicyVersion};

use crate::canonical;
use crate::error::RuleError;
use crate::manifest::{CostBound, FactReadSet, RuleId, RuleManifest, RuleMetadata};

/// The only policy AST version that this crate executes.
pub const SUPPORTED_POLICY_AST_VERSION: PolicyAstVersion = PolicyAstVersion::new(1);

/// One policy identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PolicyId(String);

impl PolicyId {
    /// Parses one policy identity.
    ///
    /// The grammar is `[a-z][a-z0-9_.-]*`.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::CompileFailed`] when the text does not match the
    /// grammar.
    pub fn parse(text: &str) -> Result<Self, RuleError> {
        crate::manifest::validate_identifier("policy", text)?;
        Ok(Self(text.to_owned()))
    }

    /// Returns the identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PolicyId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The canonical AST bytes of one rule and its static cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleBody {
    ast: Vec<u8>,
    read_paths: std::collections::BTreeSet<String>,
    steps: u64,
    depth: u32,
}

impl RuleBody {
    /// Creates one rule body.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::PolicyParse`] when the AST bytes are empty or a
    /// read path is empty.
    pub fn new(
        ast: Vec<u8>,
        read_paths: std::collections::BTreeSet<String>,
        steps: u64,
        depth: u32,
    ) -> Result<Self, RuleError> {
        if ast.is_empty() {
            return Err(RuleError::PolicyParse {
                detail: "The rule AST is empty.".to_owned(),
            });
        }
        if read_paths.iter().any(String::is_empty) {
            return Err(RuleError::PolicyParse {
                detail: "A rule read path is empty.".to_owned(),
            });
        }
        Ok(Self {
            ast,
            read_paths,
            steps,
            depth,
        })
    }

    /// Returns the canonical AST bytes.
    #[must_use]
    pub fn ast(&self) -> &[u8] {
        &self.ast
    }

    /// Returns the read paths in canonical order.
    pub fn read_paths(&self) -> impl Iterator<Item = &str> {
        self.read_paths.iter().map(String::as_str)
    }

    /// Returns true when the body reads the path.
    #[must_use]
    pub fn reads_path(&self, path: &str) -> bool {
        self.read_paths.contains(path)
    }

    /// Returns the static step count.
    #[must_use]
    pub const fn steps(&self) -> u64 {
        self.steps
    }

    /// Returns the static depth.
    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }
}

/// One immutable policy content object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyContent {
    policy_id: PolicyId,
    ast_version: PolicyAstVersion,
    rules: BTreeMap<RuleId, RuleBody>,
    budget: CostBound,
    version: PolicyVersion,
}

impl<'a> IntoIterator for &'a PolicyContent {
    type Item = (&'a RuleId, &'a RuleBody);
    type IntoIter = Iter<'a, RuleId, RuleBody>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl PolicyContent {
    /// Creates one policy content object.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::PolicyParse`] when the content has no rule or a
    /// rule body is invalid.
    pub fn new(
        policy_id: PolicyId,
        ast_version: PolicyAstVersion,
        rules: BTreeMap<RuleId, RuleBody>,
        budget: CostBound,
    ) -> Result<Self, RuleError> {
        if rules.is_empty() {
            return Err(RuleError::PolicyParse {
                detail: "The policy has no rule body.".to_owned(),
            });
        }
        let mut content = Self {
            policy_id,
            ast_version,
            rules,
            budget,
            version: PolicyVersion::from_bytes([0; 32]),
        };
        let version = Hash32::of(&content.canonical_bytes());
        content.version = PolicyVersion::from_bytes(*version.as_bytes());
        Ok(content)
    }

    /// Returns the policy identity.
    #[must_use]
    pub const fn policy_id(&self) -> &PolicyId {
        &self.policy_id
    }

    /// Returns the AST version.
    #[must_use]
    pub const fn ast_version(&self) -> PolicyAstVersion {
        self.ast_version
    }

    /// Returns the content address.
    #[must_use]
    pub const fn version(&self) -> PolicyVersion {
        self.version
    }

    /// Returns the cost model.
    #[must_use]
    pub const fn budget(&self) -> CostBound {
        self.budget
    }

    /// Returns one rule body.
    #[must_use]
    pub fn rule(&self, id: &RuleId) -> Option<&RuleBody> {
        self.rules.get(id)
    }

    /// Iterates over the bodies in canonical rule identity order.
    pub fn iter(&self) -> Iter<'_, RuleId, RuleBody> {
        self.rules.iter()
    }

    /// Returns the rule count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Returns true when the content has no rule.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Returns the canonical bytes of the policy.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        canonical::push_str(&mut bytes, self.policy_id.as_str());
        canonical::push_u64(&mut bytes, u64::from(self.ast_version.revision()));
        canonical::push_u64(&mut bytes, self.budget.max_steps());
        canonical::push_u32(&mut bytes, self.budget.max_depth());
        canonical::push_u64(&mut bytes, self.rules.len() as u64);
        for (id, body) in &self.rules {
            canonical::push_str(&mut bytes, id.as_str());
            canonical::push_bytes(&mut bytes, body.ast());
            canonical::push_u64(&mut bytes, body.steps());
            canonical::push_u32(&mut bytes, body.depth());
            canonical::push_u64(&mut bytes, body.read_paths.len() as u64);
            for path in body.read_paths() {
                canonical::push_str(&mut bytes, path);
            }
        }
        bytes
    }

    /// Returns a new content object with one changed rule body.
    ///
    /// The original object does not change.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::CompileFailed`] when the rule identity is absent.
    pub fn with_rule_body(&self, id: &RuleId, body: RuleBody) -> Result<Self, RuleError> {
        if !self.rules.contains_key(id) {
            return Err(RuleError::CompileFailed {
                detail: format!("The policy has no rule {id}."),
            });
        }
        let mut rules = self.rules.clone();
        rules.insert(id.clone(), body);
        Self::new(self.policy_id.clone(), self.ast_version, rules, self.budget)
    }

    /// Compiles the content against one rule manifest.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::AstUnsupported`] for an unknown AST version.
    /// Returns [`RuleError::CompileFailed`] for a missing rule body.
    /// Returns [`RuleError::RuleUnknownPath`] for an undeclared read path.
    /// Returns [`RuleError::RuleUnbounded`] for a cost over the bound.
    pub fn compile(&self, manifest: &RuleManifest) -> Result<CompiledPolicy, RuleError> {
        if self.ast_version != SUPPORTED_POLICY_AST_VERSION {
            return Err(RuleError::AstUnsupported {
                ast_version: self.ast_version.to_string(),
            });
        }
        let mut compiled = BTreeMap::new();
        for (id, metadata) in manifest {
            let body = self.rules.get(id).ok_or_else(|| RuleError::CompileFailed {
                detail: format!("The policy has no body for the rule {id}."),
            })?;
            for path in body.read_paths() {
                if !metadata.selector.allows_path(path) {
                    return Err(RuleError::RuleUnknownPath {
                        rule_id: id.to_string(),
                        path: path.to_owned(),
                    });
                }
            }
            check_cost(id, metadata, body, self.budget)?;
            compiled.insert(
                id.clone(),
                CompiledRule {
                    metadata: metadata.clone(),
                    body: body.clone(),
                },
            );
        }
        for id in self.rules.keys() {
            if manifest.rule(id).is_none() {
                return Err(RuleError::CompileFailed {
                    detail: format!("The manifest has no metadata for the rule {id}."),
                });
            }
        }
        Ok(CompiledPolicy {
            version: self.version,
            policy_id: self.policy_id.clone(),
            budget: self.budget,
            rules: compiled,
        })
    }
}

fn check_cost(
    id: &RuleId,
    metadata: &RuleMetadata,
    body: &RuleBody,
    budget: CostBound,
) -> Result<(), RuleError> {
    if metadata.cost.max_steps() == 0
        || metadata.cost.max_depth() == 0
        || budget.max_steps() == 0
        || budget.max_depth() == 0
        || body.steps() > metadata.cost.max_steps()
        || body.depth() > metadata.cost.max_depth()
        || body.steps() > budget.max_steps()
        || body.depth() > budget.max_depth()
    {
        return Err(RuleError::RuleUnbounded {
            rule_id: id.to_string(),
        });
    }
    Ok(())
}

/// One compiled rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRule {
    metadata: RuleMetadata,
    body: RuleBody,
}

impl CompiledRule {
    /// Returns the manifest metadata.
    #[must_use]
    pub const fn metadata(&self) -> &RuleMetadata {
        &self.metadata
    }

    /// Returns the rule body.
    #[must_use]
    pub const fn body(&self) -> &RuleBody {
        &self.body
    }
}

/// One policy that passed compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPolicy {
    version: PolicyVersion,
    policy_id: PolicyId,
    budget: CostBound,
    rules: BTreeMap<RuleId, CompiledRule>,
}

impl<'a> IntoIterator for &'a CompiledPolicy {
    type Item = (&'a RuleId, &'a CompiledRule);
    type IntoIter = Iter<'a, RuleId, CompiledRule>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl CompiledPolicy {
    /// Returns the content address.
    #[must_use]
    pub const fn version(&self) -> PolicyVersion {
        self.version
    }

    /// Returns the policy identity.
    #[must_use]
    pub const fn policy_id(&self) -> &PolicyId {
        &self.policy_id
    }

    /// Returns the cost model.
    #[must_use]
    pub const fn budget(&self) -> CostBound {
        self.budget
    }

    /// Returns one compiled rule.
    #[must_use]
    pub fn rule(&self, id: &RuleId) -> Option<&CompiledRule> {
        self.rules.get(id)
    }

    /// Iterates over the compiled rules in canonical identity order.
    pub fn iter(&self) -> Iter<'_, RuleId, CompiledRule> {
        self.rules.iter()
    }

    /// Returns the rule count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Returns true when the policy has no rule.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Returns the union of the rule fact read sets.
    #[must_use]
    pub fn fact_read_set(&self) -> FactReadSet {
        let mut facts = FactReadSet::new();
        for rule in self.rules.values() {
            for source in rule.metadata.fact_reads.iter() {
                facts.insert(source.clone());
            }
        }
        facts
    }

    /// Returns the canonical bytes of the compiled rule bodies.
    #[must_use]
    pub fn rule_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        canonical::push_u64(&mut bytes, self.rules.len() as u64);
        for (id, rule) in &self.rules {
            canonical::push_str(&mut bytes, id.as_str());
            canonical::push_bytes(&mut bytes, rule.body.ast());
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(body: &[u8]) -> PolicyContent {
        let id = RuleId::parse("tenant_scope").expect("the rule identity is valid");
        let mut rules = BTreeMap::new();
        rules.insert(
            id,
            RuleBody::new(body.to_vec(), std::collections::BTreeSet::new(), 0, 0)
                .expect("the body is valid"),
        );
        PolicyContent::new(
            PolicyId::parse("tenant_policy").expect("the policy identity is valid"),
            SUPPORTED_POLICY_AST_VERSION,
            rules,
            CostBound::new(100, 8),
        )
        .expect("the policy is valid")
    }

    #[test]
    fn a_body_change_creates_a_new_policy_version() {
        let first = policy(b"allow owner");
        let second = policy(b"deny owner");
        assert_ne!(first.version(), second.version());
        assert_eq!(first.policy_id(), second.policy_id());
    }

    #[test]
    fn one_policy_version_is_stable() {
        let first = policy(b"allow owner");
        let second = policy(b"allow owner");
        assert_eq!(first.version(), second.version());
        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
    }

    #[test]
    fn an_empty_rule_ast_is_a_parse_error() {
        let id = RuleId::parse("tenant_scope").expect("the rule identity is valid");
        let result = RuleBody::new(Vec::new(), std::collections::BTreeSet::new(), 1, 1);
        assert!(result.is_err());
        assert!(
            RuleBody::new(
                vec![1],
                {
                    let mut paths = std::collections::BTreeSet::new();
                    paths.insert(id.to_string());
                    paths
                },
                1,
                1
            )
            .is_ok()
        );
    }
}
