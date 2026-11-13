//! Search coordination over pinned shard responses.
//!
//! [`execute`] validates the pin, the cursor, the fan-out bound, the index and
//! tokenizer identities, and the query budget. It then merges the ordered
//! shard streams and builds one [`SearchPage`].
//!
//! The coordinator never reads storage. The index crate supplies one
//! [`ShardResponse`] per targeted shard.

use std::collections::BTreeMap;

use oxkv_contracts::cursor::SearchCursor;
use oxkv_contracts::version::{
    CommitVersion, ContentDigest, ScopeVersionVector, ShardId, TokenizerVersions,
};

use crate::budget::QueryBudget;
use crate::coverage::{CoverageDescriptor, ShardServing};
use crate::cursor::validate_cursor;
use crate::error::{CursorInvalidReason, SearchError};
use crate::hit::{QueryOrder, SearchHit, SortPosition};
use crate::index::{verify_index_digest, verify_tokenizer_versions};
use crate::merge::merge_streams;
use crate::pin::{PinRetention, SearchPin};
use crate::result::{ResultMode, SearchPage};

/// The answer from one targeted shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardResponse {
    shard_id: ShardId,
    served_version: Option<CommitVersion>,
    oldest_readable: Option<CommitVersion>,
    hits: Vec<SearchHit>,
    index_digest: Option<ContentDigest>,
    tokenizer_versions: Option<TokenizerVersions>,
}

impl ShardResponse {
    /// Creates a response from one shard that served the pin.
    #[must_use]
    pub fn served(shard_id: ShardId, served_version: CommitVersion, hits: Vec<SearchHit>) -> Self {
        Self {
            shard_id,
            served_version: Some(served_version),
            oldest_readable: None,
            hits,
            index_digest: None,
            tokenizer_versions: None,
        }
    }

    /// Creates a response from one unavailable shard.
    #[must_use]
    pub fn unavailable(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            served_version: None,
            oldest_readable: None,
            hits: Vec::new(),
            index_digest: None,
            tokenizer_versions: None,
        }
    }

    /// Creates a response from one shard that cannot serve the pin.
    #[must_use]
    pub fn pin_expired(shard_id: ShardId, oldest_readable: CommitVersion) -> Self {
        Self {
            shard_id,
            served_version: None,
            oldest_readable: Some(oldest_readable),
            hits: Vec::new(),
            index_digest: None,
            tokenizer_versions: None,
        }
    }

    /// Sets the oldest readable version for this shard.
    #[must_use]
    pub fn with_oldest_readable(mut self, oldest_readable: CommitVersion) -> Self {
        self.oldest_readable = Some(oldest_readable);
        self
    }

    /// Sets the index digest for this shard.
    #[must_use]
    pub fn with_index_digest(mut self, index_digest: ContentDigest) -> Self {
        self.index_digest = Some(index_digest);
        self
    }

    /// Sets the tokenizer version map for this shard.
    #[must_use]
    pub fn with_tokenizer_versions(mut self, tokenizer_versions: TokenizerVersions) -> Self {
        self.tokenizer_versions = Some(tokenizer_versions);
        self
    }

    /// Returns the shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the served commit version.
    #[must_use]
    pub const fn served_version(&self) -> Option<CommitVersion> {
        self.served_version
    }

    /// Returns the oldest readable commit version.
    #[must_use]
    pub const fn oldest_readable(&self) -> Option<CommitVersion> {
        self.oldest_readable
    }

    /// Returns the ordered hits from this shard.
    #[must_use]
    pub fn hits(&self) -> &[SearchHit] {
        &self.hits
    }

    /// Returns the index digest from this shard.
    #[must_use]
    pub const fn index_digest(&self) -> Option<ContentDigest> {
        self.index_digest
    }

    /// Returns the tokenizer version map from this shard.
    #[must_use]
    pub const fn tokenizer_versions(&self) -> Option<&TokenizerVersions> {
        self.tokenizer_versions.as_ref()
    }
}

