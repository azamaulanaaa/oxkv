//! The scope declaration and its membership rule.
//!
//! A scope is a policy-filtered query over one or more user-table prefixes.
//! The replica holds exactly one scope. The scope decides whether a page row
//! belongs in the mirror.
//!
//! This crate does not implement the policy language. The authority delivers
//! a `ScopeSnapshot` that the policy already filtered. The scope here checks the
//! table prefix, the key shape, and the declared fact sources. A page that
//! carries another row is a scope violation.

use oxkv_contracts::keyspace::{
    KEY_ENCODING_VERSION, SYSTEM_MARKER, TableName, validate_user_component_tag,
};
use oxkv_contracts::version::{
    FactSourceId, ManifestDigest, MapDigest, MapVersion, PolicyVersion, ScopeIdentity,
    TokenizerVersion,
};

/// The tuple that names one scope version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopePin {
    /// The manifest digest.
    pub manifest_digest: ManifestDigest,
    /// The policy version.
    pub policy_version: PolicyVersion,
    /// The shard map version.
    pub map_version: MapVersion,
    /// The shard map digest.
    pub map_digest: MapDigest,
    /// The canonical scope identity.
    pub scope_identity: ScopeIdentity,
}

impl ScopePin {
    /// Creates a scope pin.
    #[must_use]
    pub const fn new(
        manifest_digest: ManifestDigest,
        policy_version: PolicyVersion,
        map_version: MapVersion,
        map_digest: MapDigest,
        scope_identity: ScopeIdentity,
    ) -> Self {
        Self {
            manifest_digest,
            policy_version,
            map_version,
            map_digest,
            scope_identity,
        }
    }
}

/// A policy-filtered scope declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pin: ScopePin,
    tokenizer_version: TokenizerVersion,
    tables: Vec<TableName>,
    fact_sources: Vec<FactSourceId>,
}

impl Scope {
    /// Creates a scope declaration.
    ///
    /// The constructor sorts and deduplicates the table list and the fact
    /// source list. The pin is the scope version identity.
    #[must_use]
    pub fn new(
        pin: ScopePin,
        tokenizer_version: TokenizerVersion,
        tables: impl IntoIterator<Item = TableName>,
        fact_sources: impl IntoIterator<Item = FactSourceId>,
    ) -> Self {
        let mut tables: Vec<TableName> = tables.into_iter().collect();
        tables.sort();
        tables.dedup();
        let mut fact_sources: Vec<FactSourceId> = fact_sources.into_iter().collect();
        fact_sources.sort();
        fact_sources.dedup();
        Self {
            pin,
            tokenizer_version,
            tables,
            fact_sources,
        }
    }

    /// Returns the scope pin.
    #[must_use]
    pub const fn pin(&self) -> &ScopePin {
        &self.pin
    }

    /// Returns the canonical scope identity.
    #[must_use]
    pub const fn identity(&self) -> ScopeIdentity {
        self.pin.scope_identity
    }

    /// Returns the policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.pin.policy_version
    }

    /// Returns the manifest digest.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.pin.manifest_digest
    }

    /// Returns the tokenizer version of the local index.
    #[must_use]
    pub const fn tokenizer_version(&self) -> TokenizerVersion {
        self.tokenizer_version
    }

    /// Returns the admitted table names in canonical order.
    #[must_use]
    pub fn tables(&self) -> &[TableName] {
        &self.tables
    }

    /// Returns the admitted fact sources in canonical order.
    #[must_use]
    pub fn fact_sources(&self) -> &[FactSourceId] {
        &self.fact_sources
    }

    /// Returns true when the scope admits one user key.
    #[must_use]
    pub fn admits_row(&self, key: &[u8]) -> bool {
        let Some(table) = user_key_table(key) else {
            return false;
        };
        self.tables
            .iter()
            .any(|candidate| candidate.as_str() == table)
    }

    /// Returns true when the scope admits one fact source.
    #[must_use]
    pub fn admits_fact(&self, source: &FactSourceId) -> bool {
        self.fact_sources
            .iter()
            .any(|candidate| candidate == source)
    }
}

/// Returns the table name of one user key.
///
/// The function checks the key encoding version, the table grammar, the zero
/// terminator, and the user component marker. It returns `None` for a system
/// key, a malformed key, or an empty component list.
#[must_use]
pub fn user_key_table(key: &[u8]) -> Option<&str> {
    if key.first().copied()? != KEY_ENCODING_VERSION {
        return None;
    }
    let rest = &key[1..];
    let terminator = rest.iter().position(|byte| *byte == 0)?;
    let table = core::str::from_utf8(&rest[..terminator]).ok()?;
    TableName::parse_user(table).ok()?;
    let components = &rest[terminator + 1..];
    if components.is_empty() {
        return None;
    }
    if components.contains(&SYSTEM_MARKER) {
        return None;
    }
    validate_user_component_tag(components[0]).ok()?;
    Some(table)
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{
        FactSourceId, ManifestDigest, MapDigest, MapVersion, PolicyVersion, ScopeIdentity,
        TokenizerVersion,
    };

    use super::{Scope, ScopePin, user_key_table};

    fn sample_scope() -> Scope {
        Scope::new(
            ScopePin::new(
                ManifestDigest::from_bytes([1; 32]),
                PolicyVersion::from_bytes([2; 32]),
                MapVersion::new(3),
                MapDigest::from_bytes([4; 32]),
                ScopeIdentity::from_bytes([5; 32]),
            ),
            TokenizerVersion::new(1),
            [
                oxkv_contracts::TableName::parse_user("accounts").expect("the table name is valid"),
                oxkv_contracts::TableName::parse_user("ledger").expect("the table name is valid"),
            ],
            [FactSourceId::new("roles")],
        )
    }

    #[test]
    fn the_scope_admits_its_tables_only() {
        let scope = sample_scope();
        assert!(scope.admits_row(b"\x01accounts\x00\x10alice"));
        assert!(scope.admits_row(b"\x01ledger\x00\x20\x00\x00\x00\x00\x00\x00\x00\x01"));
        assert!(!scope.admits_row(b"\x01orders\x00\x10alice"));
        assert!(!scope.admits_row(b"not a user key"));
        assert!(!scope.admits_row(b"\x01accounts\x00"));
        assert!(!scope.admits_row(b"\x01accounts\x00\xffhidden"));
    }

    #[test]
    fn the_scope_admits_its_fact_sources_only() {
        let scope = sample_scope();
        assert!(scope.admits_fact(&FactSourceId::new("roles")));
        assert!(!scope.admits_fact(&FactSourceId::new("billing")));
    }

    #[test]
    fn a_key_table_reads_the_prefix() {
        assert_eq!(
            user_key_table(b"\x01accounts\x00\x10alice"),
            Some("accounts")
        );
        assert_eq!(user_key_table(b"\x02accounts\x00\x10alice"), None);
        assert_eq!(user_key_table(b"\x01Accounts\x00\x10alice"), None);
        assert_eq!(user_key_table(b"\x01accounts\x00\x10\xff"), None);
    }

    #[test]
    fn a_scope_sorts_and_deduplicates_its_tables() {
        let scope = sample_scope();
        assert_eq!(scope.tables().len(), 2);
        assert_eq!(scope.fact_sources().len(), 1);
    }
}
