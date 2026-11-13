//! Prefix-scoped handles for stores.
//!
//! [`ScopedStore`] wraps a clone of a store and maps every key to
//! `{prefix}/{key}`. The handle returns unscoped keys to the caller. Cursors map
//! in both directions. A scan stays inside the scope even when a cursor is
//! unbounded.
//!
//! An empty prefix leaves every key unchanged. The wrapper owns a clone of the
//! inner store. For stores whose clone is cheap, such as [`OxKvStore`], the
//! clone shares the cache, client, and session.
//!
//! [`OxKvStore`]: super::OxKvStore

use async_trait::async_trait;

use super::{Cas, CasResult, Direction, GetSet, KeyPrefix, KeyValue, Result, Store, Transaction};

/// Returns the storage key for `key` in `prefix`.
fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}/{key}")
    }
}

/// Removes `prefix` from a storage key. The function returns `None` when the
/// key is outside the scope.
fn unmap(prefix: &str, key: String) -> Option<String> {
    if prefix.is_empty() {
        return Some(key);
    }
    let rest = key.strip_prefix(prefix)?.strip_prefix('/')?;
    Some(rest.to_string())
}

/// Returns the minimum storage key in the scope.
fn scope_start(prefix: &str) -> String {
    format!("{prefix}/")
}

/// Returns the exclusive upper bound of the scope, when one exists.
fn scope_end(prefix: &str) -> Option<String> {
    KeyPrefix::new(&scope_start(prefix)).upper_bound()
}

/// Adds one to a fetch limit when the caller asked for every row. The extra row
/// replaces a possible out-of-scope sentinel row at the upper bound.
fn widen(limit: Option<usize>) -> Option<usize> {
    limit.map(|value| value.saturating_add(1))
}

fn unmap_row(prefix: &str, mut row: KeyValue) -> Option<KeyValue> {
    row.key = unmap(prefix, row.key)?;
    Some(row)
}

/// Reads one scoped page of rows.
async fn scoped_gets_bytes<T: GetSet + Sync>(
    inner: &T,
    prefix: &str,
    limit: Option<usize>,
    direction: Direction,
    cursor: (Option<String>, Option<String>),
) -> Result<Vec<KeyValue>> {
    if prefix.is_empty() {
        return inner.gets_bytes(limit, direction, cursor).await;
    }
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let start_bound = scope_start(prefix);
    match direction {
        Direction::Next => {
            let start = cursor.0.map(|key| join(prefix, &key)).or(Some(start_bound));
            let unbounded_end = cursor.1.is_none();
            let end = cursor
                .1
                .map(|key| join(prefix, &key))
                .or_else(|| scope_end(prefix));
            let fetch = if unbounded_end { widen(limit) } else { limit };
            let rows = inner
                .gets_bytes(fetch, Direction::Next, (start, end))
                .await?;
            Ok(rows
                .into_iter()
                .filter_map(|row| unmap_row(prefix, row))
                .collect())
        }
        Direction::Prev => {
            if let Some(cursor_start) = cursor.0 {
                let start = Some(join(prefix, &cursor_start));
                let end = cursor.1.map(|key| join(prefix, &key)).or(Some(start_bound));
                let rows = inner
                    .gets_bytes(limit, Direction::Prev, (start, end))
                    .await?;
                return Ok(rows
                    .into_iter()
                    .filter_map(|row| unmap_row(prefix, row))
                    .collect());
            }
            // A descending scan needs a starting point. Read the scope in
            // ascending order, keep the last page, then reverse it.
            let end = cursor
                .1
                .map(|key| join(prefix, &key))
                .or_else(|| scope_end(prefix));
            let mut rows: Vec<KeyValue> = inner
                .gets_bytes(None, Direction::Next, (Some(start_bound), end))
                .await?
                .into_iter()
                .filter_map(|row| unmap_row(prefix, row))
                .collect();
            if let Some(limit) = limit {
                let excess = rows.len().saturating_sub(limit);
                if excess > 0 {
                    rows.drain(..excess);
                }
            }
            rows.reverse();
            Ok(rows)
        }
    }
}

