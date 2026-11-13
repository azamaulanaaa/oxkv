//! Deterministic scope identity.
//!
//! The identity is `BLAKE3` over the canonical declaration bytes. The domain
//! tag is [`DigestDomain::Scope`]. The input covers the query prefixes, the
//! rule set selector, the projection, the ordering, and the admission limits.
//!
//! The form uses no host byte order, no clock, and no randomness. Native and
//! `wasm32` produce the same 32 bytes.

use oxkv_contracts::version::{DigestDomain, Hash32, ScopeIdentity};

use crate::declaration::ScopeDeclaration;

/// Computes the identity of one declaration.
#[must_use]
pub fn scope_identity(declaration: &ScopeDeclaration) -> ScopeIdentity {
    let digest = Hash32::of_domain(DigestDomain::Scope, &declaration.canonical_bytes());
    ScopeIdentity::from_bytes(*digest.as_bytes())
}

impl ScopeDeclaration {
    /// Returns the deterministic scope identity.
    #[must_use]
    pub fn identity(&self) -> ScopeIdentity {
        scope_identity(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declaration::{
        AdmissionLimits, FieldName, Ordering, PolicySelector, ProjectionSelector, RuleSetSelector,
        TablePrefix,
    };
    use oxkv_contracts::version::PolicyVersion;

    fn declaration() -> ScopeDeclaration {
        ScopeDeclaration::new(
            vec![
                TablePrefix::parse("accounts").expect("valid table"),
                TablePrefix::parse("ledger_entry").expect("valid table"),
            ],
            PolicySelector::RuleSet(RuleSetSelector::new("default").expect("valid selector")),
            ProjectionSelector::fields([
                FieldName::new("title").expect("valid field"),
                FieldName::new("body").expect("valid field"),
            ])
            .expect("valid projection"),
            Ordering::Field(FieldName::new("title").expect("valid field")),
            AdmissionLimits::new(1_000, 1_000_000, 3),
        )
        .expect("valid declaration")
    }

    #[test]
    fn the_same_query_has_the_same_identity() {
        assert_eq!(declaration().identity(), declaration().identity());
    }

    #[test]
    fn a_different_selector_changes_the_identity() {
        let base = declaration();
        let other = ScopeDeclaration::new(
            base.prefixes().to_vec(),
            PolicySelector::RuleSet(RuleSetSelector::new("other").expect("valid selector")),
            base.projection().clone(),
            base.ordering().clone(),
            base.limits(),
        )
        .expect("valid declaration");
        assert_ne!(base.identity(), other.identity());
    }

    #[test]
    fn a_canonical_policy_reference_changes_the_identity() {
        let base = declaration();
        let other = ScopeDeclaration::new(
            base.prefixes().to_vec(),
            PolicySelector::Canonical(PolicyVersion::from_bytes([7; 32])),
            base.projection().clone(),
            base.ordering().clone(),
            base.limits(),
        )
        .expect("valid declaration");
        assert_ne!(base.identity(), other.identity());
    }

    #[test]
    fn a_different_limit_changes_the_identity() {
        let base = declaration();
        let other = ScopeDeclaration::new(
            base.prefixes().to_vec(),
            base.selector().clone(),
            base.projection().clone(),
            base.ordering().clone(),
            AdmissionLimits::new(1, 2, 3),
        )
        .expect("valid declaration");
        assert_ne!(base.identity(), other.identity());
    }
}
