//! Exact and coverage result modes.
//!
//! Exact mode is the default. It requires every targeted shard to serve the
//! pin. If one shard cannot, exact mode returns a named error and no data.
//! Coverage mode is opt-in. A coverage result carries a descriptor with the
//! answered shards, the missing shards, and the served versions. A coverage
//! page never advances an exact cursor.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::{CommitVersion, CostCursor, ScopeVersionVector, ShardId};

use crate::budget::CostBudget;
use crate::cursor::CostCursorExt;
use crate::error::{CostCode, CostError};
use crate::merge::{MergeOptions, MergeOrder, QueryRow, ShardStream, merge_streams};
use crate::util::json_hex;

/// The result mode of one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ResultMode {
    /// Every targeted shard must serve the pin.
    #[default]
    Exact,
    /// A partial result carries an explicit coverage descriptor.
    Coverage,
}

impl ResultMode {
    /// Returns the stable contract text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Coverage => "coverage",
        }
    }
}

impl fmt::Display for ResultMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One shard reply in a scatter-gather.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardReply {
    /// The shard identifier.
    pub shard_id: ShardId,
    /// The served commit version. A value of `None` means the shard is
    /// unavailable.
    pub served_version: Option<CommitVersion>,
    /// The rows from the shard.
    pub rows: Vec<QueryRow>,
    /// The observed latency in abstract ticks. The merge ignores it.
    pub latency_ticks: u64,
}

impl ShardReply {
    /// Creates one served reply.
    #[must_use]
    pub const fn served(
        shard_id: ShardId,
        served_version: CommitVersion,
        rows: Vec<QueryRow>,
    ) -> Self {
        Self {
            shard_id,
            served_version: Some(served_version),
            rows,
            latency_ticks: 0,
        }
    }

    /// Creates one unavailable reply.
    #[must_use]
    pub const fn unavailable(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            served_version: None,
            rows: Vec::new(),
            latency_ticks: 0,
        }
    }

    /// Sets the observed latency.
    #[must_use]
    pub const fn with_latency_ticks(mut self, latency_ticks: u64) -> Self {
        self.latency_ticks = latency_ticks;
        self
    }
}

/// The coverage of one result set.
///
/// The descriptor lists every answered shard with its served version and every
/// missing shard. The map order and the set order follow shard byte order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageDescriptor {
    answered: BTreeMap<ShardId, CommitVersion>,
    missing: BTreeSet<ShardId>,
}

impl CoverageDescriptor {
    /// Creates one descriptor.
    ///
    /// A shard that appears in both lists counts as answered.
    #[must_use]
    pub fn new(answered: BTreeMap<ShardId, CommitVersion>, missing: BTreeSet<ShardId>) -> Self {
        let missing = missing
            .into_iter()
            .filter(|shard_id| !answered.contains_key(shard_id))
            .collect();
        Self { answered, missing }
    }

    /// Returns the answered shards and their served versions.
    pub fn answered(&self) -> impl Iterator<Item = (&ShardId, CommitVersion)> {
        self.answered
            .iter()
            .map(|(shard_id, version)| (shard_id, *version))
    }

    /// Returns the missing shards.
    pub fn missing(&self) -> impl Iterator<Item = &ShardId> {
        self.missing.iter()
    }

    /// Returns the served version of one answered shard.
    #[must_use]
    pub fn served_version(&self, shard_id: &ShardId) -> Option<CommitVersion> {
        self.answered.get(shard_id).copied()
    }

    /// Returns the served versions.
    #[must_use]
    pub const fn served_versions(&self) -> &BTreeMap<ShardId, CommitVersion> {
        &self.answered
    }

    /// Returns true when every targeted shard answered.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    /// Returns the answered shard count.
    #[must_use]
    pub fn answered_count(&self) -> usize {
        self.answered.len()
    }

    /// Returns the missing shard count.
    #[must_use]
    pub fn missing_count(&self) -> usize {
        self.missing.len()
    }

