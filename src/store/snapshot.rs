//! A materialized, read-only snapshot of a store.
//!
//! [`Snapshot`] copies every key and value one time. A later write or delete
//! on the source store does not change the snapshot. Reads use only the
//! copied map.
//!
//! The snapshot is not free. It holds every key and every value in memory at
//! the same time. Its size is the sum of the sizes of all keys and values in
//! the source store.
//!
//! [`Snapshot`] implements [`GetSet`], [`Store`], and [`Transaction`]. Every
//! write returns [`StoreError::Other`] with the message `read-only snapshot:
//! writes are rejected`. A transaction commit on a snapshot is a no-op.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::{Direction, GetSet, KeyValue, Result, Store, StoreError, Transaction};

/// The message that every write to a [`Snapshot`] returns.
const READ_ONLY_MESSAGE: &str = "read-only snapshot: writes are rejected";

/// A fixed, read-only view of every key and value in a store.
///
/// Create a snapshot with [`StoreExt::snapshot`](super::StoreExt::snapshot).
/// The method reads every pair one time and copies it. A write or delete on
/// the source store after that call does not change the snapshot.
///
/// The snapshot buys the fixed view with memory. It holds every key and every
/// value at the same time. Do not create one for a store that does not fit in
/// memory.
///
/// A clone shares the copied map. It does not copy the data again.
#[derive(Clone)]
pub struct Snapshot {
    map: Arc<BTreeMap<String, Vec<u8>>>,
}

impl Snapshot {
    /// Builds a snapshot from one full read of a store.
    pub(crate) fn from_rows(rows: Vec<KeyValue>) -> Self {
        let map = rows
            .into_iter()
            .map(|row| (row.key, row.value))
            .collect::<BTreeMap<_, _>>();
        Self { map: Arc::new(map) }
    }

    /// Returns the number of keys in the snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns `true` when the snapshot holds no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("len", &self.map.len())
            .finish_non_exhaustive()
    }
}

/// The read-only transaction type of a [`Snapshot`].
///
/// A snapshot transaction reads the copied map. It cannot stage a write. A
/// commit or a rollback does nothing.
#[derive(Clone)]
pub struct SnapshotTx {
    map: Arc<BTreeMap<String, Vec<u8>>>,
}

impl std::fmt::Debug for SnapshotTx {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SnapshotTx")
            .field("len", &self.map.len())
            .finish_non_exhaustive()
    }
}

/// Returns the standard error for a write that targets a snapshot.
fn read_only_error() -> StoreError {
    StoreError::Other(READ_ONLY_MESSAGE.to_string())
}

/// Collects rows from an iterator, up to the optional limit.
fn collect_rows<'a, I>(iter: I, limit: Option<usize>) -> Vec<KeyValue>
where
    I: Iterator<Item = (&'a String, &'a Vec<u8>)>,
{
    let mut rows = Vec::new();
    for (key, value) in iter {
        if limit.is_some_and(|max| rows.len() >= max) {
            break;
        }
        rows.push(KeyValue {
            key: key.clone(),
            value: value.clone(),
        });
    }
    rows
}

/// Returns rows in ascending key order.
fn next_rows(
    map: &BTreeMap<String, Vec<u8>>,
    limit: Option<usize>,
    cursor: &(Option<String>, Option<String>),
) -> Vec<KeyValue> {
    match (&cursor.0, &cursor.1) {
        (Some(start), Some(end)) => {
            if start > end {
                return Vec::new();
            }
            collect_rows(map.range(start.clone()..=end.clone()), limit)
        }
        (Some(start), None) => collect_rows(map.range(start.clone()..), limit),
        (None, Some(end)) => collect_rows(map.range(..=end.clone()), limit),
        (None, None) => collect_rows(map.iter(), limit),
    }
}

/// Returns rows in descending key order.
fn prev_rows(
    map: &BTreeMap<String, Vec<u8>>,
    limit: Option<usize>,
    cursor: &(Option<String>, Option<String>),
) -> Vec<KeyValue> {
    let (Some(start), end) = (&cursor.0, &cursor.1) else {
        return Vec::new();
    };
    match end {
        Some(end) => {
            if start < end {
                return Vec::new();
            }
            collect_rows(map.range(end.clone()..=start.clone()).rev(), limit)
        }
        None => collect_rows(map.range(..=start.clone()).rev(), limit),
    }
}

/// Reads one page from the copied map.
fn read_page(
    map: &BTreeMap<String, Vec<u8>>,
    limit: Option<usize>,
    direction: Direction,
    cursor: &(Option<String>, Option<String>),
) -> Vec<KeyValue> {
    match direction {
        Direction::Next => next_rows(map, limit, cursor),
        Direction::Prev => prev_rows(map, limit, cursor),
    }
}

#[async_trait]
impl GetSet for Snapshot {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.map.get(key).cloned())
    }

    async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.map.contains_key(key))
    }

    async fn delete(&self, _key: &str) -> Result<bool> {
        Err(read_only_error())
    }

    async fn set_bytes(&self, _key: &str, _value: &[u8]) -> Result<Option<Vec<u8>>> {
        Err(read_only_error())
    }

    async fn gets_bytes(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        Ok(read_page(&self.map, limit, direction, &cursor))
    }
}

#[async_trait]
impl Store for Snapshot {
    type Transaction = SnapshotTx;

    fn begin_tx(&self) -> Result<Self::Transaction> {
        Ok(SnapshotTx {
            map: Arc::clone(&self.map),
        })
    }
}

#[async_trait]
impl GetSet for SnapshotTx {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.map.get(key).cloned())
    }

    async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.map.contains_key(key))
    }

    async fn delete(&self, _key: &str) -> Result<bool> {
        Err(read_only_error())
    }

    async fn set_bytes(&self, _key: &str, _value: &[u8]) -> Result<Option<Vec<u8>>> {
        Err(read_only_error())
    }

    async fn gets_bytes(
        &self,
        limit: Option<usize>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        Ok(read_page(&self.map, limit, direction, &cursor))
    }
}

