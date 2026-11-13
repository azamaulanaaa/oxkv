//! Rule metadata for the manifest.
//!
//! The manifest declares rule identity, selector, effect, scope, cost bound,
//! strict flag, retention class, audit visibility, and fact read set. The
//! manifest does not hold a rule body. A rule body lives in the policy
//! catalog. A body change therefore leaves [`RuleManifest::digest`] unchanged.

use std::collections::BTreeSet;
use std::collections::btree_map::Iter;

use oxkv_contracts::keyspace::TableName;
use oxkv_contracts::version::{ContentDigest, DigestDomain, FactSourceId, Hash32, ManifestDigest};

use crate::canonical;
use crate::error::RuleError;

/// One rule identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleId(String);

impl RuleId {
    /// Parses one rule identity.
    ///
    /// The grammar is `[a-z][a-z0-9_.-]*`.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::CompileFailed`] when the text does not match the
    /// grammar.
    pub fn parse(text: &str) -> Result<Self, RuleError> {
        validate_identifier("rule", text)?;
        Ok(Self(text.to_owned()))
    }

    /// Returns the identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The effect of one rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuleEffect {
    /// The rule filters a read scope.
    ReadScope,
    /// The rule validates a write.
    WriteValidation,
}

impl RuleEffect {
    /// Returns the canonical effect name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ReadScope => "read_scope",
            Self::WriteValidation => "write_validation",
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::ReadScope => 1,
            Self::WriteValidation => 2,
        }
    }
}

/// The cohort unit that one rule may govern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuleScope {
    /// One tenant.
    Tenant,
    /// One explicit scope.
    Scope,
    /// One shard.
    Shard,
}

impl RuleScope {
    /// Returns the canonical scope name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::Scope => "scope",
            Self::Shard => "shard",
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::Tenant => 1,
            Self::Scope => 2,
            Self::Shard => 3,
        }
    }
}

/// The retention class of one rule record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RetentionClass {
    /// The record follows the audit retention floor.
    AuditFloor,
    /// The record follows the operational retention window.
    Operational,
}

impl RetentionClass {
    /// Returns the canonical class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::AuditFloor => "audit_floor",
            Self::Operational => "operational",
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::AuditFloor => 1,
            Self::Operational => 2,
        }
    }
}

/// The audit visibility of one rule verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AuditVisibility {
    /// The audit record carries the full decision.
    Visible,
    /// The audit record redacts the row value.
    Redacted,
}

impl AuditVisibility {
    /// Returns the canonical visibility name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Redacted => "redacted",
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::Visible => 1,
            Self::Redacted => 2,
        }
    }
}

/// A finite cost bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CostBound {
    /// The maximum step count.
    max_steps: u64,
    /// The maximum expression depth.
    max_depth: u32,
}

impl CostBound {
    /// Creates a cost bound.
    #[must_use]
    pub const fn new(max_steps: u64, max_depth: u32) -> Self {
        Self {
            max_steps,
            max_depth,
        }
    }

    /// Returns the maximum step count.
    #[must_use]
    pub const fn max_steps(self) -> u64 {
        self.max_steps
    }

    /// Returns the maximum expression depth.
    #[must_use]
    pub const fn max_depth(self) -> u32 {
        self.max_depth
    }
}

/// The selector of one rule.
///
/// The selector names one table and the field paths that the rule may read.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuleSelector {
    table: TableName,
    paths: BTreeSet<String>,
}

impl RuleSelector {
    /// Creates a selector.
    #[must_use]
    pub fn new(table: TableName, paths: BTreeSet<String>) -> Self {
        Self { table, paths }
    }

    /// Returns the table name.
    #[must_use]
    pub const fn table(&self) -> &TableName {
        &self.table
    }

    /// Returns true when the selector declares the path.
    #[must_use]
    pub fn allows_path(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    /// Iterates over the declared paths in canonical order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.paths.iter().map(String::as_str)
    }

    /// Returns true when the selector declares no path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// The set of fact sources that one rule reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct FactReadSet(BTreeSet<FactSourceId>);

impl FactReadSet {
    /// Creates an empty read set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one fact source.
    pub fn insert(&mut self, source: FactSourceId) {
        self.0.insert(source);
    }

    /// Returns true when the set holds the source.
    #[must_use]
    pub fn contains(&self, source: &FactSourceId) -> bool {
        self.0.contains(source)
    }

    /// Iterates over the fact sources in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = &FactSourceId> {
        self.0.iter()
    }

    /// Returns true when the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the content digest of the canonical read set.
    #[must_use]
    pub fn digest(&self) -> ContentDigest {
        let mut bytes = Vec::new();
        canonical::push_u64(&mut bytes, self.0.len() as u64);
        for source in &self.0 {
            canonical::push_str(&mut bytes, source.as_str());
        }
        ContentDigest::of_bytes(&bytes)
    }
}

impl FromIterator<FactSourceId> for FactReadSet {
    fn from_iter<T: IntoIterator<Item = FactSourceId>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// The rule metadata of one manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuleManifest {
    rules: std::collections::BTreeMap<RuleId, RuleMetadata>,
}

