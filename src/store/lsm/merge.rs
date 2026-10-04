//! Newest-wins merge algebra over sorted read sources.
//!
//! Point and scan lookups resolve each key from layered sources. The code
//! orders the sources newest first (replay overlay, `MemTable`, SSTs). The
//! first entry for a key decides that key. Tombstones suppress older
//! duplicates. Forward scans pull lazily. Therefore a page never decodes more
//! than it yields.

use super::sst::SstScan;
use crate::store::{Direction, KeyValue, Result};

/// Merges sorted sources (newest first). The merge applies newest-wins
/// deduplication. The merge suppresses tombstones.
///
/// Each source is a `Vec<(key, Option<value>)>` in ascending order. `None` is
/// a tombstone. The function returns deduplicated `KeyValue` values in sorted
/// order. The function removes the tombstones.
#[must_use]
pub(crate) fn merge_sources(sources: Vec<Vec<(String, Option<Vec<u8>>)>>) -> Vec<KeyValue> {
    let mut map = std::collections::BTreeMap::new();
    for src in sources {
        for (key, value) in src {
            map.entry(key).or_insert(value);
        }
    }
    map.into_iter()
        .filter_map(|(key, value)| value.map(|v| KeyValue { key, value: v }))
        .collect()
}

/// One pull source for [`pull_merge_next`]. A source holds owned memory rows
/// or a borrowing file scan. File scans borrow their SST. Therefore sources
/// never outlive the handle vector that `gets_bytes` builds alongside them.
pub(crate) enum MergeSource<'a> {
    Mem(std::vec::IntoIter<(String, Option<Vec<u8>>)>),
    File(SstScan<'a>),
}

impl Iterator for MergeSource<'_> {
    type Item = Result<(String, Option<Vec<u8>>)>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Mem(it) => it.next().map(Ok),
            Self::File(it) => it.next(),
        }
    }
}

/// Heap entry for [`pull_merge_next`]. The heap orders entries by ascending
/// key. The source rank breaks ties. A smaller rank means a newer entry.
/// Therefore the first pop of a key is the newest entry for that key.
/// Therefore the first pop decides the key. `BinaryHeap` is a max-heap.
/// Therefore the code reverses the comparison.
struct MergeHeapEntry {
    key: String,
    rank: usize,
    value: Option<Vec<u8>>,
    source: usize,
}

impl PartialEq for MergeHeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.rank == other.rank
    }
}

impl Eq for MergeHeapEntry {}

impl PartialOrd for MergeHeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MergeHeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .key
            .cmp(&self.key)
            .then_with(|| other.rank.cmp(&self.rank))
    }
}