/// One search request.
///
/// The default mode is exact. The default order is relevance. The default page
/// limit is ten hits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    pin: SearchPin,
    mode: ResultMode,
    order: QueryOrder,
    limit: usize,
    cursor: Option<SearchCursor>,
    targets: Vec<ShardId>,
    fanout_limit: usize,
    budget: QueryBudget,
    retention: PinRetention,
    index_expectations: BTreeMap<ShardId, ContentDigest>,
    tokenizer_expectation: Option<TokenizerVersions>,
}

impl SearchRequest {
    /// Creates an exact relevance request for one target set.
    ///
    /// The constructor sorts and deduplicates the target list. The fan-out
    /// limit defaults to the number of unique targets.
    #[must_use]
    pub fn new(pin: SearchPin, targets: impl IntoIterator<Item = ShardId>) -> Self {
        let mut targets: Vec<ShardId> = targets.into_iter().collect();
        targets.sort();
        targets.dedup();
        let fanout_limit = targets.len();
        Self {
            pin,
            mode: ResultMode::Exact,
            order: QueryOrder::Relevance,
            limit: 10,
            cursor: None,
            targets,
            fanout_limit,
            budget: QueryBudget::unlimited(),
            retention: PinRetention::unbounded(),
            index_expectations: BTreeMap::new(),
            tokenizer_expectation: None,
        }
    }

