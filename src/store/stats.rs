//! Client-side operation counters for stores.
//!
//! [`StatsStore`] wraps any store. It counts API calls, returned bytes, scan
//! rows, transaction completions, and errors. The counters use atomics. A
//! clone of the decorator shares the same counters.
//!
//! These counters are client-side. They count calls that reach the wrapper.
//! They do not count S3 requests, WAL objects, or compaction backlog. The LSM
//! cache statistics remain the source for cache hit and miss data. Use
//! [`Cache::stats`](super::Cache::stats) or
//! [`OxKvStore::sst_cache_stats`](super::OxKvStore::sst_cache_stats) for those
//! numbers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use super::{Cas, CasResult, Direction, GetSet, KeyValue, Result, Store, Transaction};

/// A snapshot of the counters held by a [`StatsStore`].
///
/// The snapshot is not transactional. Concurrent calls can change the
/// underlying counters between field reads. The snapshot is still consistent
/// enough for monitoring and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreStats {
    /// `get_bytes` calls.
    pub gets: u64,
    /// `has` calls.
    pub has: u64,
    /// `set_bytes` calls.
    pub sets: u64,
    /// `put_bytes` calls.
    pub puts: u64,
    /// `delete` calls.
    pub deletes: u64,
    /// `gets_bytes` and `gets_keys` calls.
    pub scans: u64,
    /// Rows returned by `gets_bytes` plus keys returned by `gets_keys`.
    pub scan_items: u64,
    /// Value bytes returned by `get_bytes` and `gets_bytes`.
    pub bytes_read: u64,
    /// Value bytes accepted by `set_bytes` and `put_bytes`.
    ///
    /// The count includes writes staged in a transaction.
    pub bytes_written: u64,
    /// `commit` calls on transaction handles.
    pub tx_commits: u64,
    /// `rollback` calls on transaction handles.
    pub tx_rollbacks: u64,
    /// Calls that returned a [`StoreError`](super::StoreError).
    pub errors: u64,
}

