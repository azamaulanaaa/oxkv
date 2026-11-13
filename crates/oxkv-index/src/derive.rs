//! The derive-on-read search oracle.
//!
//! [`DerivedIndex`] stores documents and tokenizes them at query time. It is
//! the second in-tree implementation of the storage seam. The inverted index
//! and the derived scan must return the same result for the same documents and
//! the same scorer. The derived scan is also a fallback for a tiny table.

use std::collections::BTreeMap;

use oxkv_contracts::version::{CommitVersion, PrefixBounds};

use crate::error::IndexError;
use crate::index::{key_in_bounds, prepare_document};
use crate::manifest::IndexManifest;
use crate::posting::{IndexDocument, PostingFlags};
use crate::scope::Scope;
use crate::search::{
    Accumulator, MatchMode, SearchHit, SearchRequest, SearchResult, TermMatch, finish_hits,
    prepare_query, selected_fields,
};
use crate::tokenizer::tokenize_field;

/// A derive-on-read index for one shard and one commit version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedIndex {
    manifest: IndexManifest,
    bounds: PrefixBounds,
    commit_version: CommitVersion,
    documents: BTreeMap<Vec<u8>, IndexDocument>,
}

impl DerivedIndex {
    /// Creates an empty derived index.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::InvalidManifest`] when the shard bounds are empty.
    pub fn new(
        manifest: IndexManifest,
        bounds: PrefixBounds,
        commit_version: CommitVersion,
    ) -> Result<Self, IndexError> {
        if bounds.is_empty() {
            return Err(IndexError::InvalidManifest {
                reason: "the shard bounds are empty".to_owned(),
            });
        }
        Ok(Self {
            manifest,
            bounds,
            commit_version,
            documents: BTreeMap::new(),
        })
    }

    /// Adds or replaces one document.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as a write transaction. The document
    /// must fit the token budget and use declared fields and flags.
    pub fn put(&mut self, document: &IndexDocument) -> Result<(), IndexError> {
        let _ = prepare_document(&self.manifest, &self.bounds, document)?;
        self.documents
            .insert(document.key().to_vec(), document.clone());
        Ok(())
    }

    /// Returns the current commit version.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Returns the documents.
    #[must_use]
    pub const fn documents(&self) -> &BTreeMap<Vec<u8>, IndexDocument> {
        &self.documents
    }

    /// Searches the derived index.
    ///
    /// # Errors
    ///
    /// Returns the same request errors as [`crate::ShardIndex::search`].
    /// Returns [`IndexError::PinUnavailable`] when the pin is not the derived
    /// version.
    #[allow(clippy::too_many_lines)] // The scan mirrors the inverted search plan.
    pub fn search(
        &self,
        request: &SearchRequest,
        scope: &dyn Scope,
    ) -> Result<SearchResult, IndexError> {
        if request.pin() != self.commit_version {
            return Err(IndexError::PinUnavailable {
                requested: request.pin(),
                current: self.commit_version,
                oldest: self.commit_version,
            });
        }
        request.tokenizer().validate()?;
        let selected = selected_fields(&self.manifest, request)?;
        let query_terms = prepare_query(&self.manifest, request)?;
        let mut accumulators: BTreeMap<Vec<u8>, Accumulator> = BTreeMap::new();
        for (key, document) in &self.documents {
            if !key_in_bounds(&self.bounds, key) || !scope.contains(key) {
                continue;
            }
            let mut accumulator = Accumulator::default();
            let mut flags_match = false;
            for field_spec in &selected {
                let Some(field) = document
                    .fields()
                    .iter()
                    .find(|field| field.field() == field_spec.name())
                else {
                    continue;
                };
                let filter_flags = PostingFlags::new(field.filter_flags());
                let search_flags = PostingFlags::new(field.search_flags());
                if !filter_flags.contains(request.filter_flags())
                    || !search_flags.contains(request.search_flags())
                {
                    continue;
                }
                flags_match = true;
                let tokens = tokenize_field(
                    field_spec.name(),
                    field_spec.tokenizer(),
                    field.text(),
                    self.manifest.token_budget(),
                )?;
                let token_count = u32::try_from(tokens.len()).unwrap_or(u32::MAX);
                let mut terms: BTreeMap<String, (u32, Vec<u32>)> = BTreeMap::new();
                for token in &tokens {
                    let entry = terms
                        .entry(token.text().to_owned())
                        .or_insert_with(|| (0, Vec::new()));
                    entry.0 = entry.0.saturating_add(1);
                    entry.1.push(token.position());
                }
                for term in &query_terms {
                    let Some((frequency, positions)) = terms.get(term) else {
                        continue;
                    };
                    accumulator.score = accumulator
                        .score
                        .saturating_add(self.manifest.scorer().score(*frequency, token_count));
                    accumulator.matched_terms.insert(term.clone());
                    accumulator.matches.insert(
                        (field_spec.name().to_owned(), term.clone()),
                        TermMatch {
                            field: field_spec.name().to_owned(),
                            term: term.clone(),
                            term_frequency: *frequency,
                            positions: if request.include_positions() {
                                positions.clone()
                            } else {
                                Vec::new()
                            },
                        },
                    );
                }
            }
            if query_terms.is_empty() {
                if flags_match {
                    accumulators.insert(key.clone(), accumulator);
                }
                continue;
            }
            let matched = match request.match_mode() {
                MatchMode::Any => !accumulator.matches.is_empty(),
                MatchMode::All => accumulator.matched_terms.len() == query_terms.len(),
            };
            if matched {
                accumulators.insert(key.clone(), accumulator);
            }
        }
        let mut hits: Vec<SearchHit> =
            finish_hits(accumulators, request.match_mode(), &query_terms);
        let total_hits = hits.len();
        hits.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.key.cmp(&right.key))
        });
        if let Some(limit) = request.limit() {
            hits.truncate(limit);
        }
        Ok(SearchResult {
            pin: request.pin(),
            hits,
            total_hits,
        })
    }
}

