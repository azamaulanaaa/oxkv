//! Pinned search over the inverted index.
//!
//! A search reads one pin. It filters candidates through the shard bounds and
//! the scope before it ranks and before it applies the limit. The order is
//! `(score descending, canonical key ascending)`. The scorer uses document-local
//! features only.

use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::version::CommitVersion;

use crate::error::IndexError;
use crate::index::{ShardIndex, key_in_bounds};
use crate::manifest::{FieldSpec, IndexManifest};
use crate::posting::{PostingFlags, StoredDocument};
use crate::scope::Scope;
use crate::tokenizer::{TokenizerIdentity, tokenize_text};

/// The match mode of a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchMode {
    /// A document matches when it contains any query term.
    Any,
    /// A document matches when it contains every query term.
    All,
}

/// One pinned search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    text: String,
    fields: Vec<String>,
    tokenizer: TokenizerIdentity,
    match_mode: MatchMode,
    filter_flags: PostingFlags,
    search_flags: PostingFlags,
    include_positions: bool,
    limit: Option<usize>,
    pin: CommitVersion,
}

impl SearchRequest {
    /// Creates a request for one pin.
    #[must_use]
    pub fn new(text: impl Into<String>, pin: CommitVersion) -> Self {
        Self {
            text: text.into(),
            fields: Vec::new(),
            tokenizer: TokenizerIdentity::unicode_word(),
            match_mode: MatchMode::Any,
            filter_flags: PostingFlags::EMPTY,
            search_flags: PostingFlags::EMPTY,
            include_positions: false,
            limit: None,
            pin,
        }
    }

    /// Creates a request with the default tokenizer of a manifest.
    #[must_use]
    pub fn for_manifest(
        manifest: &IndexManifest,
        text: impl Into<String>,
        pin: CommitVersion,
    ) -> Self {
        Self::new(text, pin).with_tokenizer(manifest.default_tokenizer())
    }

    /// Sets the field list. An empty list selects every searchable field.
    #[must_use]
    pub fn with_fields(mut self, fields: Vec<String>) -> Self {
        self.fields = fields;
        self
    }

    /// Sets the request tokenizer identity.
    #[must_use]
    pub const fn with_tokenizer(mut self, tokenizer: TokenizerIdentity) -> Self {
        self.tokenizer = tokenizer;
        self
    }

    /// Sets the match mode.
    #[must_use]
    pub const fn with_match_mode(mut self, match_mode: MatchMode) -> Self {
        self.match_mode = match_mode;
        self
    }

    /// Sets the required filter flag bits.
    #[must_use]
    pub const fn with_filter_flags(mut self, flags: u64) -> Self {
        self.filter_flags = PostingFlags::new(flags);
        self
    }

    /// Sets the required search flag bits.
    #[must_use]
    pub const fn with_search_flags(mut self, flags: u64) -> Self {
        self.search_flags = PostingFlags::new(flags);
        self
    }

    /// Sets whether the result includes token positions.
    #[must_use]
    pub const fn with_positions(mut self, include_positions: bool) -> Self {
        self.include_positions = include_positions;
        self
    }

    /// Sets the maximum number of hits.
    #[must_use]
    pub const fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Returns the query text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the requested field names.
    #[must_use]
    pub fn fields(&self) -> &[String] {
        &self.fields
    }

    /// Returns the request tokenizer identity.
    #[must_use]
    pub const fn tokenizer(&self) -> TokenizerIdentity {
        self.tokenizer
    }

    /// Returns the match mode.
    #[must_use]
    pub const fn match_mode(&self) -> MatchMode {
        self.match_mode
    }

    /// Returns the required filter flag bits.
    #[must_use]
    pub const fn filter_flags(&self) -> PostingFlags {
        self.filter_flags
    }

    /// Returns the required search flag bits.
    #[must_use]
    pub const fn search_flags(&self) -> PostingFlags {
        self.search_flags
    }

    /// Returns true when the result includes token positions.
    #[must_use]
    pub const fn include_positions(&self) -> bool {
        self.include_positions
    }

    /// Returns the hit limit.
    #[must_use]
    pub const fn limit(&self) -> Option<usize> {
        self.limit
    }