/// Shared atomic counters. Every clone of a [`StatsStore`] points to one
/// `Counters` value.
#[derive(Debug, Default)]
struct Counters {
    gets: AtomicU64,
    has: AtomicU64,
    sets: AtomicU64,
    puts: AtomicU64,
    deletes: AtomicU64,
    scans: AtomicU64,
    scan_items: AtomicU64,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    tx_commits: AtomicU64,
    tx_rollbacks: AtomicU64,
    errors: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> StoreStats {
        StoreStats {
            gets: self.gets.load(Ordering::Relaxed),
            has: self.has.load(Ordering::Relaxed),
            sets: self.sets.load(Ordering::Relaxed),
            puts: self.puts.load(Ordering::Relaxed),
            deletes: self.deletes.load(Ordering::Relaxed),
            scans: self.scans.load(Ordering::Relaxed),
            scan_items: self.scan_items.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            tx_commits: self.tx_commits.load(Ordering::Relaxed),
            tx_rollbacks: self.tx_rollbacks.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }

    fn reset(&self) {
        for counter in [
            &self.gets,
            &self.has,
            &self.sets,
            &self.puts,
            &self.deletes,
            &self.scans,
            &self.scan_items,
            &self.bytes_read,
            &self.bytes_written,
            &self.tx_commits,
            &self.tx_rollbacks,
            &self.errors,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }

    fn add(counter: &AtomicU64, value: usize) {
        let value = u64::try_from(value).unwrap_or(u64::MAX);
        counter.fetch_add(value, Ordering::Relaxed);
    }

    fn add_bytes_read(&self, value: usize) {
        Self::add(&self.bytes_read, value);
    }

    fn add_bytes_written(&self, value: usize) {
        Self::add(&self.bytes_written, value);
    }

    fn add_scan_items(&self, value: usize) {
        Self::add(&self.scan_items, value);
    }

    fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// A store decorator that counts client-side operations.
///
/// `StatsStore` implements [`GetSet`], [`Store`], and [`Cas`] when the wrapped
/// store implements them. It returns the wrapped store from
/// [`StatsStore::into_inner`].
///
/// # Counter contract
///
/// - A counter for an API method counts calls to that method. A failed call
///   still increments its method counter and increments `errors`.
/// - `scans` counts `gets_bytes` and `gets_keys` calls. `scan_items` counts the
///   rows or keys that those calls return.
/// - `bytes_read` counts value bytes returned by reads. `bytes_written` counts
///   value bytes accepted by `set_bytes` and `put_bytes`.
/// - `Cas` calls do not change `gets`, `sets`, or `deletes`. They only
///   increment `errors` when they fail.
/// - The counters are client-side. They do not count S3 requests, WAL objects,
///   or compaction backlog. Use the LSM cache statistics for cache hit and miss
///   data.
///
/// # Example
///
/// ```rust
/// # #[cfg(feature = "btree")]
/// # mod btree_example {
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// use oxkv::{BTreeStore, GetSet, StatsStore};
///
/// let store = StatsStore::new(BTreeStore::default());
/// store.set_bytes("k", b"value").await.unwrap();
/// let _ = store.get_bytes("k").await.unwrap();
///
/// let stats = store.stats();
/// assert_eq!(stats.sets, 1);
/// assert_eq!(stats.gets, 1);
/// assert_eq!(stats.bytes_read, 5);
/// # }
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct StatsStore<S> {
    inner: S,
    counters: Arc<Counters>,
}

impl<S> StatsStore<S> {
    /// Wraps `inner` with operation counters.
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            counters: Arc::new(Counters::default()),
        }
    }

    /// Returns a snapshot of the counters.
    ///
    /// Concurrent calls can change the counters between field reads. The
    /// snapshot is not transactional.
    #[must_use]
    pub fn stats(&self) -> StoreStats {
        self.counters.snapshot()
    }

    /// Sets every counter to zero.
    pub fn reset(&self) {
        self.counters.reset();
    }

    /// Returns the wrapped store and discards the counters.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }
}

#[async_trait]
impl<S: GetSet + Send + Sync> GetSet for StatsStore<S> {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.counters.gets.fetch_add(1, Ordering::Relaxed);
        match self.inner.get_bytes(key).await {
            Ok(value) => {
                if let Some(value) = &value {
                    self.counters.add_bytes_read(value.len());
                }
                Ok(value)
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn has(&self, key: &str) -> Result<bool> {
        self.counters.has.fetch_add(1, Ordering::Relaxed);
        match self.inner.has(key).await {
            Ok(value) => Ok(value),
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        self.counters.deletes.fetch_add(1, Ordering::Relaxed);
        match self.inner.delete(key).await {
            Ok(value) => Ok(value),
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        self.counters.sets.fetch_add(1, Ordering::Relaxed);
        match self.inner.set_bytes(key, value).await {
            Ok(previous) => {
                self.counters.add_bytes_written(value.len());
                Ok(previous)
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        self.counters.puts.fetch_add(1, Ordering::Relaxed);
        match self.inner.put_bytes(key, value).await {
            Ok(()) => {
                self.counters.add_bytes_written(value.len());
                Ok(())
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn gets_bytes(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.counters.scans.fetch_add(1, Ordering::Relaxed);
        match self.inner.gets_bytes(limit, direction, cursor).await {
            Ok(rows) => {
                self.counters.add_scan_items(rows.len());
                let bytes = rows.iter().map(|row| row.value.len()).sum();
                self.counters.add_bytes_read(bytes);
                Ok(rows)
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn gets_keys(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<String>> {
        self.counters.scans.fetch_add(1, Ordering::Relaxed);
        match self.inner.gets_keys(limit, direction, cursor).await {
            Ok(keys) => {
                self.counters.add_scan_items(keys.len());
                Ok(keys)
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }
}

#[async_trait]
impl<S> Store for StatsStore<S>
where
    S: Store + Send + Sync,
    S::Transaction: Send + Sync,
{
    type Transaction = StatsTx<S::Transaction>;

    fn begin_tx(&self) -> Result<Self::Transaction> {
        match self.inner.begin_tx() {
            Ok(inner) => Ok(StatsTx {
                inner,
                counters: Arc::clone(&self.counters),
            }),
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }
}

#[async_trait]
impl<S: Cas> Cas for StatsStore<S> {
    async fn set_if_absent(&self, key: &str, value: &[u8]) -> Result<bool> {
        self.inner
            .set_if_absent(key, value)
            .await
            .inspect_err(|_error| {
                self.counters.record_error();
            })
    }

    async fn compare_and_set(
        &self,
        key: &str,
        expected: Option<&[u8]>,
        new: &[u8],
    ) -> Result<CasResult> {
        self.inner
            .compare_and_set(key, expected, new)
            .await
            .inspect_err(|_error| {
                self.counters.record_error();
            })
    }

    async fn delete_if(&self, key: &str, expected: &[u8]) -> Result<bool> {
        self.inner
            .delete_if(key, expected)
            .await
            .inspect_err(|_error| {
                self.counters.record_error();
            })
    }
}

/// The transaction type produced by [`StatsStore::begin_tx`].
///
/// It forwards every operation to the wrapped transaction and shares the
/// counters of its parent store. A commit or rollback increments the matching
/// terminal counter. An error increments `errors`.
#[derive(Debug, Clone)]
pub struct StatsTx<T> {
    inner: T,
    counters: Arc<Counters>,
}

#[async_trait]
impl<T: GetSet + Send + Sync> GetSet for StatsTx<T> {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.counters.gets.fetch_add(1, Ordering::Relaxed);
        match self.inner.get_bytes(key).await {
            Ok(value) => {
                if let Some(value) = &value {
                    self.counters.add_bytes_read(value.len());
                }
                Ok(value)
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn has(&self, key: &str) -> Result<bool> {
        self.counters.has.fetch_add(1, Ordering::Relaxed);
        match self.inner.has(key).await {
            Ok(value) => Ok(value),
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        self.counters.deletes.fetch_add(1, Ordering::Relaxed);
        match self.inner.delete(key).await {
            Ok(value) => Ok(value),
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        self.counters.sets.fetch_add(1, Ordering::Relaxed);
        match self.inner.set_bytes(key, value).await {
            Ok(previous) => {
                self.counters.add_bytes_written(value.len());
                Ok(previous)
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        self.counters.puts.fetch_add(1, Ordering::Relaxed);
        match self.inner.put_bytes(key, value).await {
            Ok(()) => {
                self.counters.add_bytes_written(value.len());
                Ok(())
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn gets_bytes(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.counters.scans.fetch_add(1, Ordering::Relaxed);
        match self.inner.gets_bytes(limit, direction, cursor).await {
            Ok(rows) => {
                self.counters.add_scan_items(rows.len());
                let bytes = rows.iter().map(|row| row.value.len()).sum();
                self.counters.add_bytes_read(bytes);
                Ok(rows)
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }

    async fn gets_keys(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<String>> {
        self.counters.scans.fetch_add(1, Ordering::Relaxed);
        match self.inner.gets_keys(limit, direction, cursor).await {
            Ok(keys) => {
                self.counters.add_scan_items(keys.len());
                Ok(keys)
            }
            Err(error) => {
                self.counters.record_error();
                Err(error)
            }
        }
    }
}

#[async_trait]
impl<T: Transaction + Send + Sync> Transaction for StatsTx<T> {
    async fn commit(self) -> Result<()> {
        self.counters.tx_commits.fetch_add(1, Ordering::Relaxed);
        self.inner.commit().await.inspect_err(|_error| {
            self.counters.record_error();
        })
    }

    async fn rollback(self) -> Result<()> {
        self.counters.tx_rollbacks.fetch_add(1, Ordering::Relaxed);
        self.inner.rollback().await.inspect_err(|_error| {
            self.counters.record_error();
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{StatsStore, StoreStats};
    use crate::store::{
        GetSet, Hooks, KeyPrefix, Result, ScanExt, Scope, Store, Transaction, Validator,
    };

    struct RejectPrefix;

    #[async_trait::async_trait]
    impl Validator for RejectPrefix {
        fn scope(&self) -> Scope {
            Scope::Prefix("reject/".to_string())
        }

        async fn validate(
            &self,
            _ctx: &dyn crate::store::StoreView,
            key: &str,
            _value: &[u8],
        ) -> Result<()> {
            Err(crate::store::StoreError::Rejected {
                key: key.to_string(),
                reason: "rejected by the stats test".to_string(),
            })
        }
    }

    async fn assert_counts<S>(inner: S)
    where
        S: Store + Hooks + Send + Sync,
        S::Transaction: Send + Sync,
    {
        inner.add_validator(RejectPrefix);
        let store = StatsStore::new(inner);
        assert_eq!(store.stats(), StoreStats::default());

        store.set_bytes("a", b"12").await.unwrap();
        store.put_bytes("b", b"345").await.unwrap();
        assert_eq!(store.get_bytes("a").await.unwrap(), Some(b"12".to_vec()));
        assert!(store.has("a").await.unwrap());
        let page = store.scan_keys(KeyPrefix::new(""), None, 10).await.unwrap();
        assert_eq!(page.items, ["a", "b"]);
        assert!(store.delete("a").await.unwrap());

        let tx = store.begin_tx().unwrap();
        tx.set_bytes("c", b"x").await.unwrap();
        tx.commit().await.unwrap();

        let tx = store.begin_tx().unwrap();
        tx.rollback().await.unwrap();

        assert!(store.set_bytes("reject/x", b"bad").await.is_err());

        let stats = store.stats();
        assert_eq!(stats.gets, 1, "get_bytes");
        assert_eq!(stats.has, 1, "has");
        assert_eq!(stats.sets, 3, "set_bytes, transaction set, rejected set");
        assert_eq!(stats.puts, 1, "put_bytes");
        assert_eq!(stats.deletes, 1, "delete");
        assert_eq!(stats.scans, 1, "scan_keys");
        assert_eq!(stats.scan_items, 2, "scan rows");
        assert_eq!(stats.bytes_read, 2, "get value");
        assert_eq!(
            stats.bytes_written, 6,
            "two writes plus one transaction write"
        );
        assert_eq!(stats.tx_commits, 1, "commit");
        assert_eq!(stats.tx_rollbacks, 1, "rollback");
        assert_eq!(stats.errors, 1, "rejected write");

        store.reset();
        assert_eq!(store.stats(), StoreStats::default());
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_counts_operations_bytes_transactions_and_errors() {
        assert_counts(crate::store::BTreeStore::default()).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_into_inner_returns_the_wrapped_store() {
        let inner = crate::store::BTreeStore::default();
        inner.set_bytes("kept", b"value").await.unwrap();
        let store = StatsStore::new(inner);
        let inner = store.into_inner();
        assert_eq!(
            inner.get_bytes("kept").await.unwrap(),
            Some(b"value".to_vec())
        );
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
    async fn oxkv_counts_operations_bytes_transactions_and_errors() {
        assert_counts(in_memory_oxkv().await).await;
    }
}
