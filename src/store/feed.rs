//! A durable append-only change feed on top of a [`Store`] backend.
//!
//! [`ChangeFeed`] stores events under one key prefix. Each event has a
//! monotonically increasing sequence number. A consumer reads the events after
//! its cursor with [`ChangeFeed::since`].
//!
//! # Keys
//!
//! The feed uses two key shapes:
//!
//! - `{prefix}/{seq:020}` for an event. The sequence is a zero-padded `u64`.
//!   The fixed width makes lexicographic order equal numeric order.
//! - `{prefix}/next` for the next-sequence counter.
//!
//! The first sequence number is 1.
//!
//! # Delivery contract
//!
//! - The feed gives at-least-once delivery. A consumer must be idempotent. Use
//!   the sequence number as the idempotency key.
//! - A gap in the sequence numbers is possible. [`ChangeFeed::append`] updates
//!   the counter first and writes the event second. A crash between the two
//!   steps leaves a gap. A consumer must tolerate a gap and continue.
//! - This type is a durable event-log helper. It is not automatic change-data
//!   capture. The store does not write an event for every write. The caller
//!   writes each event with [`ChangeFeed::append`] or
//!   [`ChangeFeed::append_in_tx`].
//! - [`ChangeFeed::prune`] deletes events in chunks. It is not atomic across
//!   chunks.
//!
//! # Concurrency
//!
//! [`ChangeFeed::next_seq`] uses [`Cas::compare_and_set`] on the counter key.
//! Concurrent callers cannot get the same sequence number. The method retries
//! when the counter changes before the write.

use super::{Cas, CasResult, Direction, GetSet, Result, Store, StoreError, Transaction};

/// One event in a [`ChangeFeed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEntry {
    /// The sequence number of the event. The first sequence is 1.
    pub seq: u64,
    /// The event payload.
    pub value: Vec<u8>,
}

/// A durable append-only event log with a cursor and chunked pruning.
///
/// The feed keeps its events under one prefix. It uses the key
/// `{prefix}/{seq:020}` for an event and `{prefix}/next` for the counter. See
/// the module documentation for the delivery contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChangeFeed {
    prefix: String,
}

impl ChangeFeed {
    /// Creates a feed under `prefix`.
    #[must_use]
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    /// Returns the event prefix.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Returns the event key for `seq`.
    fn event_key(&self, seq: u64) -> String {
        format!("{}/{:020}", self.prefix, seq)
    }

    /// Returns the counter key.
    fn counter_key(&self) -> String {
        format!("{}/next", self.prefix)
    }

    /// Returns the sequence number encoded in `key`.
    fn event_seq(&self, key: &str) -> Option<u64> {
        let prefix = format!("{}/", self.prefix);
        key.strip_prefix(&prefix)?.parse().ok()
    }

    /// Allocates the next sequence number and returns it.
    ///
    /// The method reads the counter, adds one, and writes the counter with
    /// [`Cas::compare_and_set`]. It retries when another caller changes the
    /// counter first. The first call on an empty feed returns 1.
    ///
    /// A successful call reserves the sequence. A later failure can leave a
    /// gap when the caller does not write the event. A consumer must tolerate
    /// a gap.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if a read or a write fails, if the counter
    /// holds a value that is not a `u64`, or if the sequence overflows.
    pub async fn next_seq<S: Cas>(&self, store: &S) -> Result<u64> {
        let counter = self.counter_key();
        let first = format!("{:020}", 1).into_bytes();
        loop {
            // A create-if-absent probe gives the first sequence and reads the
            // counter. `Cas` has no separate read method.
            match store.compare_and_set(&counter, None, &first).await? {
                CasResult::Applied => return Ok(1),
                CasResult::Conflict { current } => {
                    let current = current.ok_or_else(|| {
                        StoreError::Other(
                            "change feed counter conflict without a value".to_string(),
                        )
                    })?;
                    let next = decode_counter(&current)?.checked_add(1).ok_or_else(|| {
                        StoreError::Other("change feed sequence overflow".to_string())
                    })?;
                    let encoded = format!("{next:020}").into_bytes();
                    match store
                        .compare_and_set(&counter, Some(&current), &encoded)
                        .await?
                    {
                        CasResult::Applied => return Ok(next),
                        CasResult::Conflict { .. } => {}
                    }
                }
            }
        }
    }

    /// Allocates a sequence number and writes `event` under it.
    ///
    /// The method calls [`ChangeFeed::next_seq`] and then writes the event. A
    /// crash between the two steps leaves a gap. A consumer must tolerate the
    /// gap.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the sequence allocation or the write fails.
    pub async fn append<S: Store + Cas>(&self, store: &S, event: &[u8]) -> Result<u64> {
        let seq = self.next_seq(store).await?;
        store.put_bytes(&self.event_key(seq), event).await?;
        Ok(seq)
    }