    /// Returns the search pin.
    #[must_use]
    pub const fn pin(&self) -> CommitVersion {
        self.pin
    }
}

/// One term match in a search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermMatch {
    /// The field name.
    pub field: String,
    /// The term text.
    pub term: String,
    /// The term frequency in the field.
    pub term_frequency: u32,
    /// The token positions. The list is empty when positions were not
    /// requested or the field does not store positions.
    pub positions: Vec<u32>,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The canonical document key.
    pub key: Vec<u8>,
    /// The integer score in thousandths.
    pub score: u64,
    /// The matching terms in canonical `(field, term)` order.
    pub matches: Vec<TermMatch>,
}

/// The result of one pinned search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    /// The pin that served the result.
    pub pin: CommitVersion,
    /// The ranked hits.
    pub hits: Vec<SearchHit>,
    /// The number of hits before the limit.
    pub total_hits: usize,
}

impl ShardIndex {
    /// Searches the index at one pin.
    ///
    /// The index verifies its digest before the search. A corrupt index
    /// returns [`IndexError::IndexMismatch`]. A tokenizer mismatch returns
    /// [`IndexError::TokenizerMismatch`].
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::PinUnavailable`] when the pin is not retained.
    /// Returns [`IndexError::UnknownField`] or
    /// [`IndexError::FieldNotSearchable`] for a bad field list. Returns
    /// [`IndexError::TokenizerMismatch`] or
    /// [`IndexError::TokenizerUnknown`] for a tokenizer disagreement. Returns
    /// [`IndexError::UnknownFlag`] for an undeclared flag. Returns
    /// [`IndexError::TokenCountExceeded`] or
    /// [`IndexError::TokenLengthExceeded`] when the query exceeds the token
    /// budget.
    pub fn search(
        &self,
        request: &SearchRequest,
        scope: &dyn Scope,
    ) -> Result<SearchResult, IndexError> {
        self.verify()?;
        request.tokenizer.validate()?;
        let selected = selected_fields(self.manifest(), request)?;
        let query_terms = prepare_query(self.manifest(), request)?;
        let visible = self.visible_state(request.pin)?;
        let mut accumulators: BTreeMap<Vec<u8>, Accumulator> = BTreeMap::new();
        if query_terms.is_empty() {
            collect_filtered(self, request, scope, &selected, &visible, &mut accumulators);
        } else {
            collect_terms(
                self,
                request,
                scope,
                &selected,
                &query_terms,
                &visible,
                &mut accumulators,
            );
        }
        let mut hits = finish_hits(accumulators, request.match_mode, &query_terms);
        let total_hits = hits.len();
        hits.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.key.cmp(&right.key))
        });
        if let Some(limit) = request.limit {
            hits.truncate(limit);
        }
        Ok(SearchResult {
            pin: request.pin,
            hits,
            total_hits,
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct Accumulator {
    pub(crate) score: u64,
    pub(crate) matched_terms: BTreeSet<String>,
    pub(crate) matches: BTreeMap<(String, String), TermMatch>,
}

pub(crate) fn selected_fields<'a>(
    manifest: &'a IndexManifest,
    request: &SearchRequest,
) -> Result<Vec<&'a FieldSpec>, IndexError> {
    let mut selected: Vec<&FieldSpec> = if request.fields.is_empty() {
        manifest
            .fields()
            .iter()
            .filter(|field| field.indexed() && field.searchable())
            .collect()
    } else {
        let mut fields = Vec::with_capacity(request.fields.len());
        for name in &request.fields {
            let Some(field) = manifest.field(name) else {
                return Err(IndexError::UnknownField {
                    field: name.clone(),
                });
            };
            if !field.indexed() {
                return Err(IndexError::UnknownField {
                    field: name.clone(),
                });
            }
            if !field.searchable() {
                return Err(IndexError::FieldNotSearchable {
                    field: name.clone(),
                });
            }
            fields.push(field);
        }
        fields
    };
    selected.sort_by(|left, right| left.name().cmp(right.name()));
    selected.dedup_by(|left, right| left.name() == right.name());
    if selected.is_empty() {
        return Err(IndexError::FieldNotSearchable {
            field: "<all>".to_owned(),
        });
    }
    for field in &selected {
        if field.tokenizer() != request.tokenizer {
            return Err(IndexError::TokenizerMismatch {
                expected: request.tokenizer,
                found: field.tokenizer(),
            });
        }
    }
    let filter_mask = selected
        .iter()
        .fold(0_u64, |mask, field| mask | field.filter_flags().mask());
    let search_mask = selected
        .iter()
        .fold(0_u64, |mask, field| mask | field.search_flags().mask());
    validate_mask(
        request.filter_flags,
        filter_mask,
        selected.first().map_or("<all>", |field| field.name()),
    )?;
    validate_mask(
        request.search_flags,
        search_mask,
        selected.first().map_or("<all>", |field| field.name()),
    )?;
    Ok(selected)
}

fn validate_mask(flags: PostingFlags, mask: u64, field: &str) -> Result<(), IndexError> {
    let unknown = flags.bits() & !mask;
    if unknown == 0 {
        return Ok(());
    }
    let bit = u8::try_from(unknown.trailing_zeros()).unwrap_or(u8::MAX);
    Err(IndexError::UnknownFlag {
        field: field.to_owned(),
        bit,
    })
}

pub(crate) fn prepare_query(
    manifest: &IndexManifest,
    request: &SearchRequest,
) -> Result<BTreeSet<String>, IndexError> {
    let budget = manifest.token_budget();
    let tokens = tokenize_text(request.tokenizer, &request.text, budget)?;
    Ok(tokens.iter().map(|token| token.text().to_owned()).collect())
}

fn collect_filtered(
    index: &ShardIndex,
    request: &SearchRequest,
    scope: &dyn Scope,
    selected: &[&FieldSpec],
    visible: &BTreeMap<Vec<u8>, (CommitVersion, &StoredDocument)>,
    accumulators: &mut BTreeMap<Vec<u8>, Accumulator>,
) {
    for (key, (_, document)) in visible {
        if !key_in_bounds(index.bounds(), key) || !scope.contains(key) {
            continue;
        }
        let matched = selected.iter().any(|field| {
            document.fields().get(field.name()).is_some_and(|stored| {
                stored.filter_flags().contains(request.filter_flags)
                    && stored.search_flags().contains(request.search_flags)
            })
        });
        if matched {
            accumulators.insert(key.clone(), Accumulator::default());
        }
    }
}

#[allow(clippy::too_many_arguments)] // The arguments name one explicit plan.
fn collect_terms(
    index: &ShardIndex,
    request: &SearchRequest,
    scope: &dyn Scope,
    selected: &[&FieldSpec],
    query_terms: &BTreeSet<String>,
    visible: &BTreeMap<Vec<u8>, (CommitVersion, &StoredDocument)>,
    accumulators: &mut BTreeMap<Vec<u8>, Accumulator>,
) {
    for field in selected {
        for term in query_terms {
            let posting_key = (field.name().to_owned(), term.clone());
            for segment in index.segments() {
                if segment.commit_version() > request.pin {
                    continue;
                }
                let Some(postings) = segment.postings().get(&posting_key) else {
                    continue;
                };
                for posting in postings {
                    let Some((version, document)) = visible.get(posting.document()) else {
                        continue;
                    };
                    if *version != posting.commit_version()
                        || !key_in_bounds(index.bounds(), posting.document())
                        || !scope.contains(posting.document())
                        || !posting.filter_flags().contains(request.filter_flags)
                        || !posting.search_flags().contains(request.search_flags)
                    {
                        continue;
                    }
                    let Some(stored) = document.fields().get(field.name()) else {
                        continue;
                    };
                    let term_score = index
                        .manifest()
                        .scorer()
                        .score(posting.term_frequency(), stored.token_count());
                    let accumulator = accumulators.entry(posting.document().to_vec()).or_default();
                    accumulator.score = accumulator.score.saturating_add(term_score);
                    accumulator.matched_terms.insert(term.clone());
                    accumulator.matches.insert(
                        posting_key.clone(),
                        TermMatch {
                            field: field.name().to_owned(),
                            term: term.clone(),
                            term_frequency: posting.term_frequency(),
                            positions: if request.include_positions {
                                posting.positions().to_vec()
                            } else {
                                Vec::new()
                            },
                        },
                    );
                }
            }
        }
    }
}

pub(crate) fn finish_hits(
    accumulators: BTreeMap<Vec<u8>, Accumulator>,
    match_mode: MatchMode,
    query_terms: &BTreeSet<String>,
) -> Vec<SearchHit> {
    accumulators
        .into_iter()
        .filter(|(_, accumulator)| {
            match_mode == MatchMode::Any || accumulator.matched_terms.len() == query_terms.len()
        })
        .map(|(key, accumulator)| SearchHit {
            key,
            score: accumulator.score,
            matches: accumulator.matches.into_values().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::version::PrefixBounds;

    use crate::manifest::FieldSpec;
    use crate::posting::{DocumentField, FlagDecl, IndexDocument};
    use crate::score::ScorerSpec;
    use crate::tokenizer::TokenBudget;

    fn manifest(token_budget: TokenBudget) -> IndexManifest {
        let field = FieldSpec::new("title", TokenizerIdentity::unicode_word())
            .with_positions(true)
            .with_filterable(true)
            .with_filter_flags(vec![
                FlagDecl::new("published", 0).expect("the flag is valid"),
            ])
            .expect("the filter flags are valid")
            .with_search_flags(vec![
                FlagDecl::new("promoted", 0).expect("the flag is valid"),
            ])
            .expect("the search flags are valid");
        IndexManifest::new(
            vec![field],
            TokenizerIdentity::unicode_word(),
            ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
            100_000,
            token_budget,
        )
        .expect("the manifest is valid")
    }

    fn bounds() -> PrefixBounds {
        PrefixBounds::new(Vec::new(), vec![0xFF])
    }

    fn document(key: &[u8], text: &str, filter: u64, search: u64) -> IndexDocument {
        IndexDocument::new(
            key.to_vec(),
            vec![DocumentField::new("title", text, filter, search)],
        )
    }

    fn index() -> ShardIndex {
        let mut index =
            ShardIndex::new(manifest(TokenBudget::DEFAULT), bounds()).expect("the index is valid");
        let mut transaction = index
            .begin_write(CommitVersion::new(1))
            .expect("the version advances");
        transaction
            .put(&document(b"a", "rust rust search", 1, 0))
            .expect("the document is valid");
        transaction
            .put(&document(b"b", "rust database", 1, 1))
            .expect("the document is valid");
        transaction
            .put(&document(b"c", "search only", 1, 0))
            .expect("the document is valid");
        transaction.commit().expect("the commit succeeds");
        index
    }

    #[test]
    fn search_ranks_by_score_then_key() {
        let index = index();
        let request = SearchRequest::new("rust", CommitVersion::new(1));
        let result = index
            .search(&request, &crate::scope::AllScope)
            .expect("the search succeeds");
        assert_eq!(result.total_hits, 2);
        assert_eq!(result.hits[0].key, b"a");
        assert_eq!(result.hits[1].key, b"b");
        assert!(result.hits[0].score > result.hits[1].score);
    }

    #[test]
    fn search_returns_positions_when_requested() {
        let index = index();
        let request = SearchRequest::new("rust", CommitVersion::new(1)).with_positions(true);
        let result = index
            .search(&request, &crate::scope::AllScope)
            .expect("the search succeeds");
        let hit = &result.hits[0];
        assert_eq!(hit.matches.len(), 1);
        assert_eq!(hit.matches[0].positions, [0, 1]);
    }

    #[test]
    fn search_honours_the_limit_and_reports_the_total() {
        let index = index();
        let request = SearchRequest::new("rust", CommitVersion::new(1)).with_limit(1);
        let result = index
            .search(&request, &crate::scope::AllScope)
            .expect("the search succeeds");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.total_hits, 2);
    }

    #[test]
    fn search_filters_on_declared_flags() {
        let index = index();
        let request = SearchRequest::new("rust", CommitVersion::new(1)).with_search_flags(1);
        let result = index
            .search(&request, &crate::scope::AllScope)
            .expect("the search succeeds");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].key, b"b");

        let unknown = SearchRequest::new("rust", CommitVersion::new(1)).with_filter_flags(2);
        assert_eq!(
            index
                .search(&unknown, &crate::scope::AllScope)
                .expect_err("the flag is undeclared")
                .code(),
            "index.unknown_flag"
        );
    }

    #[test]
    fn search_filters_on_scope_before_ranking() {
        let index = index();
        let scope = PrefixBounds::new(b"a".to_vec(), b"b".to_vec());
        let request = SearchRequest::new("rust", CommitVersion::new(1));
        let result = index.search(&request, &scope).expect("the search succeeds");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].key, b"a");
    }

    #[test]
    fn match_all_requires_every_term() {
        let index = index();
        let request = SearchRequest::new("rust search", CommitVersion::new(1))
            .with_match_mode(MatchMode::All);
        let result = index
            .search(&request, &crate::scope::AllScope)
            .expect("the search succeeds");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].key, b"a");
    }

    #[test]
    fn a_delete_hides_a_document_at_the_new_pin_only() {
        let mut index = index();
        let mut transaction = index
            .begin_write(CommitVersion::new(2))
            .expect("the version advances");
        transaction.delete(b"a").expect("the key is in bounds");
        transaction.commit().expect("the commit succeeds");
        let request = SearchRequest::new("rust", CommitVersion::new(2));
        let after = index
            .search(&request, &crate::scope::AllScope)
            .expect("the search succeeds");
        assert_eq!(after.total_hits, 1);
        assert_eq!(after.hits[0].key, b"b");
        let before = SearchRequest::new("rust", CommitVersion::new(1));
        let before_result = index
            .search(&before, &crate::scope::AllScope)
            .expect("the old pin is retained");
        assert_eq!(before_result.total_hits, 2);
    }

    #[test]
    fn a_tokenizer_mismatch_is_named() {
        let index = index();
        let request = SearchRequest::new("rust", CommitVersion::new(1))
            .with_tokenizer(TokenizerIdentity::ascii_word());
        let error = index
            .search(&request, &crate::scope::AllScope)
            .expect_err("the tokenizer does not match");
        assert_eq!(error.code(), "search.tokenizer_mismatch");
    }

    #[test]
    fn a_pin_above_the_index_fails() {
        let index = index();
        let request = SearchRequest::new("rust", CommitVersion::new(2));
        let error = index
            .search(&request, &crate::scope::AllScope)
            .expect_err("the pin is not readable");
        assert_eq!(error.code(), "search.pin_expired");
    }

    #[test]
    fn a_token_budget_failure_leaves_the_index_unchanged() {
        let manifest = manifest(TokenBudget::new(1, 64));
        let mut index = ShardIndex::new(manifest, bounds()).expect("the index is valid");
        let before = index.encode();
        let mut transaction = index
            .begin_write(CommitVersion::new(1))
            .expect("the version advances");
        let error = transaction
            .put(&document(b"a", "two words", 1, 0))
            .expect_err("the text exceeds the budget");
        assert_eq!(error.code(), "search.token_budget_exceeded");
        drop(transaction);
        assert_eq!(index.encode(), before);
        assert_eq!(index.commit_version(), CommitVersion::new(0));
    }

    #[test]
    fn an_overhead_failure_leaves_the_index_unchanged() {
        let field = FieldSpec::new("title", TokenizerIdentity::unicode_word());
        let manifest = IndexManifest::new(
            vec![field],
            TokenizerIdentity::unicode_word(),
            ScorerSpec::new(1200, 750, 8).expect("the scorer is valid"),
            8,
            TokenBudget::DEFAULT,
        )
        .expect("the manifest is valid");
        let mut index = ShardIndex::new(manifest, bounds()).expect("the index is valid");
        let before = index.encode();
        let mut transaction = index
            .begin_write(CommitVersion::new(1))
            .expect("the version advances");
        transaction
            .put(&document(b"a", "rust", 0, 0))
            .expect("the document is valid");
        let error = transaction
            .commit()
            .expect_err("the index exceeds the budget");
        assert_eq!(error.code(), "index.overhead_exceeded");
        assert_eq!(index.encode(), before);
        assert_eq!(index.commit_version(), CommitVersion::new(0));
    }
}