    /// Renders the canonical JSON form.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        let answered: Vec<String> = self
            .answered
            .iter()
            .map(|(shard_id, version)| {
                let mut entry = String::from("{\"shard_id\":");
                entry.push_str(&json_hex(shard_id.as_bytes()));
                entry.push_str(",\"served_version\":");
                entry.push_str(&version.get().to_string());
                entry.push('}');
                entry
            })
            .collect();
        let missing: Vec<String> = self
            .missing
            .iter()
            .map(|shard_id| json_hex(shard_id.as_bytes()))
            .collect();
        let answered = array(&answered);
        let missing = array(&missing);
        format!("{{\"answered\":{answered},\"missing\":{missing}}}")
    }
}

fn array(items: &[String]) -> String {
    let mut text = String::from("[");
    text.push_str(&items.join(","));
    text.push(']');
    text
}

/// One page request.
#[derive(Debug, Clone)]
pub struct PageRequest<'a> {
    /// The result mode.
    pub mode: ResultMode,
    /// The pinned version vector.
    pub pin: &'a ScopeVersionVector,
    /// The merge order.
    pub order: MergeOrder,
    /// The maximum row count.
    pub limit: usize,
    /// The active budget.
    pub budget: &'a CostBudget,
    /// An exact cursor from a previous page.
    pub cursor: Option<&'a CostCursor>,
}

impl<'a> PageRequest<'a> {
    /// Creates one page request.
    #[must_use]
    pub const fn new(
        mode: ResultMode,
        pin: &'a ScopeVersionVector,
        order: MergeOrder,
        limit: usize,
        budget: &'a CostBudget,
        cursor: Option<&'a CostCursor>,
    ) -> Self {
        Self {
            mode,
            pin,
            order,
            limit,
            budget,
            cursor,
        }
    }
}

/// One query page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPage {
    /// The result mode.
    pub mode: ResultMode,
    /// The rows in final order.
    pub rows: Vec<QueryRow>,
    /// The coverage descriptor. It is present in coverage mode only.
    pub coverage: Option<CoverageDescriptor>,
    /// The advanced exact cursor. It is present in exact mode only.
    pub cursor: Option<CostCursor>,
    /// True when the page is partial.
    pub partial: bool,
}

impl QueryPage {
    /// Returns true when the page is partial.
    #[must_use]
    pub const fn is_partial(&self) -> bool {
        self.partial
    }
}

/// A page failure with optional coverage.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{error}")]
pub struct PageError {
    /// The named cost error.
    #[source]
    pub error: CostError,
    /// The coverage that was known when the page failed.
    pub coverage: Option<CoverageDescriptor>,
}

impl PageError {
    /// Returns the frozen error code.
    #[must_use]
    pub const fn code(&self) -> CostCode {
        self.error.code()
    }

    /// Returns the coverage descriptor.
    #[must_use]
    pub const fn coverage(&self) -> Option<&CoverageDescriptor> {
        self.coverage.as_ref()
    }
}

impl From<CostError> for PageError {
    fn from(error: CostError) -> Self {
        Self {
            error,
            coverage: None,
        }
    }
}

/// Assembles one exact or coverage page from shard replies.
///
/// The method ignores `latency_ticks`. A blocked or slow shard cannot change
/// the merge order. The method checks the fan-out limit before it merges rows.
///
/// # Errors
///
/// Returns [`PageError`] with [`CostCode::SearchFanoutExceeded`] when the
/// fan-out is above the limit. Returns [`PageError`] with
/// [`CostCode::ScopeShardUnreachable`] when exact mode has an unavailable
/// shard. Returns [`PageError`] with [`CostCode::SearchPinExpired`] when a
/// shard does not serve the pin. Returns [`PageError`] with
/// [`CostCode::QueryBudgetExceeded`] when the page is above the budget.
pub fn assemble_page(
    request: &PageRequest<'_>,
    replies: &[ShardReply],
) -> Result<QueryPage, PageError> {
    let replies_by_shard = index_replies(replies)?;
    let target_ids = target_ids(request, replies);
    if target_ids.len() > request.budget.fanout_limit() {
        return Err(PageError::from(CostError::FanoutExceeded {
            requested: crate::util::len_to_u32(target_ids.len()),
            limit: request.budget.max_fanout,
        }));
    }
    let descriptor = build_descriptor(&target_ids, &replies_by_shard);
    if request.mode == ResultMode::Exact {
        check_exact(request, &target_ids, &replies_by_shard, &descriptor)?;
    }
    let output = merge_replies(request, &target_ids, &replies_by_shard, &descriptor)?;
    let advanced = advance_cursor(request, &output.frontiers, &descriptor)?;
    let (coverage, cursor, partial) = match request.mode {
        ResultMode::Exact => (None, Some(advanced), false),
        ResultMode::Coverage => (Some(descriptor), None, true),
    };
    Ok(QueryPage {
        mode: request.mode,
        rows: output.rows,
        coverage,
        cursor,
        partial,
    })
}