/// Reads one scoped page of keys.
async fn scoped_gets_keys<T: GetSet + Sync>(
    inner: &T,
    prefix: &str,
    limit: Option<usize>,
    direction: Direction,
    cursor: (Option<String>, Option<String>),
) -> Result<Vec<String>> {
    if prefix.is_empty() {
        return inner.gets_keys(limit, direction, cursor).await;
    }
    if limit == Some(0) {
        return Ok(Vec::new());
    }
    let start_bound = scope_start(prefix);
    match direction {
        Direction::Next => {
            let start = cursor.0.map(|key| join(prefix, &key)).or(Some(start_bound));
            let unbounded_end = cursor.1.is_none();
            let end = cursor
                .1
                .map(|key| join(prefix, &key))
                .or_else(|| scope_end(prefix));
            let fetch = if unbounded_end { widen(limit) } else { limit };
            let keys = inner
                .gets_keys(fetch, Direction::Next, (start, end))
                .await?;
            Ok(keys
                .into_iter()
                .filter_map(|key| unmap(prefix, key))
                .collect())
        }
        Direction::Prev => {
            if let Some(cursor_start) = cursor.0 {
                let start = Some(join(prefix, &cursor_start));
                let end = cursor.1.map(|key| join(prefix, &key)).or(Some(start_bound));
                let keys = inner
                    .gets_keys(limit, Direction::Prev, (start, end))
                    .await?;
                return Ok(keys
                    .into_iter()
                    .filter_map(|key| unmap(prefix, key))
                    .collect());
            }
            let end = cursor
                .1
                .map(|key| join(prefix, &key))
                .or_else(|| scope_end(prefix));
            let mut keys: Vec<String> = inner
                .gets_keys(None, Direction::Next, (Some(start_bound), end))
                .await?
                .into_iter()
                .filter_map(|key| unmap(prefix, key))
                .collect();
            if let Some(limit) = limit {
                let excess = keys.len().saturating_sub(limit);
                if excess > 0 {
                    keys.drain(..excess);
                }
            }
            keys.reverse();
            Ok(keys)
        }
    }
}

/// A store handle restricted to one key prefix.
///
/// Every key is mapped to `{prefix}/{key}`. An empty prefix leaves keys
/// unchanged. Reads return keys without the prefix. Cursors map in both
/// directions. A scan stays inside the scope even when its cursor is
/// unbounded.
///
/// The wrapper owns a clone of the inner store. For stores whose clone is
/// cheap, the clone shares the cache, client, and session.
///
/// # Example
///
/// ```rust
/// # #[cfg(feature = "btree")]
/// # mod btree_example {
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// use oxkv::{BTreeStore, GetSet, StoreExt};
///
/// let store = BTreeStore::default();
/// let tenant = store.scoped("tenant-a");
/// tenant.set_bytes("key", b"value").await.unwrap();
///
/// assert_eq!(
///     tenant.get_bytes("key").await.unwrap(),
///     Some(b"value".to_vec())
/// );
/// assert_eq!(
///     store.get_bytes("tenant-a/key").await.unwrap(),
///     Some(b"value".to_vec())
/// );
/// # }
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ScopedStore<S> {
    inner: S,
    prefix: String,
}

impl<S> ScopedStore<S> {
    /// Creates a handle for `prefix` over `inner`.
    #[must_use]
    pub fn new(inner: S, prefix: impl Into<String>) -> Self {
        Self {
            inner,
            prefix: prefix.into(),
        }
    }

    /// Returns the scope prefix.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Returns the wrapped store and discards the scope.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }
}

#[async_trait]
impl<S: GetSet + Send + Sync> GetSet for ScopedStore<S> {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let key = join(&self.prefix, key);
        self.inner.get_bytes(&key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        let key = join(&self.prefix, key);
        self.inner.has(&key).await
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        let key = join(&self.prefix, key);
        self.inner.delete(&key).await
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let key = join(&self.prefix, key);
        self.inner.set_bytes(&key, value).await
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        let key = join(&self.prefix, key);
        self.inner.put_bytes(&key, value).await
    }

    async fn gets_bytes(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        scoped_gets_bytes(&self.inner, &self.prefix, limit, direction, cursor).await
    }