#[async_trait]
impl Transaction for SnapshotTx {
    async fn commit(self) -> Result<()> {
        // Writes are rejected when the caller stages them, so a transaction
        // on a snapshot never holds a staged write. Commit is a no-op.
        Ok(())
    }

    async fn rollback(self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::Snapshot;
    use crate::store::{
        Direction, GetSet, GetSetExt, Page, ScanExt, Store, StoreError, StoreExt, Transaction,
    };

    async fn assert_snapshot_is_stable<S>(store: &S)
    where
        S: Store + Sync,
    {
        store.set_bytes("keep", b"old").await.unwrap();
        store.set_bytes("drop", b"old").await.unwrap();

        let snapshot = store.snapshot().await.unwrap();
        assert_eq!(snapshot.len(), 2);
        assert!(!snapshot.is_empty());

        store.set_bytes("keep", b"new").await.unwrap();
        store.delete("drop").await.unwrap();
        store.set_bytes("added", b"new").await.unwrap();

        assert_eq!(
            snapshot.get_bytes("keep").await.unwrap(),
            Some(b"old".to_vec()),
            "a write must not change the snapshot"
        );
        assert_eq!(
            snapshot.get_bytes("drop").await.unwrap(),
            Some(b"old".to_vec()),
            "a delete must not change the snapshot"
        );
        assert_eq!(
            snapshot.get_bytes("added").await.unwrap(),
            None,
            "a new key must not appear in the snapshot"
        );

        // The source store sees every change.
        assert_eq!(
            store.get_bytes("keep").await.unwrap(),
            Some(b"new".to_vec())
        );
        assert_eq!(store.get_bytes("drop").await.unwrap(), None);

        // `save` works through the read-only transaction path.
        let saved = snapshot.save().await.unwrap();
        assert!(!saved.is_empty(), "save must write the snapshot header");

        // An empty snapshot transaction commit is a no-op.
        let tx = snapshot.begin_tx().unwrap();
        tx.commit().await.unwrap();

        let error = snapshot
            .set_bytes("keep", b"new")
            .await
            .expect_err("a snapshot write must fail");
        assert_eq!(
            error,
            StoreError::Other("read-only snapshot: writes are rejected".to_string())
        );
        assert!(matches!(
            snapshot.delete("keep").await,
            Err(StoreError::Other(_))
        ));

        let error = snapshot
            .put_bytes("keep", b"new")
            .await
            .expect_err("a blind snapshot write must fail");
        assert_eq!(
            error,
            StoreError::Other("read-only snapshot: writes are rejected".to_string())
        );

        let tx = snapshot.begin_tx().unwrap();
        let error = tx
            .set_bytes("keep", b"new")
            .await
            .expect_err("a snapshot transaction write must fail");
        assert!(matches!(error, StoreError::Other(_)));
    }

    async fn assert_snapshot_reads_page<S>(store: &S)
    where
        S: Store + Sync,
    {
        for key in ["a", "b", "c"] {
            store.set_bytes(key, key.as_bytes()).await.unwrap();
        }
        let snapshot = store.snapshot().await.unwrap();

        let rows = snapshot
            .gets_bytes(Some(2), Direction::Next, (None, None))
            .await
            .unwrap();
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["a", "b"]);

        let rows = snapshot
            .gets_bytes(Some(2), Direction::Prev, (Some("c".to_string()), None))
            .await
            .unwrap();
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["c", "b"]);

        // The snapshot also supports prefix scans.
        let page: Page<String> = snapshot
            .scan_keys(crate::store::KeyPrefix::new("b"), None, 10)
            .await
            .unwrap();
        assert_eq!(page.items, ["b"]);

        // JSON reads work through `GetSetExt`.
        let text = snapshot
            .get::<String>("a")
            .await
            .expect_err("the raw bytes are not JSON");
        assert!(matches!(text, StoreError::Json(_)));

        assert_eq!(snapshot.len(), 3);
        assert!(!snapshot.is_empty());
    }

    #[test]
    fn empty_rows_make_an_empty_snapshot() {
        let snapshot = Snapshot::from_rows(Vec::new());
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        assert!(format!("{snapshot:?}").contains("len: 0"));
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_snapshot_is_stable_and_read_only() {
        let store = crate::store::BTreeStore::default();
        assert_snapshot_is_stable(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_snapshot_reads_both_directions() {
        let store = crate::store::BTreeStore::default();
        assert_snapshot_reads_page(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_snapshot_copy_is_independent() {
        let store = crate::store::BTreeStore::default();
        store.set_bytes("a", b"1").await.unwrap();
        let snapshot = store.snapshot().await.unwrap();
        let copy = snapshot.clone();
        store.set_bytes("a", b"2").await.unwrap();
        assert_eq!(
            copy.get_bytes("a").await.unwrap(),
            Some(b"1".to_vec()),
            "a clone must share the fixed map"
        );
    }

    #[cfg(feature = "oxkv")]
    async fn in_memory_oxkv() -> crate::store::OxKvStore {
        use std::sync::Arc;

        use crate::store::{MemStorage, OxKvStore};

        OxKvStore::builder()
            .with_store(Arc::new(MemStorage::new()))
            .skip_probe(true)
            .build()
            .await
            .unwrap()
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_snapshot_is_stable_and_read_only() {
        let store = in_memory_oxkv().await;
        assert_snapshot_is_stable(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_snapshot_reads_both_directions() {
        let store = in_memory_oxkv().await;
        assert_snapshot_reads_page(&store).await;
    }
}