    /// Sets the result mode.
    #[must_use]
    pub fn with_mode(mut self, mode: ResultMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the query order.
    #[must_use]
    pub fn with_order(mut self, order: QueryOrder) -> Self {
        self.order = order;
        self
    }

    /// Sets the page limit.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Sets the exact pagination cursor.
    #[must_use]
    pub fn with_cursor(mut self, cursor: SearchCursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Sets the fan-out limit.
    #[must_use]
    pub fn with_fanout_limit(mut self, fanout_limit: usize) -> Self {
        self.fanout_limit = fanout_limit;
        self
    }

    /// Sets the query budget.
    #[must_use]
    pub fn with_budget(mut self, budget: QueryBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Sets the pin retention floor.
    #[must_use]
    pub fn with_retention(mut self, retention: PinRetention) -> Self {
        self.retention = retention;
        self
    }

    /// Sets the expected index digest for one shard.
    #[must_use]
    pub fn with_index_expectation(
        mut self,
        shard_id: ShardId,
        index_digest: ContentDigest,
    ) -> Self {
        self.index_expectations.insert(shard_id, index_digest);
        self
    }

    /// Sets the expected tokenizer version map.
    #[must_use]
    pub fn with_tokenizer_expectation(mut self, tokenizers: TokenizerVersions) -> Self {
        self.tokenizer_expectation = Some(tokenizers);
        self
    }

    /// Returns the search pin.
    #[must_use]
    pub const fn pin(&self) -> &SearchPin {
        &self.pin
    }

    /// Returns the result mode.
    #[must_use]
    pub const fn mode(&self) -> ResultMode {
        self.mode
    }

    /// Returns the query order.
    #[must_use]
    pub const fn order(&self) -> QueryOrder {
        self.order
    }

    /// Returns the page limit.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the exact pagination cursor.
    #[must_use]
    pub const fn cursor(&self) -> Option<&SearchCursor> {
        self.cursor.as_ref()
    }

    /// Returns the targeted shard identifiers.
    #[must_use]
    pub fn targets(&self) -> &[ShardId] {
        &self.targets
    }

    /// Returns the fan-out limit.
    #[must_use]
    pub const fn fanout_limit(&self) -> usize {
        self.fanout_limit
    }

    /// Returns the query budget.
    #[must_use]
    pub const fn budget(&self) -> &QueryBudget {
        &self.budget
    }

    /// Returns the pin retention floor.
    #[must_use]
    pub const fn retention(&self) -> &PinRetention {
        &self.retention
    }
}

/// Executes one search over the shard responses.
///
/// The function returns exactly one page. It returns no page on any error.
///
/// # Errors
///
/// Returns a named [`SearchError`] for an expired pin, an invalid cursor, a
/// fan-out overrun, an unavailable shard in exact mode, an index or tokenizer
/// mismatch, or an exhausted query budget.
pub fn execute(
    request: &SearchRequest,
    responses: &[ShardResponse],
) -> Result<SearchPage, SearchError> {
    if request.targets.len() > request.fanout_limit {
        return Err(SearchError::FanoutExceeded {
            requested: request.targets.len(),
            limit: request.fanout_limit,
        });
    }

    if request.mode.is_coverage() && request.cursor.is_some() {
        return Err(SearchError::CursorInvalid {
            reason: CursorInvalidReason::CoverageCursor,
        });
    }

    if let Some(cursor) = &request.cursor {
        validate_cursor(cursor, &request.pin, request.order, &request.retention)?;
    }
    request.pin.check(&request.retention)?;

    let after = request
        .cursor
        .as_ref()
        .map(|cursor| SortPosition::from_sort_tuple(request.order, &cursor.sort_tuple))
        .transpose()?;

    let mut answered = Vec::new();
    let mut missing = Vec::new();
    let mut streams = Vec::new();

    for target in &request.targets {
        let Some(requested_version) = request.pin.version_vector().get(target) else {
            if request.mode.is_exact() {
                return Err(SearchError::PinMissingShard {
                    shard_id: target.clone(),
                });
            }
            missing.push(target.clone());
            continue;
        };

        let Some(response) = responses
            .iter()
            .find(|response| response.shard_id() == target)
        else {
            if request.mode.is_exact() {
                return Err(SearchError::ShardUnavailable {
                    shard_id: target.clone(),
                });
            }
            missing.push(target.clone());
            continue;
        };

        if response.served_version != Some(requested_version) {
            if request.mode.is_exact() {
                return Err(response_error(request, response));
            }
            missing.push(target.clone());
            continue;
        }

        verify_identities(request, response)?;

        let hits: Vec<SearchHit> = response
            .hits
            .iter()
            .filter(|hit| hit.commit_version() <= requested_version)
            .cloned()
            .collect();
        answered.push(ShardServing::new(target.clone(), requested_version));
        streams.push((target.clone(), hits));
    }

    let coverage = request
        .mode
        .is_coverage()
        .then(|| CoverageDescriptor::new(answered, missing));

    let mut budget = request.budget.clone();
    for (shard_id, hits) in &streams {
        let cost = u64::try_from(hits.len()).unwrap_or(u64::MAX);
        if let Err(error) = budget.charge(shard_id, cost) {
            return Err(with_coverage(error, coverage.as_ref()));
        }
    }

    let merged = merge_streams(request.order, &streams, after.as_ref(), request.limit)?;
    let next_cursor = if request.mode.is_exact() && merged.has_more {
        merged.hits.last().map(|hit| {
            SearchCursor::new(
                request.pin.version_vector().clone(),
                request.pin.query_fingerprint(),
                hit.sort_tuple(request.order),
            )
        })
    } else {
        None
    };

    Ok(SearchPage::new(
        merged.hits,
        request.mode,
        coverage,
        next_cursor,
        budget,
    ))
}

fn verify_identities(request: &SearchRequest, response: &ShardResponse) -> Result<(), SearchError> {
    if let Some(expected) = request.index_expectations.get(response.shard_id()) {
        verify_index_digest(response.shard_id(), *expected, response.index_digest)?;
    }
    if let Some(expected) = &request.tokenizer_expectation {
        match response.tokenizer_versions() {
            Some(found) => verify_tokenizer_versions(expected, found)?,
            None => {
                return Err(SearchError::TokenizerUnknown {
                    field: String::from("*"),
                });
            }
        }
    }
    Ok(())
}

fn response_error(request: &SearchRequest, response: &ShardResponse) -> SearchError {
    if let Some(oldest_readable) = response.oldest_readable() {
        let mut oldest_vector = request.retention.oldest_readable().as_vector().clone();
        oldest_vector.set(response.shard_id().clone(), oldest_readable);
        SearchError::PinExpired {
            requested: request.pin.version_vector().clone(),
            oldest_readable: ScopeVersionVector::from_vector(oldest_vector),
        }
    } else {
        SearchError::ShardUnavailable {
            shard_id: response.shard_id().clone(),
        }
    }
}

fn with_coverage(error: SearchError, coverage: Option<&CoverageDescriptor>) -> SearchError {
    match error {
        SearchError::QueryBudgetExceeded {
            required, limit, ..
        } => SearchError::QueryBudgetExceeded {
            required,
            limit,
            coverage: coverage.cloned(),
        },
        other => other,
    }
}
