//! The scope predicate and the replica index build.
//!
//! A replica builds its own index from the documents in its scope. The build
//! never reads or copies an authority segment. The same tokenizer, the same
//! scorer, and the same canonical encoding produce the same bytes on native and
//! `wasm32-unknown-unknown`.

use std::collections::BTreeMap;

use oxkv_contracts::version::{CommitVersion, PrefixBounds};

use crate::error::IndexError;
use crate::index::ShardIndex;
use crate::manifest::IndexManifest;
use crate::posting::IndexDocument;

/// A policy-filtered scope predicate.
///
/// The same predicate governs reads, writes, and the index build. A candidate
/// posting must pass it before it enters a result.
pub trait Scope {
    /// Returns true when the key is inside the scope.
    fn contains(&self, key: &[u8]) -> bool;
}

/// A scope that admits every key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AllScope;

impl Scope for AllScope {
    fn contains(&self, _key: &[u8]) -> bool {
        true
    }
}

impl Scope for PrefixBounds {
    fn contains(&self, key: &[u8]) -> bool {
        key >= self.lower() && key < self.upper()
    }
}

/// Builds a replica index from scope members.
///
/// The function filters every member through the shard bounds and the scope.
/// It sorts the members by key, so the input order does not change the bytes.
/// A duplicate key fails with [`IndexError::MalformedDocument`].
///
/// # Errors
///
/// Returns the same errors as [`ShardIndex::begin_write`] and
/// [`crate::index::WriteTransaction::put`]. Returns
/// [`IndexError::MalformedDocument`] for a duplicate member key.
pub fn build_scope_index(
    manifest: &IndexManifest,
    bounds: &PrefixBounds,
    scope: &dyn Scope,
    members: impl IntoIterator<Item = IndexDocument>,
    commit_version: CommitVersion,
) -> Result<ShardIndex, IndexError> {
    let mut selected: BTreeMap<Vec<u8>, IndexDocument> = BTreeMap::new();
    for member in members {
        if !bounds.contains(member.key()) || !scope.contains(member.key()) {
            continue;
        }
        if selected.insert(member.key().to_vec(), member).is_some() {
            return Err(IndexError::MalformedDocument {
                reason: "a scope member key repeats".to_owned(),
            });
        }
    }
    if selected.is_empty() {
        return ShardIndex::empty_at(manifest.clone(), bounds.clone(), commit_version);
    }
    let mut index = ShardIndex::new(manifest.clone(), bounds.clone())?;
    let mut transaction = index.begin_write(commit_version)?;
    for document in selected.into_values() {
        transaction.put(&document)?;
    }
    transaction.commit()?;
    index.set_oldest_retained(commit_version)?;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::FieldSpec;
    use crate::posting::DocumentField;
    use crate::score::ScorerSpec;
    use crate::tokenizer::{TokenBudget, TokenizerIdentity};

    fn manifest() -> IndexManifest {
        let field = FieldSpec::new("title", TokenizerIdentity::unicode_word());
        IndexManifest::new(
            vec![field],
            TokenizerIdentity::unicode_word(),
            ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
            100_000,
            TokenBudget::DEFAULT,
        )
        .expect("the manifest is valid")
    }

    fn document(key: &[u8], text: &str) -> IndexDocument {
        IndexDocument::new(key.to_vec(), vec![DocumentField::new("title", text, 0, 0)])
    }

    #[test]
    fn a_scope_build_drops_out_of_scope_members() {
        let bounds = PrefixBounds::new(b"a".to_vec(), b"b".to_vec());
        let index = build_scope_index(
            &manifest(),
            &bounds,
            &bounds,
            vec![
                document(b"apple", "apple"),
                document(b"avocado", "avocado"),
                document(b"banana", "banana"),
            ],
            CommitVersion::new(1),
        )
        .expect("the build succeeds");
        assert!(
            index
                .documents_at(CommitVersion::new(1))
                .expect("the pin is valid")
                .contains_key(b"apple".as_slice())
        );
        assert!(
            !index
                .documents_at(CommitVersion::new(1))
                .expect("the pin is valid")
                .contains_key(b"banana".as_slice())
        );
        let postings = index.postings_for("title", "banana");
        assert_eq!(postings.len(), 0, "the out-of-scope posting is present");
    }

    #[test]
    fn the_build_is_independent_of_input_order() {
        let bounds = PrefixBounds::new(b"a".to_vec(), b"b".to_vec());
        let first = build_scope_index(
            &manifest(),
            &bounds,
            &AllScope,
            vec![document(b"apple", "apple"), document(b"avocado", "avocado")],
            CommitVersion::new(1),
        )
        .expect("the build succeeds");
        let second = build_scope_index(
            &manifest(),
            &bounds,
            &AllScope,
            vec![document(b"avocado", "avocado"), document(b"apple", "apple")],
            CommitVersion::new(1),
        )
        .expect("the build succeeds");
        assert_eq!(first.encode(), second.encode());
    }

    #[test]
    fn a_duplicate_member_fails() {
        let bounds = PrefixBounds::new(b"a".to_vec(), b"b".to_vec());
        let error = build_scope_index(
            &manifest(),
            &bounds,
            &AllScope,
            vec![document(b"apple", "apple"), document(b"apple", "apple")],
            CommitVersion::new(1),
        )
        .expect_err("the duplicate is rejected");
        assert_eq!(error.code(), "index.malformed_document");
    }
}