    /// Writes `event` under the caller-supplied `seq` in `tx`.
    ///
    /// Use this method to commit a business row and the event in one
    /// transaction. The caller must supply a sequence that is unique for this
    /// feed. A sequence from [`ChangeFeed::next_seq`] is unique. The counter
    /// update is not part of `tx`, so a rollback after `next_seq` leaves a
    /// gap.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the staged write fails.
    pub async fn append_in_tx<T: Transaction + Sync>(
        &self,
        tx: &T,
        seq: u64,
        event: &[u8],
    ) -> Result<()> {
        tx.put_bytes(&self.event_key(seq), event).await?;
        Ok(())
    }

    /// Returns the events with `seq > cursor`.
    ///
    /// The result is in ascending sequence order and holds at most `limit`
    /// events. A `limit` of 0 returns an empty vector. A cursor at
    /// `u64::MAX` returns an empty vector.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if a read fails or if a returned key is not an
    /// event key.
    pub async fn since<S: GetSet>(
        &self,
        store: &S,
        cursor: u64,
        limit: usize,
    ) -> Result<Vec<FeedEntry>> {
        if limit == 0 || cursor == u64::MAX {
            return Ok(Vec::new());
        }
        let start = self.event_key(cursor + 1);
        let end = self.event_key(u64::MAX);
        let rows = store
            .gets_bytes(Some(limit), Direction::Next, (Some(start), Some(end)))
            .await?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let seq = self.event_seq(&row.key).ok_or_else(|| {
                StoreError::Other(format!("unexpected change feed key `{}`", row.key))
            })?;
            entries.push(FeedEntry {
                seq,
                value: row.value,
            });
        }
        Ok(entries)
    }

    /// Deletes the events with `seq < before` in chunks.
    ///
    /// Each chunk runs in one transaction with at most `batch` deletes. The
    /// whole range is not one atomic unit. A failure can leave earlier chunks
    /// deleted.
    ///
    /// Returns the number of deleted events. A `before` value of 0 or 1
    /// deletes nothing because the first sequence is 1.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Other`] when `batch` is 0. Returns a
    /// [`StoreError`] if a read, a staged delete, or a commit fails.
    pub async fn prune<S: Store>(&self, store: &S, before: u64, batch: usize) -> Result<usize> {
        if batch == 0 {
            return Err(StoreError::Other(
                "prune batch size must be greater than 0".to_string(),
            ));
        }
        if before <= 1 {
            return Ok(0);
        }

        let start = self.event_key(1);
        let end = self.event_key(before - 1);
        let mut deleted = 0usize;

        loop {
            let rows = store
                .gets_bytes(
                    Some(batch),
                    Direction::Next,
                    (Some(start.clone()), Some(end.clone())),
                )
                .await?;
            if rows.is_empty() {
                break;
            }

            let scanned = rows.len();
            let keys: Vec<String> = rows.into_iter().map(|row| row.key).collect();
            let tx = store.begin_tx()?;
            for key in &keys {
                tx.delete(key).await?;
            }
            tx.commit().await?;
            deleted += keys.len();

            if scanned < batch {
                break;
            }
        }

        Ok(deleted)
    }
}

/// Decodes the zero-padded counter value.
fn decode_counter(bytes: &[u8]) -> Result<u64> {
    let text = std::str::from_utf8(bytes)?;
    text.parse::<u64>().map_err(|error| {
        StoreError::Serialization(format!("invalid change feed sequence counter: {error}"))
    })
}