    async fn gets_keys(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<String>> {
        scoped_gets_keys(&self.inner, &self.prefix, limit, direction, cursor).await
    }
}

#[async_trait]
impl<S> Store for ScopedStore<S>
where
    S: Store + Send + Sync,
    S::Transaction: Send + Sync,
{
    type Transaction = ScopedTx<S::Transaction>;

    fn begin_tx(&self) -> Result<Self::Transaction> {
        Ok(ScopedTx {
            inner: self.inner.begin_tx()?,
            prefix: self.prefix.clone(),
        })
    }
}

#[async_trait]
impl<S: Cas> Cas for ScopedStore<S> {
    async fn set_if_absent(&self, key: &str, value: &[u8]) -> Result<bool> {
        let key = join(&self.prefix, key);
        self.inner.set_if_absent(&key, value).await
    }

    async fn compare_and_set(
        &self,
        key: &str,
        expected: Option<&[u8]>,
        new: &[u8],
    ) -> Result<CasResult> {
        let key = join(&self.prefix, key);
        self.inner.compare_and_set(&key, expected, new).await
    }

    async fn delete_if(&self, key: &str, expected: &[u8]) -> Result<bool> {
        let key = join(&self.prefix, key);
        self.inner.delete_if(&key, expected).await
    }
}

/// The transaction type produced by [`ScopedStore::begin_tx`].
///
/// It maps each key into the scope of its parent store. It delegates commit and
/// rollback to the wrapped transaction.
#[derive(Debug, Clone)]
pub struct ScopedTx<T> {
    inner: T,
    prefix: String,
}

#[async_trait]
impl<T: GetSet + Send + Sync> GetSet for ScopedTx<T> {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let key = join(&self.prefix, key);
        self.inner.get_bytes(&key).await
    }

    async fn has(&self, key: &str) -> Result<bool> {
        let key = join(&self.prefix, key);
        self.inner.has(&key).await
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        let key = join(&self.prefix, key);
        self.inner.delete(&key).await
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let key = join(&self.prefix, key);
        self.inner.set_bytes(&key, value).await
    }

    async fn put_bytes(&self, key: &str, value: &[u8]) -> Result<()> {
        let key = join(&self.prefix, key);
        self.inner.put_bytes(&key, value).await
    }

    async fn gets_bytes(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        scoped_gets_bytes(&self.inner, &self.prefix, limit, direction, cursor).await
    }

    async fn gets_keys(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<String>> {
        scoped_gets_keys(&self.inner, &self.prefix, limit, direction, cursor).await
    }
}

#[async_trait]
impl<T: Transaction + Send + Sync> Transaction for ScopedTx<T> {
    async fn commit(self) -> Result<()> {
        self.inner.commit().await
    }

    async fn rollback(self) -> Result<()> {
        self.inner.rollback().await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::ScopedStore;
    use crate::store::{
        Cas, CasResult, Direction, GetSet, KeyPrefix, ScanExt, Store, StoreExt, Transaction,
    };

    async fn assert_two_scopes<S>(store: &S)
    where
        S: Store + Cas + Clone + Send + Sync,
        S::Transaction: Send + Sync,
    {
        let first = store.scoped("first");
        let second = store.scoped("second");

        first.set_bytes("key", b"one").await.unwrap();
        second.set_bytes("key", b"two").await.unwrap();

        assert_eq!(first.get_bytes("key").await.unwrap(), Some(b"one".to_vec()));
        assert_eq!(
            second.get_bytes("key").await.unwrap(),
            Some(b"two".to_vec())
        );
        assert_eq!(
            store.get_bytes("first/key").await.unwrap(),
            Some(b"one".to_vec())
        );
        assert_eq!(
            store.get_bytes("second/key").await.unwrap(),
            Some(b"two".to_vec())
        );
        assert!(!first.has("absent").await.unwrap());
        assert!(!second.has("absent").await.unwrap());

        assert!(first.set_if_absent("claim", b"a").await.unwrap());
        assert!(!first.set_if_absent("claim", b"b").await.unwrap());
        assert!(second.set_if_absent("claim", b"c").await.unwrap());
        assert_eq!(
            first
                .compare_and_set("claim", Some(b"a"), b"changed")
                .await
                .unwrap(),
            CasResult::Applied
        );
        assert_eq!(
            second
                .compare_and_set("claim", Some(b"a"), b"changed")
                .await
                .unwrap(),
            CasResult::Conflict {
                current: Some(b"c".to_vec())
            }
        );
    }

    async fn assert_scans_stay_scoped<S>(store: &S)
    where
        S: Store + Clone + Send + Sync,
        S::Transaction: Send + Sync,
    {
        let first = store.scoped("first");
        let second = store.scoped("second");
        first.set_bytes("one", b"1").await.unwrap();
        first.set_bytes("two", b"2").await.unwrap();
        second.set_bytes("zero", b"0").await.unwrap();
        second.set_bytes("one", b"1").await.unwrap();

        let page = first.scan_keys(KeyPrefix::new(""), None, 10).await.unwrap();
        assert_eq!(page.items, ["one", "two"]);
        assert_eq!(page.next, None);

        let rows = first
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .unwrap();
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["one", "two"]);

        let page = first.scan_keys(KeyPrefix::new(""), None, 1).await.unwrap();
        assert_eq!(page.items, ["one"]);
        assert_eq!(page.next, Some("one".to_string()));

        let page = first
            .scan_keys(KeyPrefix::new(""), page.next, 10)
            .await
            .unwrap();
        assert_eq!(page.items, ["two"]);
        assert_eq!(page.next, None);

        let rows = first
            .gets_bytes(None, Direction::Prev, (None, None))
            .await
            .unwrap();
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["two", "one"]);