/// Builds a derived index from scope members.
///
/// # Errors
///
/// Returns [`IndexError::MalformedDocument`] for a duplicate member key.
/// Returns the same validation errors as [`crate::ShardIndex`].
pub fn build_derived_index(
    manifest: &IndexManifest,
    bounds: &PrefixBounds,
    scope: &dyn Scope,
    members: impl IntoIterator<Item = IndexDocument>,
    commit_version: CommitVersion,
) -> Result<DerivedIndex, IndexError> {
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
    let mut index = DerivedIndex::new(manifest.clone(), bounds.clone(), commit_version)?;
    for document in selected.into_values() {
        index.put(&document)?;
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::FieldSpec;
    use crate::posting::DocumentField;
    use crate::scope::AllScope;
    use crate::score::ScorerSpec;
    use crate::search::MatchMode;
    use crate::tokenizer::{TokenBudget, TokenizerIdentity};

    fn manifest() -> IndexManifest {
        let field = FieldSpec::new("title", TokenizerIdentity::unicode_word()).with_positions(true);
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

    fn members() -> Vec<IndexDocument> {
        vec![
            document(b"a", "rust rust search"),
            document(b"b", "rust database"),
            document(b"c", "search only"),
        ]
    }

    #[test]
    fn the_derived_scan_matches_the_inverted_index() {
        let bounds = PrefixBounds::new(Vec::new(), vec![0xFF]);
        let commit_version = CommitVersion::new(1);
        let inverted = crate::scope::build_scope_index(
            &manifest(),
            &bounds,
            &AllScope,
            members(),
            commit_version,
        )
        .expect("the inverted build succeeds");
        let derived =
            build_derived_index(&manifest(), &bounds, &AllScope, members(), commit_version)
                .expect("the derived build succeeds");
        for request in [
            SearchRequest::new("rust", commit_version),
            SearchRequest::new("rust search", commit_version).with_match_mode(MatchMode::All),
            SearchRequest::new("rust", commit_version).with_positions(true),
            SearchRequest::new("search", commit_version).with_limit(1),
        ] {
            let left = inverted
                .search(&request, &AllScope)
                .expect("the inverted search succeeds");
            let right = derived
                .search(&request, &AllScope)
                .expect("the derived search succeeds");
            assert_eq!(left, right);
        }
    }

    #[test]
    fn the_derived_scan_rejects_another_pin() {
        let bounds = PrefixBounds::new(Vec::new(), vec![0xFF]);
        let derived = build_derived_index(
            &manifest(),
            &bounds,
            &AllScope,
            members(),
            CommitVersion::new(1),
        )
        .expect("the derived build succeeds");
        let error = derived
            .search(
                &SearchRequest::new("rust", CommitVersion::new(2)),
                &AllScope,
            )
            .expect_err("the pin is not the derived version");
        assert_eq!(error.code(), "search.pin_expired");
    }
}