#[cfg(all(test, any(feature = "btree", feature = "oxkv")))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::{ChangeFeed, FeedEntry};
    use crate::store::{Cas, GetSet, Store, StoreError, Transaction};

    async fn assert_append_and_since<S: Store + Cas>(store: &S) {
        let feed = ChangeFeed::new("feed");
        assert_eq!(feed.prefix(), "feed");

        assert_eq!(feed.append(store, b"one").await.unwrap(), 1);
        assert_eq!(feed.append(store, b"two").await.unwrap(), 2);
        assert_eq!(feed.append(store, b"three").await.unwrap(), 3);

        let all = feed.since(store, 0, 10).await.unwrap();
        assert_eq!(
            all,
            vec![
                FeedEntry {
                    seq: 1,
                    value: b"one".to_vec(),
                },
                FeedEntry {
                    seq: 2,
                    value: b"two".to_vec(),
                },
                FeedEntry {
                    seq: 3,
                    value: b"three".to_vec(),
                },
            ]
        );

        let after_one = feed.since(store, 1, 10).await.unwrap();
        assert_eq!(
            after_one,
            vec![
                FeedEntry {
                    seq: 2,
                    value: b"two".to_vec(),
                },
                FeedEntry {
                    seq: 3,
                    value: b"three".to_vec(),
                },
            ]
        );

        assert_eq!(feed.since(store, 0, 2).await.unwrap().len(), 2);
        assert_eq!(feed.since(store, 0, 0).await.unwrap(), Vec::new());
        assert_eq!(feed.since(store, 3, 10).await.unwrap(), Vec::new());
        assert_eq!(feed.since(store, u64::MAX, 10).await.unwrap(), Vec::new());
    }

    async fn assert_prune<S: Store + Cas>(store: &S) {
        let feed = ChangeFeed::new("feed");
        for index in 1..=5 {
            assert_eq!(
                feed.append(store, format!("event-{index}").as_bytes())
                    .await
                    .unwrap(),
                index
            );
        }

        assert_eq!(feed.prune(store, 4, 2).await.unwrap(), 3);
        let remaining: Vec<u64> = feed
            .since(store, 0, 10)
            .await
            .unwrap()
            .iter()
            .map(|entry| entry.seq)
            .collect();
        assert_eq!(remaining, vec![4, 5]);

        assert_eq!(feed.prune(store, 100, 2).await.unwrap(), 2);
        assert_eq!(feed.since(store, 0, 10).await.unwrap(), Vec::new());
        assert_eq!(feed.prune(store, 100, 2).await.unwrap(), 0);
        assert_eq!(feed.prune(store, 0, 2).await.unwrap(), 0);

        let error = feed
            .prune(store, 100, 0)
            .await
            .expect_err("a batch size of 0 must fail");
        assert!(matches!(error, StoreError::Other(_)), "got {error:?}");
    }

    async fn assert_append_in_tx<S: Store + Cas>(store: &S) {
        let feed = ChangeFeed::new("feed");
        let seq = feed.next_seq(store).await.unwrap();

        let tx = store.begin_tx().unwrap();
        tx.set_bytes("entity/42", b"body").await.unwrap();
        feed.append_in_tx(&tx, seq, b"audited").await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(
            store.get_bytes("entity/42").await.unwrap(),
            Some(b"body".to_vec())
        );
        assert_eq!(
            feed.since(store, 0, 10).await.unwrap(),
            vec![FeedEntry {
                seq,
                value: b"audited".to_vec(),
            }]
        );

        let next = feed.next_seq(store).await.unwrap();
        let tx = store.begin_tx().unwrap();
        tx.set_bytes("entity/43", b"body").await.unwrap();
        feed.append_in_tx(&tx, next, b"rolled-back").await.unwrap();
        tx.rollback().await.unwrap();

        assert_eq!(store.get_bytes("entity/43").await.unwrap(), None);
        assert_eq!(feed.since(store, seq, 10).await.unwrap(), Vec::new());
    }

    async fn assert_concurrent_sequences<S>(store: Arc<S>)
    where
        S: Cas + Send + Sync + 'static,
    {
        let feed = Arc::new(ChangeFeed::new("feed"));
        let writers = (0..8).map(|_| {
            let store = Arc::clone(&store);
            let feed = Arc::clone(&feed);
            async move { feed.next_seq(store.as_ref()).await.unwrap() }
        });
        let mut sequences = futures::future::join_all(writers).await;
        sequences.sort_unstable();
        assert_eq!(sequences, (1..=8).collect::<Vec<u64>>());
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_change_feed_appends_and_reads_with_a_cursor() {
        let store = crate::store::BTreeStore::default();
        assert_append_and_since(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_change_feed_prunes_in_chunks() {
        let store = crate::store::BTreeStore::default();
        assert_prune(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_change_feed_append_in_tx_commits_with_the_business_row() {
        let store = crate::store::BTreeStore::default();
        assert_append_in_tx(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_change_feed_concurrent_sequences_are_unique() {
        let store = Arc::new(crate::store::BTreeStore::default());
        assert_concurrent_sequences(store).await;
    }

    #[cfg(feature = "oxkv")]
    async fn new_oxkv() -> crate::store::OxKvStore {
        crate::store::OxKvStore::builder()
            .with_store(crate::store::lsm::new_in_memory())
            .skip_probe(true)
            .build()
            .await
            .unwrap()
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_change_feed_appends_and_reads_with_a_cursor() {
        let store = new_oxkv().await;
        assert_append_and_since(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_change_feed_prunes_in_chunks() {
        let store = new_oxkv().await;
        assert_prune(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_change_feed_append_in_tx_commits_with_the_business_row() {
        let store = new_oxkv().await;
        assert_append_in_tx(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_change_feed_concurrent_sequences_are_unique() {
        let store = Arc::new(new_oxkv().await);
        assert_concurrent_sequences(store).await;
    }
}
