//! Prefix-bounded scans that can return keys without values.
//!
//! [`KeyPrefix`] turns a string prefix into an exclusive upper bound. The
//! bound handles UTF-8 scalars and an empty prefix. [`ScanExt`] reads one
//! bounded page at a time. The page carries a cursor for the next call.
//!
//! The crate root already uses `Prefix` for the query-clause enum. This scan
//! prefix type is `KeyPrefix`.

use async_trait::async_trait;

use super::{Direction, GetSet, KeyValue, Result};

/// A prefix for a range scan. The type borrows the prefix text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPrefix<'a>(&'a str);

impl<'a> KeyPrefix<'a> {
    /// Creates a prefix from `prefix`.
    #[must_use]
    pub const fn new(prefix: &'a str) -> Self {
        Self(prefix)
    }

    /// Returns the prefix text.
    #[must_use]
    pub const fn as_str(&self) -> &'a str {
        self.0
    }

    /// Returns the exclusive upper bound of the prefix range.
    ///
    /// The range from the prefix to the bound holds exactly the keys that
    /// start with the prefix.
    ///
    /// The method increments the last scalar value. It skips the UTF-16
    /// surrogate range, because a surrogate is not a Unicode scalar value. If
    /// the last scalar value is `char::MAX`, the method removes it and
    /// increments the scalar value before it.
    ///
    /// The result is `None` in two cases:
    ///
    /// - The prefix is empty.
    /// - The prefix contains only `char::MAX` scalars.
    ///
    /// In both cases, an unbounded end gives the correct range.
    #[must_use]
    pub fn upper_bound(&self) -> Option<String> {
        let mut end = self.0.to_string();
        while let Some(last) = end.pop() {
            let mut next = u32::from(last) + 1;
            if (0xD800..=0xDFFF).contains(&next) {
                next = 0xE000;
            }
            if let Some(next) = char::from_u32(next) {
                end.push(next);
                return Some(end);
            }
        }
        None
    }
}

/// One page of a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The rows in this page. The rows are in ascending key order.
    pub items: Vec<T>,
    /// The cursor for the next page.
    ///
    /// The value is the last returned key. The value is `None` when no more
    /// rows may exist. Pass the value as the `after` cursor of the next call.
    pub next: Option<String>,
}

/// Returns the fetch limit for a caller page size.
///
/// The function adds one row. That extra row tells the caller whether more
/// rows exist after the page.
fn fetch_limit(limit: usize) -> usize {
    limit.saturating_add(1)
}

/// Builds the inclusive start and exclusive end of one page range.
///
/// The result is `None` when the `after` cursor is at or above the upper
/// bound. No key in the prefix can follow that cursor.
fn page_range(prefix: KeyPrefix<'_>, after: Option<&str>) -> Option<(String, Option<String>)> {
    let end = prefix.upper_bound();
    let start = match after {
        Some(cursor) if cursor > prefix.as_str() => cursor,
        _ => prefix.as_str(),
    };
    if let Some(last) = &end
        && start >= last.as_str()
    {
        return None;
    }
    Some((start.to_string(), end))
}

/// Prefix scans for every [`GetSet`] backend.
///
/// Every method reads in ascending key order. Every method passes a bounded
/// limit to the backend. The methods never pass `limit = None`.
#[async_trait]
pub trait ScanExt: GetSet {
    /// Returns one page of keys that start with `prefix`.
    ///
    /// The scan starts after the `after` cursor. The scan returns at most
    /// `limit` keys. A `limit` of 0 returns an empty page with no cursor.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the read fails.
    async fn scan_keys(
        &self,
        prefix: KeyPrefix<'_>,
        after: Option<String>,
        limit: usize,
    ) -> Result<Page<String>> {
        if limit == 0 {
            return Ok(Page {
                items: Vec::new(),
                next: None,
            });
        }
        let Some((start, end)) = page_range(prefix, after.as_deref()) else {
            return Ok(Page {
                items: Vec::new(),
                next: None,
            });
        };
        let fetch = fetch_limit(limit);
        let rows = self
            .gets_keys(Some(fetch), Direction::Next, (Some(start), end.clone()))
            .await?;
        let full = rows.len() == fetch;
        let mut items: Vec<String> = rows
            .into_iter()
            .filter(|key| {
                after.as_deref().is_none_or(|cursor| key.as_str() > cursor)
                    && end.as_deref().is_none_or(|bound| key.as_str() < bound)
            })
            .collect();
        if items.len() > limit {
            items.truncate(limit);
        }
        let next = if full { items.last().cloned() } else { None };
        Ok(Page { items, next })
    }