        let keys = first
            .gets_keys(None, Direction::Prev, (None, None))
            .await
            .unwrap();
        assert_eq!(keys, ["two", "one"]);

        let keys = first
            .gets_keys(Some(1), Direction::Prev, (None, None))
            .await
            .unwrap();
        assert_eq!(keys, ["two"]);

        let page = first
            .scan(KeyPrefix::new(""), None, 10, true)
            .await
            .unwrap();
        let keys: Vec<&str> = page.items.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["one", "two"]);
        assert_eq!(page.items[0].value, b"1".to_vec());
    }

    async fn assert_transaction_stays_scoped<S>(store: &S)
    where
        S: Store + Clone + Send + Sync,
        S::Transaction: Send + Sync,
    {
        let first = store.scoped("first");
        let second = store.scoped("second");

        let tx = first.begin_tx().unwrap();
        tx.set_bytes("key", b"one").await.unwrap();
        assert_eq!(tx.get_bytes("key").await.unwrap(), Some(b"one".to_vec()));
        assert_eq!(first.get_bytes("key").await.unwrap(), None);
        tx.commit().await.unwrap();

        assert_eq!(first.get_bytes("key").await.unwrap(), Some(b"one".to_vec()));
        assert_eq!(second.get_bytes("key").await.unwrap(), None);
        assert_eq!(
            store.get_bytes("first/key").await.unwrap(),
            Some(b"one".to_vec())
        );
        assert_eq!(store.get_bytes("second/key").await.unwrap(), None);

        let tx = second.begin_tx().unwrap();
        tx.set_bytes("key", b"two").await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(first.get_bytes("key").await.unwrap(), Some(b"one".to_vec()));
        assert_eq!(
            second.get_bytes("key").await.unwrap(),
            Some(b"two".to_vec())
        );
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_scopes_are_isolated() {
        assert_two_scopes(&crate::store::BTreeStore::default()).await;
        assert_scans_stay_scoped(&crate::store::BTreeStore::default()).await;
        assert_transaction_stays_scoped(&crate::store::BTreeStore::default()).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_an_empty_prefix_leaves_keys_unchanged() {
        let store = crate::store::BTreeStore::default();
        let plain = store.scoped("");
        plain.set_bytes("key", b"value").await.unwrap();
        assert_eq!(plain.prefix(), "");
        assert_eq!(
            store.get_bytes("key").await.unwrap(),
            Some(b"value".to_vec())
        );
        let keys = plain
            .gets_keys(None, Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(keys, ["key"]);
        let inner = plain.into_inner();
        assert_eq!(
            inner.get_bytes("key").await.unwrap(),
            Some(b"value".to_vec())
        );
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_scoped_store_can_be_cloned() {
        let store = crate::store::BTreeStore::default();
        let first = store.scoped("first");
        let clone = ScopedStore::new(first.into_inner(), "first");
        clone.set_bytes("key", b"one").await.unwrap();
        assert_eq!(
            store.get_bytes("first/key").await.unwrap(),
            Some(b"one".to_vec())
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
    async fn oxkv_scopes_are_isolated() {
        assert_two_scopes(&in_memory_oxkv().await).await;
        assert_scans_stay_scoped(&in_memory_oxkv().await).await;
        assert_transaction_stays_scoped(&in_memory_oxkv().await).await;
    }
}
