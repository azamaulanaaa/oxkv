//! Deterministic k-way merge over ordered shard streams.
//!
//! The merge reads only enough hits for the requested window. It stops after
//! `limit + 1` hits, so the coordinator knows whether another page exists
//! without buffering every shard result.

use core::cmp::Ordering;

use oxkv_contracts::version::ShardId;

use crate::error::SearchError;
use crate::hit::{QueryOrder, SearchHit, SortPosition, compare_hits};

/// One merged page and the more-data flag.
pub(crate) struct MergedPage {
    pub(crate) hits: Vec<SearchHit>,
    pub(crate) has_more: bool,
}

/// Merges ordered shard streams.
///
/// The caller passes the streams in a deterministic order. The merge uses only
/// the query comparator. It does not use the shard identifier as a tie-break.
///
/// # Errors
///
/// Returns [`SearchError::UnorderedShard`] when one stream is not in comparator
/// order. Returns [`SearchError::DuplicateHit`] when two shards return the same
/// canonical key.
pub(crate) fn merge_streams(
    order: QueryOrder,
    streams: &[(ShardId, Vec<SearchHit>)],
    after: Option<&SortPosition>,
    limit: usize,
) -> Result<MergedPage, SearchError> {
    for (shard_id, hits) in streams {
        for pair in hits.windows(2) {
            if compare_hits(order, &pair[0], &pair[1]) == Ordering::Greater {
                return Err(SearchError::UnorderedShard {
                    shard_id: shard_id.clone(),
                });
            }
        }
    }

    let mut positions: Vec<usize> = streams
        .iter()
        .map(|(_, hits)| {
            after.map_or(0, |position| {
                hits.iter()
                    .position(|hit| position.precedes(order, hit))
                    .unwrap_or(hits.len())
            })
        })
        .collect();

    let take = limit.saturating_add(1);
    let mut merged = Vec::new();
    let mut last_key: Option<Vec<u8>> = None;

    while merged.len() < take {
        let best = next_stream(order, streams, &positions);
        let Some((index, position)) = best else {
            break;
        };
        let hit = &streams[index].1[position];
        if last_key.as_deref() == Some(hit.canonical_key()) {
            return Err(SearchError::DuplicateHit {
                key: hit.canonical_key().to_vec(),
            });
        }
        last_key = Some(hit.canonical_key().to_vec());
        merged.push(hit.clone());
        positions[index] = position.saturating_add(1);
    }

    let has_more = merged.len() > limit;
    merged.truncate(limit);
    Ok(MergedPage {
        hits: merged,
        has_more,
    })
}

fn next_stream(
    order: QueryOrder,
    streams: &[(ShardId, Vec<SearchHit>)],
    positions: &[usize],
) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (index, position) in positions.iter().enumerate() {
        let Some(hit) = streams[index].1.get(*position) else {
            continue;
        };
        match best {
            None => best = Some((index, *position)),
            Some((best_index, best_position)) => {
                let candidate = &streams[best_index].1[best_position];
                if compare_hits(order, hit, candidate) == Ordering::Less {
                    best = Some((index, *position));
                }
            }
        }
    }
    best
}