impl<'a> IntoIterator for &'a RuleManifest {
    type Item = (&'a RuleId, &'a RuleMetadata);
    type IntoIter = Iter<'a, RuleId, RuleMetadata>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl RuleManifest {
    /// Creates a rule manifest.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError::DuplicateRule`] when one rule identity appears two
    /// times.
    pub fn new(rules: Vec<RuleMetadata>) -> Result<Self, RuleError> {
        let mut table = std::collections::BTreeMap::new();
        for rule in rules {
            let id = rule.id.clone();
            if table.insert(id.clone(), rule).is_some() {
                return Err(RuleError::DuplicateRule {
                    rule_id: id.to_string(),
                });
            }
        }
        Ok(Self { rules: table })
    }

    /// Returns one rule.
    #[must_use]
    pub fn rule(&self, id: &RuleId) -> Option<&RuleMetadata> {
        self.rules.get(id)
    }

    /// Iterates over the rules in canonical identity order.
    pub fn iter(&self) -> Iter<'_, RuleId, RuleMetadata> {
        self.rules.iter()
    }

    /// Returns the rule count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Returns true when the manifest declares no rule.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Returns the canonical bytes of the rule metadata section.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        canonical::push_u64(&mut bytes, self.rules.len() as u64);
        for (id, rule) in &self.rules {
            canonical::push_str(&mut bytes, id.as_str());
            canonical::push_tag(&mut bytes, rule.effect.tag());
            canonical::push_tag(&mut bytes, rule.scope.tag());
            canonical::push_str(&mut bytes, rule.selector.table().as_str());
            canonical::push_u64(&mut bytes, rule.selector.paths.len() as u64);
            for path in rule.selector.paths() {
                canonical::push_str(&mut bytes, path);
            }
            canonical::push_u64(&mut bytes, rule.cost.max_steps());
            canonical::push_u32(&mut bytes, rule.cost.max_depth());
            canonical::push_bool(&mut bytes, rule.strict);
            canonical::push_tag(&mut bytes, rule.retention.tag());
            canonical::push_tag(&mut bytes, rule.audit.tag());
            canonical::push_u64(&mut bytes, rule.fact_reads.0.len() as u64);
            for source in &rule.fact_reads.0 {
                canonical::push_str(&mut bytes, source.as_str());
            }
        }
        bytes
    }

    /// Returns the rule metadata digest.
    ///
    /// The digest covers the rule metadata only. It does not cover a rule
    /// body. A manifest that combines this section with schema and layout can
    /// use this value as the rule section contribution.
    #[must_use]
    pub fn digest(&self) -> ManifestDigest {
        let hash = Hash32::of_domain(DigestDomain::Manifest, &self.canonical_bytes());
        ManifestDigest::from_bytes(*hash.as_bytes())
    }
}

/// The metadata of one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMetadata {
    /// The rule identity.
    pub id: RuleId,
    /// The rule effect.
    pub effect: RuleEffect,
    /// The scope unit that the rule may govern.
    pub scope: RuleScope,
    /// The selector.
    pub selector: RuleSelector,
    /// The finite cost bound.
    pub cost: CostBound,
    /// True when a stale fact distance rejects the write.
    pub strict: bool,
    /// The retention class.
    pub retention: RetentionClass,
    /// The audit visibility.
    pub audit: AuditVisibility,
    /// The fact sources that the rule reads.
    pub fact_reads: FactReadSet,
}

/// Validates one lower-case identifier.
pub(crate) fn validate_identifier(kind: &str, text: &str) -> Result<(), RuleError> {
    let mut bytes = text.bytes();
    let first = bytes.next();
    if !matches!(first, Some(byte) if byte.is_ascii_lowercase()) {
        return Err(RuleError::CompileFailed {
            detail: format!("The {kind} identity {text} does not start with a lower-case letter."),
        });
    }
    if !bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
    }) {
        return Err(RuleError::CompileFailed {
            detail: format!("The {kind} identity {text} has an invalid character."),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, body_path: &str) -> RuleMetadata {
        let mut paths = BTreeSet::new();
        paths.insert(body_path.to_owned());
        RuleMetadata {
            id: RuleId::parse(id).expect("the rule identity is valid"),
            effect: RuleEffect::ReadScope,
            scope: RuleScope::Tenant,
            selector: RuleSelector::new(
                TableName::parse_user("accounts").expect("the table name is valid"),
                paths,
            ),
            cost: CostBound::new(100, 8),
            strict: false,
            retention: RetentionClass::AuditFloor,
            audit: AuditVisibility::Visible,
            fact_reads: FactReadSet::new(),
        }
    }

    #[test]
    fn a_manifest_rejects_a_duplicate_rule() {
        let result = RuleManifest::new(vec![
            rule("tenant_scope", "owner"),
            rule("tenant_scope", "owner"),
        ]);
        assert_eq!(
            result,
            Err(RuleError::DuplicateRule {
                rule_id: "tenant_scope".to_owned()
            })
        );
    }

    #[test]
    fn a_manifest_digest_ignores_no_body_because_no_body_is_present() {
        let manifest =
            RuleManifest::new(vec![rule("tenant_scope", "owner")]).expect("the manifest is valid");
        let first = manifest.digest();
        let second = manifest.digest();
        assert_eq!(first, second);
        assert_eq!(manifest.len(), 1);
    }

    #[test]
    fn a_rule_identity_rejects_a_bad_shape() {
        assert!(RuleId::parse("TenantScope").is_err());
        assert!(RuleId::parse("").is_err());
        assert!(RuleId::parse("tenant/scope").is_err());
        assert!(RuleId::parse("tenant_scope").is_ok());
    }
}