/// K-way newest-first merge over sorted per-source iterators. The merge
/// returns rows in ascending order.
///
/// The sources must yield range-filtered `(key, raw)` pairs. `None` marks a
/// tombstone. The rank is the source position. The code orders the pairs
/// newest first.
///
/// The function pulls the globally smallest undecided key. The newest entry
/// for that key decides the key. Tombstones suppress older duplicates without
/// yielding. The iteration stops after `limit` live keys. `None` drains all
/// keys.
///
/// The merge is exact. The entries pull in global order. Therefore an early
/// stop returns exactly what an uncapped merge and then truncate would
/// return. The function does not decode the unread tail.
pub(crate) fn pull_merge_next(
    sources: &mut [MergeSource<'_>],
    limit: Option<usize>,
) -> Result<Vec<(String, Vec<u8>)>> {
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let mut heap = std::collections::BinaryHeap::new();
    for (rank, source) in sources.iter_mut().enumerate() {
        if let Some(head) = source.next() {
            let (key, value) = head?;
            heap.push(MergeHeapEntry {
                key,
                rank,
                value,
                source: rank,
            });
        }
    }
    let mut out = Vec::new();
    while let Some(entry) = heap.pop() {
        while let Some(top) = heap.peek() {
            if top.key != entry.key {
                break;
            }
            if let Some(dup) = heap.pop()
                && let Some(next) = sources[dup.source].next()
            {
                let (key, value) = next?;
                heap.push(MergeHeapEntry {
                    key,
                    rank: dup.rank,
                    value,
                    source: dup.source,
                });
            }
        }
        if let Some(next) = sources[entry.source].next() {
            let (key, value) = next?;
            heap.push(MergeHeapEntry {
                key,
                rank: entry.rank,
                value,
                source: entry.source,
            });
        }
        if let Some(value) = entry.value {
            out.push((entry.key, value));
            if limit.is_some_and(|lim| out.len() >= lim) {
                break;
            }
        }
    }
    Ok(out)
}

/// Range-filtered, direction-aware scan over merged sources.
///
/// Mirrors `GetSet::gets_bytes` cursor semantics.
#[must_use]
pub(crate) fn merged_gets_bytes(
    sources: Vec<Vec<(String, Option<Vec<u8>>)>>,
    limit: Option<u32>,
    direction: Direction,
    cursor: (Option<String>, Option<String>),
) -> Vec<KeyValue> {
    let merged = merge_sources(sources);
    let (start, end) = cursor;
    let mut filtered: Vec<KeyValue> = match direction {
        Direction::Next => merged
            .into_iter()
            .filter(|kv| {
                if let Some(ref start_key) = start
                    && kv.key < *start_key
                {
                    return false;
                }
                if let Some(ref end_key) = end
                    && kv.key > *end_key
                {
                    return false;
                }
                true
            })
            .collect(),
        Direction::Prev => {
            if start.is_none() {
                return Vec::new();
            }
            let mut vec = merged
                .into_iter()
                .filter(|kv| {
                    if let Some(ref start_key) = start
                        && kv.key > *start_key
                    {
                        return false;
                    }
                    if let Some(ref end_key) = end
                        && kv.key < *end_key
                    {
                        return false;
                    }
                    true
                })
                .collect::<Vec<_>>();
            vec.reverse();
            vec
        }
    };
    if let Some(lim) = limit {
        let lim = lim as usize;
        if filtered.len() > lim {
            filtered.truncate(lim);
        }
    }
    filtered
}

#[cfg(test)]
mod tests {
    use super::super::sst;
    use super::*;
    use crate::store::SstFile;

    /// The test checks that `pull_merge_next` agrees with the materialized
    /// merge. The sources overlap. The sources contain tombstones. The test
    /// runs at every limit. The limits include the truncation edge and the
    /// full drain. The test guards the heap ordering. The test guards the
    /// newest-wins deduplication. The test guards the early-stop exactness of
    /// the lazy page scans.
    #[test]
    fn pull_merge_matches_materialized() {
        use std::collections::BTreeMap;
        let opt = |v: &str| Some(v.as_bytes().to_vec());
        let old: BTreeMap<String, Option<Vec<u8>>> = [
            ("a", opt("old-a")),
            ("b", opt("old-b")),
            ("c", opt("old-c")),
            ("d", opt("old-d")),
            ("e", opt("old-e")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let new: BTreeMap<String, Option<Vec<u8>>> =
            [("c", opt("new-c")), ("d", None), ("f", opt("new-f"))]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
        let mem: Vec<(String, Option<Vec<u8>>)> =
            vec![("a".to_string(), None), ("b".to_string(), opt("mem-b"))];
        let old_file =
            SstFile::parse(sst::build_sst(&old, 64).expect("old sst")).expect("parse old");
        let new_file =
            SstFile::parse(sst::build_sst(&new, 64).expect("new sst")).expect("parse new");
        let materialized = |limit: Option<usize>| {
            let merged = merge_sources(vec![
                mem.clone(),
                new_file
                    .scan_with_tombstones(None, None, None)
                    .expect("new scan"),
                old_file
                    .scan_with_tombstones(None, None, None)
                    .expect("old scan"),
            ]);
            let live: Vec<(String, Vec<u8>)> =
                merged.into_iter().map(|kv| (kv.key, kv.value)).collect();
            match limit {
                Some(lim) => live.into_iter().take(lim).collect(),
                None => live,
            }
        };
        for limit in [None, Some(0), Some(1), Some(2), Some(3), Some(10)] {
            let mut sources = vec![
                MergeSource::Mem(mem.clone().into_iter()),
                MergeSource::File(new_file.scan_iter(None, None)),
                MergeSource::File(old_file.scan_iter(None, None)),
            ];
            let pulled = pull_merge_next(&mut sources, limit).expect("pull");
            assert_eq!(pulled, materialized(limit), "limit {limit:?}");
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn merge_sources_newest_wins_and_tombstone_suppressed() {
        let sources = vec![
            vec![
                ("a".to_string(), Some(b"new-a".to_vec())),
                ("b".to_string(), None),
            ],
            vec![
                ("a".to_string(), Some(b"old-a".to_vec())),
                ("b".to_string(), Some(b"old-b".to_vec())),
                ("c".to_string(), Some(b"c".to_vec())),
            ],
        ];
        let merged = merge_sources(sources);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].key, "a");
        assert_eq!(merged[0].value, b"new-a");
        assert_eq!(merged[1].key, "c");
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn merged_gets_respects_direction_and_limit() {
        let sources = vec![vec![
            ("a".to_string(), Some(b"1".to_vec())),
            ("b".to_string(), Some(b"2".to_vec())),
            ("c".to_string(), Some(b"3".to_vec())),
        ]];
        let next = merged_gets_bytes(sources.clone(), Some(2), Direction::Next, (None, None));
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].key, "a");

        let prev = merged_gets_bytes(
            sources,
            None,
            Direction::Prev,
            (Some("c".to_string()), None),
        );
        assert_eq!(prev.len(), 3);
        assert_eq!(prev[0].key, "c");
        assert_eq!(prev[2].key, "a");
    }
}
