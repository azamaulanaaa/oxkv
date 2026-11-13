//! The deterministic scatter-gather merge.
//!
//! A search merges under `(score descending, canonical key ascending)`. A
//! general query merges under `(declared sort key ascending, canonical key
//! ascending)`. The shard identifier is provenance only. It is not a semantic
//! tie-break.

use core::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use oxkv_contracts::{CostFrontier, ShardId, SortTuple};

use crate::error::CostError;
use crate::util::{len_to_u32, len_to_u64};

/// The total order of one query class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergeOrder {
    /// Merge under `(score descending, canonical key ascending)`.
    Search,
    /// Merge under `(declared sort key ascending, canonical key ascending)`.
    General,
}

impl MergeOrder {
    /// Returns the stable contract text.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::General => "general",
        }
    }
}

/// One query row from one shard.
///
/// The canonical key is the final tie-break. The shard identifier stays on the
/// row for provenance. The comparator ignores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryRow {
    /// The shard that returned the row.
    pub shard_id: ShardId,
    /// The canonical key bytes.
    pub canonical_key: Vec<u8>,
    /// The integer search score.
    pub score: i64,
    /// The declared general sort key.
    pub sort_key: SortTuple,
    /// The opaque row payload.
    pub payload: Vec<u8>,
}

impl QueryRow {
    /// Creates one query row.
    #[must_use]
    pub const fn new(
        shard_id: ShardId,
        canonical_key: Vec<u8>,
        score: i64,
        sort_key: SortTuple,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            shard_id,
            canonical_key,
            score,
            sort_key,
            payload,
        }
    }
}

/// One ordered shard stream.
///
/// `latency_ticks` is a test and telemetry value. The merge never reads it.
/// A blocked or slow shard therefore cannot change the output order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardStream {
    /// The shard identifier.
    pub shard_id: ShardId,
    /// The rows from the shard. The merge sorts them.
    pub rows: Vec<QueryRow>,
    /// The observed latency in abstract ticks.
    pub latency_ticks: u64,
}