fn index_replies(replies: &[ShardReply]) -> Result<BTreeMap<ShardId, &ShardReply>, PageError> {
    let mut replies_by_shard = BTreeMap::new();
    for reply in replies {
        if replies_by_shard
            .insert(reply.shard_id.clone(), reply)
            .is_some()
        {
            return Err(PageError::from(CostError::DuplicateShard {
                shard_id: reply.shard_id.clone(),
            }));
        }
    }
    Ok(replies_by_shard)
}

fn target_ids(request: &PageRequest<'_>, replies: &[ShardReply]) -> BTreeSet<ShardId> {
    let mut target_ids: BTreeSet<ShardId> = request
        .pin
        .as_vector()
        .iter()
        .map(|(shard_id, _)| shard_id.clone())
        .collect();
    for reply in replies {
        target_ids.insert(reply.shard_id.clone());
    }
    target_ids
}

fn build_descriptor(
    target_ids: &BTreeSet<ShardId>,
    replies: &BTreeMap<ShardId, &ShardReply>,
) -> CoverageDescriptor {
    let mut answered = BTreeMap::new();
    let mut missing = BTreeSet::new();
    for shard_id in target_ids {
        match replies.get(shard_id) {
            Some(reply) => match reply.served_version {
                Some(version) => {
                    answered.insert(shard_id.clone(), version);
                }
                None => {
                    missing.insert(shard_id.clone());
                }
            },
            None => {
                missing.insert(shard_id.clone());
            }
        }
    }
    CoverageDescriptor::new(answered, missing)
}

fn check_exact(
    request: &PageRequest<'_>,
    target_ids: &BTreeSet<ShardId>,
    replies: &BTreeMap<ShardId, &ShardReply>,
    descriptor: &CoverageDescriptor,
) -> Result<(), PageError> {
    for shard_id in target_ids {
        let Some(reply) = replies.get(shard_id) else {
            return Err(PageError {
                error: CostError::ShardUnavailable {
                    shard_id: shard_id.clone(),
                },
                coverage: Some(descriptor.clone()),
            });
        };
        let Some(served) = reply.served_version else {
            return Err(PageError {
                error: CostError::ShardUnavailable {
                    shard_id: shard_id.clone(),
                },
                coverage: Some(descriptor.clone()),
            });
        };
        let Some(requested) = request.pin.get(shard_id) else {
            return Err(PageError {
                error: CostError::ShardNotPinned {
                    shard_id: shard_id.clone(),
                },
                coverage: Some(descriptor.clone()),
            });
        };
        if served != requested {
            return Err(PageError {
                error: CostError::PinExpired {
                    shard_id: shard_id.clone(),
                    requested,
                    served,
                },
                coverage: Some(descriptor.clone()),
            });
        }
    }
    Ok(())
}

fn merge_replies(
    request: &PageRequest<'_>,
    target_ids: &BTreeSet<ShardId>,
    replies: &BTreeMap<ShardId, &ShardReply>,
    descriptor: &CoverageDescriptor,
) -> Result<crate::merge::MergeOutput, PageError> {
    let streams: Vec<ShardStream> = target_ids
        .iter()
        .filter_map(|shard_id| {
            let reply = replies.get(shard_id)?;
            reply.served_version?;
            Some(
                ShardStream::new(shard_id.clone(), reply.rows.clone())
                    .with_latency_ticks(reply.latency_ticks),
            )
        })
        .collect();
    let options = MergeOptions::new(request.order, request.limit, request.budget.fanout_limit());
    merge_streams(&streams, options).map_err(|error| PageError {
        error,
        coverage: Some(descriptor.clone()),
    })
}