    /// Returns one page of rows that start with `prefix`.
    ///
    /// The parameters have the same meaning as [`ScanExt::scan_keys`]. When
    /// `values` is `false`, every row has an empty `value` field. When
    /// `values` is `true`, the method reads the values.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the read fails.
    async fn scan(
        &self,
        prefix: KeyPrefix<'_>,
        after: Option<String>,
        limit: usize,
        values: bool,
    ) -> Result<Page<KeyValue>> {
        if limit == 0 {
            return Ok(Page {
                items: Vec::new(),
                next: None,
            });
        }
        let Some((start, end)) = page_range(prefix, after.as_deref()) else {
            return Ok(Page {
                items: Vec::new(),
                next: None,
            });
        };
        let fetch = fetch_limit(limit);
        let mut items = if values {
            self.gets_bytes(Some(fetch), Direction::Next, (Some(start), end.clone()))
                .await?
        } else {
            self.gets_keys(Some(fetch), Direction::Next, (Some(start), end.clone()))
                .await?
                .into_iter()
                .map(|key| KeyValue {
                    key,
                    value: Vec::new(),
                })
                .collect()
        };
        let full = items.len() == fetch;
        items.retain(|row| {
            after
                .as_deref()
                .is_none_or(|cursor| row.key.as_str() > cursor)
                && end.as_deref().is_none_or(|bound| row.key.as_str() < bound)
        });
        if items.len() > limit {
            items.truncate(limit);
        }
        let next = if full {
            items.last().map(|row| row.key.clone())
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// Returns one page of keys that start with `prefix` and are less than
    /// `cutoff`.
    ///
    /// `cutoff` is an exclusive lexicographic bound. A key that is equal to
    /// `cutoff` or above it is not returned. The caller encodes time in the
    /// key, for example `timer/000001700000000000/`.
    ///
    /// The scan starts after the `after` cursor. The scan returns at most
    /// `limit` keys. A `limit` of 0 returns an empty page with no cursor.
    /// Pass [`Page::next`] as the `after` cursor of the next call to read the
    /// next page.
    ///
    /// This method is a cleanup helper. It is not a TTL. The store has no
    /// expiry metadata and no background expiry job.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the read fails.
    async fn scan_older_than(
        &self,
        prefix: KeyPrefix<'_>,
        cutoff: &str,
        after: Option<String>,
        limit: usize,
    ) -> Result<Page<String>> {
        let page = self.scan_keys(prefix, after, limit).await?;
        let read = page.items.len();
        let mut items = page.items;
        items.retain(|key| key.as_str() < cutoff);
        // `scan_keys` returns keys in ascending order. If the cutoff removed
        // any key, no later key can be below the cutoff.
        let next = if items.len() == read { page.next } else { None };
        Ok(Page { items, next })
    }

    /// Counts the keys that start with `prefix`, up to `max`.
    ///
    /// The result is at most `max`. A `max` of 0 returns 0. The method reads
    /// one bounded page at a time. It stops when it reaches `max`.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if a read fails.
    async fn count_prefix(&self, prefix: KeyPrefix<'_>, max: usize) -> Result<usize> {
        /// The number of keys in one internal page.
        const BATCH: usize = 256;

        if max == 0 {
            return Ok(0);
        }
        let mut count = 0usize;
        let mut after: Option<String> = None;
        while count < max {
            let want = (max - count).min(BATCH);
            let page = self.scan_keys(prefix, after, want).await?;
            count += page.items.len();
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        Ok(count.min(max))
    }
}

#[async_trait]
impl<T: GetSet + ?Sized> ScanExt for T {}

#[cfg(test)]
mod tests {
    use super::{KeyPrefix, ScanExt};
    use crate::store::{GetSet, KeyValue};

    /// The keys that the overlap test uses.
    const BASE_KEYS: [&str; 5] = ["a", "a:", "a:b", "ab", "b"];

    fn key_list(items: &[String]) -> Vec<&str> {
        items.iter().map(String::as_str).collect()
    }

    fn row_keys(items: &[KeyValue]) -> Vec<&str> {
        items.iter().map(|row| row.key.as_str()).collect()
    }

    async fn seed<S: GetSet>(store: &S, keys: &[&str]) {
        for key in keys {
            store.set_bytes(key, key.as_bytes()).await.unwrap();
        }
    }

    async fn assert_overlapping_and_paging<S: GetSet + Sync>(store: &S) {
        seed(store, &BASE_KEYS).await;

        let page = store
            .scan_keys(KeyPrefix::new("a"), None, 10)
            .await
            .unwrap();
        assert_eq!(key_list(&page.items), ["a", "a:", "a:b", "ab"]);
        assert_eq!(page.next, None);

        let page = store
            .scan_keys(KeyPrefix::new("a"), Some("b".to_string()), 10)
            .await
            .unwrap();
        assert!(page.items.is_empty(), "a cursor above the range must stop");
        assert_eq!(page.next, None, "a cursor above the range has no next");

        let page = store
            .scan_keys(KeyPrefix::new("a:"), None, 10)
            .await
            .unwrap();
        assert_eq!(key_list(&page.items), ["a:", "a:b"]);

        let page = store
            .scan_keys(KeyPrefix::new("b"), None, 10)
            .await
            .unwrap();
        assert_eq!(key_list(&page.items), ["b"]);

        let page = store.scan_keys(KeyPrefix::new(""), None, 10).await.unwrap();
        assert_eq!(key_list(&page.items), BASE_KEYS);
        assert_eq!(page.next, None);

        let page = store.scan_keys(KeyPrefix::new(""), None, 0).await.unwrap();
        assert!(page.items.is_empty(), "a limit of 0 must return no keys");
        assert_eq!(page.next, None, "a limit of 0 must return no cursor");

        let mut walked: Vec<String> = Vec::new();
        let mut after = None;
        let mut pages = 0;
        loop {
            let page = store.scan_keys(KeyPrefix::new(""), after, 2).await.unwrap();
            pages += 1;
            assert!(pages <= 8, "pagination did not stop");
            walked.extend(page.items);
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        assert_eq!(
            key_list(&walked),
            BASE_KEYS,
            "each key must appear exactly once"
        );

        assert_eq!(store.count_prefix(KeyPrefix::new("a"), 0).await.unwrap(), 0);
        assert_eq!(store.count_prefix(KeyPrefix::new("a"), 2).await.unwrap(), 2);
        assert_eq!(
            store.count_prefix(KeyPrefix::new("a"), 99).await.unwrap(),
            4
        );
        assert_eq!(
            store.count_prefix(KeyPrefix::new("z"), 99).await.unwrap(),
            0
        );

        let page = store
            .scan(KeyPrefix::new("a:"), None, 10, true)
            .await
            .unwrap();
        assert_eq!(row_keys(&page.items), ["a:", "a:b"]);
        assert_eq!(page.items[0].value, b"a:".to_vec());
        assert_eq!(page.next, None);

        let page = store
            .scan(KeyPrefix::new("a:"), None, 10, false)
            .await
            .unwrap();
        assert_eq!(row_keys(&page.items), ["a:", "a:b"]);
        assert!(page.items.iter().all(|row| row.value.is_empty()));
    }

    async fn assert_utf8_prefix<S: GetSet + Sync>(store: &S) {
        seed(store, &["aé", "aéx", "aê", "af"]).await;

        let page = store
            .scan_keys(KeyPrefix::new("aé"), None, 10)
            .await
            .unwrap();
        assert_eq!(key_list(&page.items), ["aé", "aéx"]);
        assert_eq!(page.next, None);
        assert_eq!(KeyPrefix::new("aé").upper_bound(), Some("aê".to_string()));
    }

    async fn assert_max_scalar_prefix<S: GetSet + Sync>(store: &S) {
        let max = "\u{10FFFF}";
        let repeated = "\u{10FFFF}\u{10FFFF}";
        seed(store, &[max, repeated, "z"]).await;

        let page = store
            .scan_keys(KeyPrefix::new(max), None, 10)
            .await
            .unwrap();
        assert_eq!(key_list(&page.items), [max, repeated]);
        assert_eq!(page.next, None);
        assert_eq!(store.count_prefix(KeyPrefix::new(max), 9).await.unwrap(), 2);
        assert_eq!(KeyPrefix::new(max).upper_bound(), None);
    }

    #[test]
    fn prefix_upper_bound_handles_ascii_utf8_and_empty_text() {
        assert_eq!(KeyPrefix::new("").upper_bound(), None);
        assert_eq!(KeyPrefix::new("a").upper_bound(), Some("b".to_string()));
        assert_eq!(KeyPrefix::new("a:").upper_bound(), Some("a;".to_string()));
        assert_eq!(KeyPrefix::new("aé").upper_bound(), Some("aê".to_string()));
        assert_eq!(
            KeyPrefix::new("\u{D7FF}").upper_bound(),
            Some("\u{E000}".to_string()),
            "the bound must skip the surrogate range"
        );
        assert_eq!(
            KeyPrefix::new("a\u{10FFFF}").upper_bound(),
            Some("b".to_string())
        );
        assert_eq!(KeyPrefix::new("\u{10FFFF}").upper_bound(), None);
        assert_eq!(KeyPrefix::new("\u{10FFFF}\u{10FFFF}").upper_bound(), None);
    }

    #[test]
    fn prefix_reports_the_borrowed_text() {
        let text = String::from("abc");
        let prefix = KeyPrefix::new(&text);
        assert_eq!(prefix.as_str(), "abc");
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_prefix_scans_cover_overlap_paging_and_counts() {
        let store = crate::store::BTreeStore::default();
        assert_overlapping_and_paging(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_prefix_scans_handle_utf8_and_max_scalars() {
        let store = crate::store::BTreeStore::default();
        assert_utf8_prefix(&store).await;
        let store = crate::store::BTreeStore::default();
        assert_max_scalar_prefix(&store).await;
    }

    async fn assert_scan_older_than<S: GetSet + Sync>(store: &S) {
        for key in [
            "timer/0001",
            "timer/0002",
            "timer/0003",
            "timer/0004",
            "timer/0005",
            "timerx/0001",
        ] {
            store.set_bytes(key, b"v").await.unwrap();
        }

        // The cutoff is exclusive.
        let page = store
            .scan_older_than(KeyPrefix::new("timer/"), "timer/0003", None, 10)
            .await
            .unwrap();
        assert_eq!(key_list(&page.items), ["timer/0001", "timer/0002"]);
        assert_eq!(page.next, None);

        // A cutoff above every key returns every key in the prefix.
        let page = store
            .scan_older_than(KeyPrefix::new("timer/"), "timer/9", None, 10)
            .await
            .unwrap();
        assert_eq!(
            key_list(&page.items),
            [
                "timer/0001",
                "timer/0002",
                "timer/0003",
                "timer/0004",
                "timer/0005"
            ]
        );

        // A cutoff at or below the prefix returns no key.
        for cutoff in ["timer/", "timer/0000", "a"] {
            let page = store
                .scan_older_than(KeyPrefix::new("timer/"), cutoff, None, 10)
                .await
                .unwrap();
            assert!(page.items.is_empty(), "cutoff {cutoff:?}");
            assert_eq!(page.next, None, "cutoff {cutoff:?}");
        }

        // Paging stops at the cutoff.
        let mut walked = Vec::new();
        let mut after = None;
        let mut pages = 0;
        loop {
            let page = store
                .scan_older_than(KeyPrefix::new("timer/"), "timer/0005", after, 2)
                .await
                .unwrap();
            pages += 1;
            assert!(pages <= 8, "paging did not stop");
            assert!(
                page.items.iter().all(|key| key.as_str() < "timer/0005"),
                "a page must not pass the cutoff"
            );
            walked.extend(page.items);
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        assert_eq!(
            key_list(&walked),
            ["timer/0001", "timer/0002", "timer/0003", "timer/0004"],
            "each key below the cutoff must appear exactly once"
        );

        // A cursor at or above the cutoff returns no key.
        for after in ["timer/0005".to_string(), "timer/9".to_string()] {
            let page = store
                .scan_older_than(KeyPrefix::new("timer/"), "timer/0005", Some(after), 10)
                .await
                .unwrap();
            assert!(
                page.items.is_empty(),
                "a cursor at or above the cutoff must return no key"
            );
            assert_eq!(page.next, None);
        }

        // A limit of 0 returns an empty page.
        let page = store
            .scan_older_than(KeyPrefix::new("timer/"), "timer/0005", None, 0)
            .await
            .unwrap();
        assert!(page.items.is_empty(), "limit 0");
        assert_eq!(page.next, None);
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_scan_older_than_stops_at_the_cutoff() {
        let store = crate::store::BTreeStore::default();
        assert_scan_older_than(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_tx_prefix_scans_see_staged_writes() {
        use crate::store::Store;

        let store = crate::store::BTreeStore::default();
        store.set_bytes("a:1", b"1").await.unwrap();
        let tx = store.begin_tx().unwrap();
        tx.set_bytes("a:2", b"2").await.unwrap();

        let page = tx.scan_keys(KeyPrefix::new("a:"), None, 10).await.unwrap();
        assert_eq!(key_list(&page.items), ["a:1", "a:2"]);
        let page = tx
            .scan_keys(KeyPrefix::new("a:"), Some("a:1".to_string()), 10)
            .await
            .unwrap();
        assert_eq!(key_list(&page.items), ["a:2"]);
    }

    #[cfg(feature = "oxkv")]
    async fn in_memory_oxkv() -> crate::store::OxKvStore {
        crate::store::OxKvStore::builder()
            .with_store(crate::store::lsm::new_in_memory())
            .skip_probe(true)
            .build()
            .await
            .unwrap()
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_prefix_scans_cover_overlap_paging_and_counts() {
        let store = in_memory_oxkv().await;
        assert_overlapping_and_paging(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_prefix_scans_handle_utf8_and_max_scalars() {
        let store = in_memory_oxkv().await;
        assert_utf8_prefix(&store).await;
        let store = in_memory_oxkv().await;
        assert_max_scalar_prefix(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_scan_older_than_stops_at_the_cutoff() {
        let store = in_memory_oxkv().await;
        assert_scan_older_than(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_tx_prefix_scans_see_staged_writes() {
        use crate::store::Store;

        let store = in_memory_oxkv().await;
        store.set_bytes("a:1", b"1").await.unwrap();
        let tx = store.begin_tx().unwrap();
        tx.set_bytes("a:2", b"2").await.unwrap();

        let page = tx.scan_keys(KeyPrefix::new("a:"), None, 10).await.unwrap();
        assert_eq!(key_list(&page.items), ["a:1", "a:2"]);
        let page = tx
            .scan_keys(KeyPrefix::new("a:"), Some("a:1".to_string()), 10)
            .await
            .unwrap();
        assert_eq!(key_list(&page.items), ["a:2"]);
    }
}