impl ShardStream {
    /// Creates one shard stream.
    #[must_use]
    pub const fn new(shard_id: ShardId, rows: Vec<QueryRow>) -> Self {
        Self {
            shard_id,
            rows,
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

/// The limits of one merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MergeOptions {
    /// The merge order.
    pub order: MergeOrder,
    /// The maximum row count.
    pub limit: usize,
    /// The maximum shard count.
    pub max_fanout: usize,
}

impl MergeOptions {
    /// Creates one merge option set.
    #[must_use]
    pub const fn new(order: MergeOrder, limit: usize, max_fanout: usize) -> Self {
        Self {
            order,
            limit,
            max_fanout,
        }
    }
}

/// The result of one merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutput {
    /// The merged rows in final order.
    pub rows: Vec<QueryRow>,
    /// The examined key count per shard.
    pub frontiers: BTreeMap<ShardId, CostFrontier>,
}

/// Compares two rows under one query order.
#[must_use]
pub fn compare_rows(order: MergeOrder, left: &QueryRow, right: &QueryRow) -> Ordering {
    match order {
        MergeOrder::Search => right
            .score
            .cmp(&left.score)
            .then_with(|| left.canonical_key.cmp(&right.canonical_key)),
        MergeOrder::General => left
            .sort_key
            .cmp(&right.sort_key)
            .then_with(|| left.canonical_key.cmp(&right.canonical_key)),
    }
}

/// Merges ordered shard streams under one total order.
///
/// The method reads one head row per shard. It returns at most `limit` rows.
/// A duplicate canonical key must have identical content. The method keeps one
/// row for the key and advances every duplicate stream.
///
/// # Errors
///
/// Returns [`CostError::FanoutExceeded`] when the stream count is above the
/// fan-out limit. Returns [`CostError::DuplicateKey`] when one canonical key
/// has different content.
pub fn merge_streams(
    streams: &[ShardStream],
    options: MergeOptions,
) -> Result<MergeOutput, CostError> {
    if streams.len() > options.max_fanout {
        return Err(CostError::FanoutExceeded {
            requested: len_to_u32(streams.len()),
            limit: len_to_u32(options.max_fanout),
        });
    }
    let sorted: Vec<Vec<QueryRow>> = streams
        .iter()
        .map(|stream| {
            let mut rows = stream.rows.clone();
            rows.sort_by(|left, right| compare_rows(options.order, left, right));
            rows
        })
        .collect();
    let mut position = vec![0_usize; sorted.len()];
    let mut consumed = vec![0_u64; sorted.len()];
    let mut seen: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut rows = Vec::new();
    while rows.len() < options.limit {
        let mut best: Option<usize> = None;
        for index in 0..sorted.len() {
            let Some(row) = sorted[index].get(position[index]) else {
                continue;
            };
            let replace = best.is_none_or(|best_index| {
                compare_rows(
                    options.order,
                    row,
                    &sorted[best_index][position[best_index]],
                ) == Ordering::Less
            });
            if replace {
                best = Some(index);
            }
        }
        let Some(best_index) = best else {
            break;
        };
        let best_row = sorted[best_index][position[best_index]].clone();
        let mut candidates: Vec<usize> = Vec::new();
        for index in 0..sorted.len() {
            let Some(row) = sorted[index].get(position[index]) else {
                continue;
            };
            if compare_rows(options.order, row, &best_row) == Ordering::Equal {
                candidates.push(index);
            }
        }
        if candidates.is_empty() {
            candidates.push(best_index);
        }
        let first_index = candidates[0];
        let first_row = sorted[first_index][position[first_index]].clone();
        for index in candidates.iter().copied().skip(1) {
            let row = &sorted[index][position[index]];
            if !same_content(row, &first_row) {
                return Err(CostError::DuplicateKey {
                    key: first_row.canonical_key,
                });
            }
        }
        if !seen.insert(first_row.canonical_key.clone()) {
            return Err(CostError::DuplicateKey {
                key: first_row.canonical_key,
            });
        }
        rows.push(first_row);
        for index in candidates {
            position[index] = position[index].saturating_add(1);
            consumed[index] = consumed[index].saturating_add(1);
        }
    }
    let frontiers: BTreeMap<ShardId, CostFrontier> = streams
        .iter()
        .enumerate()
        .map(|(index, stream)| (stream.shard_id.clone(), CostFrontier::new(consumed[index])))
        .collect();
    Ok(MergeOutput { rows, frontiers })
}

fn same_content(left: &QueryRow, right: &QueryRow) -> bool {
    left.canonical_key == right.canonical_key
        && left.score == right.score
        && left.sort_key == right.sort_key
        && left.payload == right.payload
}

/// Returns the examined key count of one shard stream. The merge uses this
/// count for the pagination frontier.
#[must_use]
pub fn stream_key_count(stream: &ShardStream) -> u64 {
    len_to_u64(stream.rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxkv_contracts::SortValue;

    fn row(shard: u8, key: &[u8], score: i64, payload: &[u8]) -> QueryRow {
        QueryRow::new(
            ShardId::new(vec![shard]),
            key.to_vec(),
            score,
            SortTuple::default(),
            payload.to_vec(),
        )
    }

    fn stream(shard: u8, rows: Vec<QueryRow>, latency: u64) -> ShardStream {
        ShardStream::new(ShardId::new(vec![shard]), rows).with_latency_ticks(latency)
    }

    fn keys(output: &MergeOutput) -> Vec<Vec<u8>> {
        output
            .rows
            .iter()
            .map(|row| row.canonical_key.clone())
            .collect()
    }

    #[test]
    fn a_search_breaks_a_score_tie_by_canonical_key() {
        let streams = vec![
            stream(
                2,
                vec![row(2, b"c", 9, b"one"), row(2, b"a", 7, b"two")],
                50,
            ),
            stream(1, vec![row(1, b"b", 7, b"three")], 1),
        ];
        let output = merge_streams(&streams, MergeOptions::new(MergeOrder::Search, 10, 4))
            .expect("the streams merge");
        assert_eq!(
            keys(&output),
            vec![b"c".to_vec(), b"a".to_vec(), b"b".to_vec()]
        );
        assert_eq!(output.frontiers[&ShardId::new(vec![1])].keys_examined, 1);
        assert_eq!(output.frontiers[&ShardId::new(vec![2])].keys_examined, 2);
    }

    #[test]
    fn a_general_query_uses_the_sort_key_then_the_canonical_key() {
        let mut rows = Vec::new();
        for (key, value) in [
            (b"a".as_slice(), 2_i64),
            (b"b".as_slice(), 1_i64),
            (b"c".as_slice(), 1_i64),
        ] {
            rows.push(QueryRow::new(
                ShardId::new(vec![1]),
                key.to_vec(),
                0,
                SortTuple::new(vec![SortValue::Integer(value)]),
                Vec::new(),
            ));
        }
        let streams = vec![ShardStream::new(ShardId::new(vec![1]), rows)];
        let output = merge_streams(&streams, MergeOptions::new(MergeOrder::General, 10, 1))
            .expect("the stream merges");
        assert_eq!(
            output
                .rows
                .iter()
                .map(|row| row.canonical_key.clone())
                .collect::<Vec<_>>(),
            vec![b"b".to_vec(), b"c".to_vec(), b"a".to_vec()]
        );
    }

    #[test]
    fn a_shuffled_input_and_latency_do_not_change_the_order() {
        let first = vec![
            stream(3, vec![row(3, b"d", 5, b"x")], 90),
            stream(1, vec![row(1, b"a", 8, b"y")], 2),
            stream(2, vec![row(2, b"b", 8, b"z"), row(2, b"c", 4, b"w")], 40),
        ];
        let second = vec![
            stream(2, vec![row(2, b"c", 4, b"w"), row(2, b"b", 8, b"z")], 1),
            stream(3, vec![row(3, b"d", 5, b"x")], 500),
            stream(1, vec![row(1, b"a", 8, b"y")], 0),
        ];
        let left = merge_streams(&first, MergeOptions::new(MergeOrder::Search, 10, 4))
            .expect("the streams merge");
        let right = merge_streams(&second, MergeOptions::new(MergeOrder::Search, 10, 4))
            .expect("the streams merge");
        assert_eq!(keys(&left), keys(&right));
        assert_eq!(left.frontiers, right.frontiers);
    }

    #[test]
    fn a_fanout_over_the_limit_is_rejected() {
        let streams = vec![
            stream(1, vec![row(1, b"a", 1, b"")], 0),
            stream(2, vec![row(2, b"b", 1, b"")], 0),
        ];
        let error = merge_streams(&streams, MergeOptions::new(MergeOrder::Search, 10, 1))
            .expect_err("the fan-out is over the limit");
        assert_eq!(error.code().as_str(), "search.fanout_exceeded");
    }

    #[test]
    fn one_key_with_two_contents_is_rejected() {
        let streams = vec![
            stream(1, vec![row(1, b"a", 1, b"one")], 0),
            stream(2, vec![row(2, b"a", 1, b"two")], 0),
        ];
        let error = merge_streams(&streams, MergeOptions::new(MergeOrder::Search, 10, 2))
            .expect_err("the key has two contents");
        assert_eq!(error.code().as_str(), "internal.invariant_violation");
    }

    #[test]
    fn equal_rows_from_two_shards_collapse_to_one_row() {
        let streams = vec![
            stream(1, vec![row(1, b"a", 1, b"one")], 0),
            stream(2, vec![row(2, b"a", 1, b"one")], 0),
        ];
        let output = merge_streams(&streams, MergeOptions::new(MergeOrder::Search, 10, 2))
            .expect("the equal rows merge");
        assert_eq!(output.rows.len(), 1);
        assert_eq!(output.frontiers[&ShardId::new(vec![1])].keys_examined, 1);
        assert_eq!(output.frontiers[&ShardId::new(vec![2])].keys_examined, 1);
    }

    #[test]
    fn the_shard_identifier_is_not_a_comparator() {
        let left = row(9, b"a", 1, b"");
        let right = row(1, b"a", 1, b"");
        assert_eq!(
            compare_rows(MergeOrder::Search, &left, &right),
            Ordering::Equal
        );
        let left = row(9, b"b", 1, b"");
        let right = row(1, b"a", 1, b"");
        assert_eq!(
            compare_rows(MergeOrder::Search, &left, &right),
            Ordering::Greater
        );
    }

    #[test]
    fn a_stream_key_count_is_available_for_a_frontier() {
        let stream = stream(1, vec![row(1, b"a", 1, b"")], 0);
        assert_eq!(stream_key_count(&stream), 1);
    }
}