fn advance_cursor(
    request: &PageRequest<'_>,
    frontiers: &BTreeMap<ShardId, oxkv_contracts::CostFrontier>,
    descriptor: &CoverageDescriptor,
) -> Result<CostCursor, PageError> {
    let base = request.cursor.map_or_else(
        || CostCursor::from_budget(request.pin, request.budget),
        Clone::clone,
    );
    base.after_page(frontiers).map_err(|error| PageError {
        error,
        coverage: Some(descriptor.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::{MergeOrder, QueryRow};
    use oxkv_contracts::{CursorToken, SortTuple, VersionVector};

    fn vector(entries: &[(&[u8], u64)]) -> ScopeVersionVector {
        ScopeVersionVector::from_vector(VersionVector::from_entries(
            entries.iter().map(|(shard, version)| {
                (ShardId::new(shard.to_vec()), CommitVersion::new(*version))
            }),
        ))
    }

    fn row(shard: u8, key: &[u8], score: i64) -> QueryRow {
        QueryRow::new(
            ShardId::new(vec![shard]),
            key.to_vec(),
            score,
            SortTuple::default(),
            Vec::new(),
        )
    }

    fn budget() -> CostBudget {
        CostBudget::new(10_000, 4, 100)
    }

    #[test]
    fn exact_mode_is_the_default() {
        assert_eq!(ResultMode::default(), ResultMode::Exact);
    }

    #[test]
    fn exact_mode_returns_an_error_when_one_shard_is_unavailable() {
        let pin = vector(&[(&[1], 5), (&[2], 5)]);
        let budget = budget();
        let request = PageRequest::new(
            ResultMode::Exact,
            &pin,
            MergeOrder::Search,
            10,
            &budget,
            None,
        );
        let replies = vec![
            ShardReply::served(
                ShardId::new(vec![1]),
                CommitVersion::new(5),
                vec![row(1, b"a", 1)],
            ),
            ShardReply::unavailable(ShardId::new(vec![2])),
        ];
        let error = assemble_page(&request, &replies).expect_err("the shard is unavailable");
        assert_eq!(error.code(), CostCode::ScopeShardUnreachable);
        assert!(error.coverage().is_some());
    }

    #[test]
    fn coverage_mode_lists_the_missing_shard_and_stays_partial() {
        let pin = vector(&[(&[1], 5), (&[2], 5)]);
        let budget = budget();
        let request = PageRequest::new(
            ResultMode::Coverage,
            &pin,
            MergeOrder::Search,
            10,
            &budget,
            None,
        );
        let replies = vec![
            ShardReply::served(
                ShardId::new(vec![1]),
                CommitVersion::new(5),
                vec![row(1, b"a", 1)],
            ),
            ShardReply::unavailable(ShardId::new(vec![2])),
        ];
        let page = assemble_page(&request, &replies).expect("coverage mode returns a page");
        assert!(page.partial);
        assert!(page.cursor.is_none());
        let coverage = page.coverage.expect("the page has a descriptor");
        assert_eq!(
            coverage.missing().collect::<Vec<_>>(),
            vec![&ShardId::new(vec![2])]
        );
        assert_eq!(
            coverage.served_version(&ShardId::new(vec![1])),
            Some(CommitVersion::new(5))
        );
    }

    #[test]
    fn a_coverage_page_does_not_advance_an_exact_cursor() {
        let pin = vector(&[(&[1], 5), (&[2], 5)]);
        let budget = budget();
        let exact_request = PageRequest::new(
            ResultMode::Exact,
            &pin,
            MergeOrder::Search,
            10,
            &budget,
            None,
        );
        let replies = vec![
            ShardReply::served(
                ShardId::new(vec![1]),
                CommitVersion::new(5),
                vec![row(1, b"a", 1)],
            ),
            ShardReply::served(
                ShardId::new(vec![2]),
                CommitVersion::new(5),
                vec![row(2, b"b", 2)],
            ),
        ];
        let exact = assemble_page(&exact_request, &replies).expect("the exact page fits");
        let cursor = exact.cursor.expect("the exact page has a cursor");
        let before = cursor.to_token();
        let coverage_request = PageRequest::new(
            ResultMode::Coverage,
            &pin,
            MergeOrder::Search,
            10,
            &budget,
            Some(&cursor),
        );
        let page = assemble_page(&coverage_request, &replies).expect("the coverage page fits");
        assert!(page.cursor.is_none());
        assert_eq!(cursor.to_token(), before);
    }
}
