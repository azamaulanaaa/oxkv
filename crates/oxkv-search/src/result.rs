//! Result modes and one search page.
//!
//! Exact mode is the default. A coverage result carries a coverage descriptor
//! and an explicit partial flag. A coverage page never carries an exact
//! pagination cursor.

use crate::budget::QueryBudget;
use crate::coverage::CoverageDescriptor;
use crate::cursor::encode_cursor;
use crate::hit::SearchHit;
use oxkv_contracts::cursor::SearchCursor;

/// The result mode of one search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ResultMode {
    /// Every targeted shard serves the pin.
    #[default]
    Exact,
    /// The result carries a coverage descriptor and is partial.
    Coverage,
}

impl ResultMode {
    /// Returns the canonical mode name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Coverage => "coverage",
        }
    }

    /// Returns true for exact mode.
    #[must_use]
    pub const fn is_exact(self) -> bool {
        matches!(self, Self::Exact)
    }

    /// Returns true for coverage mode.
    #[must_use]
    pub const fn is_coverage(self) -> bool {
        matches!(self, Self::Coverage)
    }
}

/// One page of search hits.
///
/// An exact page has no coverage descriptor and is not partial. A coverage
/// page always has a coverage descriptor and is always partial. Only an exact
/// page can carry an exact pagination cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    hits: Vec<SearchHit>,
    mode: ResultMode,
    partial: bool,
    coverage: Option<CoverageDescriptor>,
    next_cursor: Option<SearchCursor>,
    budget: QueryBudget,
}

impl SearchPage {
    pub(crate) const fn new(
        hits: Vec<SearchHit>,
        mode: ResultMode,
        coverage: Option<CoverageDescriptor>,
        next_cursor: Option<SearchCursor>,
        budget: QueryBudget,
    ) -> Self {
        Self {
            hits,
            mode,
            partial: mode.is_coverage(),
            coverage,
            next_cursor,
            budget,
        }
    }

    /// Returns the page hits in comparator order.
    #[must_use]
    pub fn hits(&self) -> &[SearchHit] {
        &self.hits
    }

    /// Returns the result mode.
    #[must_use]
    pub const fn mode(&self) -> ResultMode {
        self.mode
    }

    /// Returns true when the page is partial.
    #[must_use]
    pub const fn is_partial(&self) -> bool {
        self.partial
    }

    /// Returns the coverage descriptor in coverage mode.
    #[must_use]
    pub const fn coverage(&self) -> Option<&CoverageDescriptor> {
        self.coverage.as_ref()
    }

    /// Returns the exact next cursor.
    #[must_use]
    pub const fn next_cursor(&self) -> Option<&SearchCursor> {
        self.next_cursor.as_ref()
    }

    /// Encodes the exact next cursor as one opaque token.
    #[must_use]
    pub fn next_cursor_token(&self) -> Option<Vec<u8>> {
        self.next_cursor.as_ref().map(encode_cursor)
    }

    /// Returns the query budget after the search.
    ///
    /// The budget carries the total cost and the per-shard frontiers. The cost
    /// crate uses this value to build an explain record.
    #[must_use]
    pub const fn budget(&self) -> &QueryBudget {
        &self.budget
    }
}
