//! Search hits and the two query comparators.
//!
//! A relevance query uses `(score descending, canonical key ascending)`. A
//! declared query uses `(declared sort value ascending, canonical key
//! ascending)`. The shard identifier is not part of either order.

use core::cmp::Ordering;

use oxkv_contracts::cursor::{SortTuple, SortValue};
use oxkv_contracts::version::{CommitVersion, ShardId};

use crate::error::{CursorInvalidReason, SearchError};

/// The comparator that a query uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QueryOrder {
    /// Score descending, then canonical key ascending.
    #[default]
    Relevance,
    /// Declared sort value ascending, then canonical key ascending.
    Declared,
}

impl QueryOrder {
    /// Returns the canonical order name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Declared => "declared",
        }
    }

    /// Returns the stable order tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Self::Relevance => 1,
            Self::Declared => 2,
        }
    }
}

/// One search hit.
///
/// The canonical key is the final tie-break for every order. The shard
/// identifier is provenance only. It does not affect the order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    canonical_key: Vec<u8>,
    score: i64,
    sort_key: SortValue,
    shard_id: ShardId,
    commit_version: CommitVersion,
}

impl SearchHit {
    /// Creates a relevance hit.
    ///
    /// The sort key is an empty byte value. A relevance order ignores it.
    #[must_use]
    pub fn new(
        canonical_key: impl Into<Vec<u8>>,
        score: i64,
        shard_id: ShardId,
        commit_version: CommitVersion,
    ) -> Self {
        Self {
            canonical_key: canonical_key.into(),
            score,
            sort_key: SortValue::Bytes(Vec::new()),
            shard_id,
            commit_version,
        }
    }

    /// Sets the declared sort value for this hit.
    #[must_use]
    pub fn with_sort_key(mut self, sort_key: SortValue) -> Self {
        self.sort_key = sort_key;
        self
    }

    /// Returns the canonical key bytes.
    #[must_use]
    pub fn canonical_key(&self) -> &[u8] {
        &self.canonical_key
    }

    /// Returns the integer score.
    #[must_use]
    pub const fn score(&self) -> i64 {
        self.score
    }

    /// Returns the declared sort value.
    #[must_use]
    pub const fn sort_key(&self) -> &SortValue {
        &self.sort_key
    }

    /// Returns the owning shard identifier.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the commit version of the hit.
    #[must_use]
    pub const fn commit_version(&self) -> CommitVersion {
        self.commit_version
    }

    /// Builds the full sort tuple for this hit.
    #[must_use]
    pub fn sort_tuple(&self, order: QueryOrder) -> SortTuple {
        match order {
            QueryOrder::Relevance => SortTuple::new(vec![
                SortValue::Integer(self.score),
                SortValue::Bytes(self.canonical_key.clone()),
            ]),
            QueryOrder::Declared => SortTuple::new(vec![
                self.sort_key.clone(),
                SortValue::Bytes(self.canonical_key.clone()),
            ]),
        }
    }
}

/// Compares two declared sort values.
///
/// Values of the same type compare by their natural order. Values of different
/// types compare by a stable type tag. A query uses one sort type.
#[must_use]
pub fn compare_sort_values(left: &SortValue, right: &SortValue) -> Ordering {
    match (left, right) {
        (SortValue::Text(left), SortValue::Text(right)) => left.cmp(right),
        (SortValue::Bytes(left), SortValue::Bytes(right)) => left.cmp(right),
        (SortValue::Integer(left), SortValue::Integer(right)) => left.cmp(right),
        _ => sort_type_rank(left).cmp(&sort_type_rank(right)),
    }
}

const fn sort_type_rank(value: &SortValue) -> u8 {
    match value {
        SortValue::Text(_) => 1,
        SortValue::Bytes(_) => 2,
        SortValue::Integer(_) => 3,
    }
}

/// Compares two hits with the query order.
#[must_use]
pub fn compare_hits(order: QueryOrder, left: &SearchHit, right: &SearchHit) -> Ordering {
    match order {
        QueryOrder::Relevance => right
            .score
            .cmp(&left.score)
            .then_with(|| left.canonical_key.cmp(&right.canonical_key)),
        QueryOrder::Declared => compare_sort_values(&left.sort_key, &right.sort_key)
            .then_with(|| left.canonical_key.cmp(&right.canonical_key)),
    }
}

/// The position of the last hit on a page.
///
/// A cursor carries this position as a [`SortTuple`]. The position lets the
/// next page resume after the last hit without an offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortPosition {
    /// A relevance position.
    Relevance {
        /// The score of the last hit.
        score: i64,
        /// The canonical key of the last hit.
        key: Vec<u8>,
    },
    /// A declared position.
    Declared {
        /// The declared sort value of the last hit.
        sort_key: SortValue,
        /// The canonical key of the last hit.
        key: Vec<u8>,
    },
}

impl SortPosition {
    /// Builds a position from a cursor sort tuple.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::CursorInvalid`] when the tuple shape does not
    /// match the order.
    pub fn from_sort_tuple(order: QueryOrder, tuple: &SortTuple) -> Result<Self, SearchError> {
        let invalid = || SearchError::CursorInvalid {
            reason: CursorInvalidReason::SortTupleMismatch,
        };
        match (order, tuple.values()) {
            (QueryOrder::Relevance, [SortValue::Integer(score), SortValue::Bytes(key)]) => {
                Ok(Self::Relevance {
                    score: *score,
                    key: key.clone(),
                })
            }
            (QueryOrder::Declared, [sort_key, SortValue::Bytes(key)]) => Ok(Self::Declared {
                sort_key: sort_key.clone(),
                key: key.clone(),
            }),
            _ => Err(invalid()),
        }
    }

    /// Builds a position from one hit.
    #[must_use]
    pub fn from_hit(order: QueryOrder, hit: &SearchHit) -> Self {
        match order {
            QueryOrder::Relevance => Self::Relevance {
                score: hit.score,
                key: hit.canonical_key.clone(),
            },
            QueryOrder::Declared => Self::Declared {
                sort_key: hit.sort_key.clone(),
                key: hit.canonical_key.clone(),
            },
        }
    }

    /// Returns true when the hit sorts strictly after this position.
    #[must_use]
    pub fn precedes(&self, order: QueryOrder, hit: &SearchHit) -> bool {
        match (order, self) {
            (
                QueryOrder::Relevance,
                Self::Relevance {
                    score: position_score,
                    key: position_key,
                },
            ) => {
                hit.score < *position_score
                    || (hit.score == *position_score
                        && hit.canonical_key.as_slice() > position_key.as_slice())
            }
            (
                QueryOrder::Declared,
                Self::Declared {
                    sort_key: position_sort,
                    key: position_key,
                },
            ) => {
                compare_sort_values(position_sort, &hit.sort_key)
                    .then_with(|| position_key.as_slice().cmp(hit.canonical_key.as_slice()))
                    == Ordering::Less
            }
            _ => false,
        }
    }

    /// Returns the canonical key of this position.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        match self {
            Self::Relevance { key, .. } | Self::Declared { key, .. } => key,
        }
    }
}
