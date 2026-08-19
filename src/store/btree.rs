use async_trait::async_trait;
use std::collections::{BTreeMap, HashSet};
use std::ops::RangeBounds;
use std::sync::{Arc, RwLock};

use super::{Direction, GetSet, KeyValue, Result, Store, Transaction};

/// A key-value store backed by a B-tree.
#[derive(Default, Clone)]
pub struct BTreeStore {
    map: Arc<RwLock<BTreeMap<String, Vec<u8>>>>,
}

#[async_trait]
impl GetSet for BTreeStore {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let guard = self.map.read().unwrap();
        Ok(guard.get(key).cloned())
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        let guard = self.map.read().unwrap();
        Ok(guard.contains_key(key))
    }

    async fn delete(&mut self, key: &str) -> Result<bool> {
        let mut guard = self.map.write().unwrap();
        let removed = guard.remove(key).is_some();
        Ok(removed)
    }

    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut guard = self.map.write().unwrap();
        let prev = guard.insert(key.to_string(), value.to_vec());
        Ok(prev)
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        let guard = self.map.read().unwrap();
        match direction {
            Direction::Next => Ok(build_next(&guard, &cursor, limit)),
            Direction::Prev => Ok(build_prev(&guard, &cursor, limit)),
        }
    }
}

#[async_trait]
impl Store for BTreeStore {
    type Transaction = BTreeTx;

    fn begin_tx(&mut self) -> Result<Self::Transaction> {
        Ok(BTreeTx {
            store: Arc::clone(&self.map),
            overlay: BTreeMap::new(),
        })
    }
}

/// A transaction for the B-tree store.
pub struct BTreeTx {
    store: Arc<RwLock<BTreeMap<String, Vec<u8>>>>,
    overlay: BTreeMap<String, Option<Vec<u8>>>,
}

#[async_trait]
impl GetSet for BTreeTx {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if let Some(staged) = self.overlay.get(key) {
            return Ok(staged.clone());
        }
        let guard = self.store.read().unwrap();
        Ok(guard.get(key).cloned())
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        if let Some(staged) = self.overlay.get(key) {
            return Ok(staged.is_some());
        }
        let guard = self.store.read().unwrap();
        Ok(guard.contains_key(key))
    }

    async fn delete(&mut self, key: &str) -> Result<bool> {
        let existed = self.exists(key).await?;
        if existed {
            self.overlay.insert(key.to_string(), None);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = self.get_bytes(key).await?;
        self.overlay.insert(key.to_string(), Some(value.to_vec()));
        Ok(prev)
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        let guard = self.store.read().unwrap();
        match direction {
            Direction::Next => Ok(build_next_overlay(&guard, &self.overlay, &cursor, limit)),
            Direction::Prev => Ok(build_prev_overlay(&guard, &self.overlay, &cursor, limit)),
        }
    }
}

#[async_trait]
impl Transaction for BTreeTx {
    async fn commit(self) -> Result<()> {
        let mut guard = self.store.write().unwrap();
        for (k, v) in self.overlay {
            match v {
                Some(val) => {
                    guard.insert(k, val);
                }
                None => {
                    guard.remove(&k);
                }
            }
        }
        Ok(())
    }

    async fn rollback(self) -> Result<()> {
        Ok(())
    }
}

fn collect_next_range<R>(
    map: &BTreeMap<String, Vec<u8>>,
    range: R,
    limit: Option<usize>,
) -> Vec<KeyValue>
where
    R: RangeBounds<String>,
{
    let mut vec = Vec::new();
    for (k, v) in map.range(range) {
        if limit.is_some_and(|lim| vec.len() >= lim) {
            break;
        }
        vec.push(KeyValue {
            key: k.clone(),
            value: v.clone(),
        });
    }
    vec
}

fn collect_prev_range<R>(
    map: &BTreeMap<String, Vec<u8>>,
    range: R,
    limit: Option<usize>,
) -> Vec<KeyValue>
where
    R: RangeBounds<String>,
{
    let mut vec = Vec::new();
    for (k, v) in map.range(range).rev() {
        if limit.is_some_and(|lim| vec.len() >= lim) {
            break;
        }
        vec.push(KeyValue {
            key: k.clone(),
            value: v.clone(),
        });
    }
    vec
}

/// Builds a vector of `KeyValue` in ascending order (`Next` direction).
fn build_next(
    map: &BTreeMap<String, Vec<u8>>,
    cursor: &(Option<String>, Option<String>),
    limit: Option<u32>,
) -> Vec<KeyValue> {
    let limit = limit.map(|l| l as usize);
    match (&cursor.0, &cursor.1) {
        (Some(start), Some(end)) => {
            if start > end {
                return Vec::new();
            }
            collect_next_range(map, start.clone()..=end.clone(), limit)
        }
        (Some(start), None) => collect_next_range(map, start.clone().., limit),
        (None, Some(end)) => collect_next_range(map, ..=end.clone(), limit),
        (None, None) => collect_next_range(map, .., limit),
    }
}

/// Builds a vector of `KeyValue` in descending order (`Prev` direction).
fn build_prev(
    map: &BTreeMap<String, Vec<u8>>,
    cursor: &(Option<String>, Option<String>),
    limit: Option<u32>,
) -> Vec<KeyValue> {
    let limit = limit.map(|l| l as usize);
    let (start_opt, end_opt) = cursor;

    let Some(start) = start_opt else {
        return Vec::new();
    };

    if let Some(end) = end_opt {
        if start < end {
            return Vec::new();
        }
        collect_prev_range(map, end.clone()..=start.clone(), limit)
    } else {
        collect_prev_range(map, ..=start.clone(), limit)
    }
}

/// Builds a vector of `KeyValue` in ascending order merging store and overlay.
fn build_next_overlay(
    store: &BTreeMap<String, Vec<u8>>,
    overlay: &BTreeMap<String, Option<Vec<u8>>>,
    cursor: &(Option<String>, Option<String>),
    limit: Option<u32>,
) -> Vec<KeyValue> {
    let limit = limit.map(|l| l as usize);
    let overwritten_keys: HashSet<&String> = overlay.keys().collect();

    let store_items: Vec<(&String, &Vec<u8>)> = match (&cursor.0, &cursor.1) {
        (Some(start), Some(end)) => {
            if start > end {
                return Vec::new();
            }
            store
                .range(start.clone()..=end.clone())
                .filter(|(k, _)| !overwritten_keys.contains(k))
                .collect()
        }
        (Some(start), None) => store
            .range(start.clone()..)
            .filter(|(k, _)| !overwritten_keys.contains(k))
            .collect(),
        (None, Some(end)) => store
            .range(..=end.clone())
            .filter(|(k, _)| !overwritten_keys.contains(k))
            .collect(),
        (None, None) => store
            .iter()
            .filter(|(k, _)| !overwritten_keys.contains(k))
            .collect(),
    };

    let overlay_items: Vec<(&String, &Vec<u8>)> = match (&cursor.0, &cursor.1) {
        (Some(start), Some(end)) => {
            if start > end {
                return Vec::new();
            }
            overlay
                .range(start.clone()..=end.clone())
                .filter_map(|(k, v)| v.as_ref().map(|val| (k, val)))
                .collect()
        }
        (Some(start), None) => overlay
            .range(start.clone()..)
            .filter_map(|(k, v)| v.as_ref().map(|val| (k, val)))
            .collect(),
        (None, Some(end)) => overlay
            .range(..=end.clone())
            .filter_map(|(k, v)| v.as_ref().map(|val| (k, val)))
            .collect(),
        (None, None) => overlay
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|val| (k, val)))
            .collect(),
    };

    let mut merged = Vec::with_capacity(store_items.len() + overlay_items.len());
    let mut i = 0;
    let mut j = 0;

    while i < store_items.len() && j < overlay_items.len() {
        if limit.is_some_and(|lim| merged.len() >= lim) {
            break;
        }
        if store_items[i].0 <= overlay_items[j].0 {
            merged.push(KeyValue {
                key: store_items[i].0.clone(),
                value: store_items[i].1.clone(),
            });
            i += 1;
        } else {
            merged.push(KeyValue {
                key: overlay_items[j].0.clone(),
                value: overlay_items[j].1.clone(),
            });
            j += 1;
        }
    }

    while i < store_items.len() {
        if limit.is_some_and(|lim| merged.len() >= lim) {
            break;
        }
        merged.push(KeyValue {
            key: store_items[i].0.clone(),
            value: store_items[i].1.clone(),
        });
        i += 1;
    }

    while j < overlay_items.len() {
        if limit.is_some_and(|lim| merged.len() >= lim) {
            break;
        }
        merged.push(KeyValue {
            key: overlay_items[j].0.clone(),
            value: overlay_items[j].1.clone(),
        });
        j += 1;
    }

    merged
}

/// Builds a vector of `KeyValue` in descending order merging store and overlay.
fn build_prev_overlay(
    store: &BTreeMap<String, Vec<u8>>,
    overlay: &BTreeMap<String, Option<Vec<u8>>>,
    cursor: &(Option<String>, Option<String>),
    limit: Option<u32>,
) -> Vec<KeyValue> {
    let limit = limit.map(|l| l as usize);
    let (start_opt, end_opt) = cursor;

    let Some(start) = start_opt else {
        return Vec::new();
    };

    let overwritten_keys: HashSet<&String> = overlay.keys().collect();

    let store_items: Vec<(&String, &Vec<u8>)> = if let Some(end) = end_opt {
        if start < end {
            return Vec::new();
        }
        store
            .range(end.clone()..=start.clone())
            .rev()
            .filter(|(k, _)| !overwritten_keys.contains(k))
            .collect()
    } else {
        store
            .range(..=start.clone())
            .rev()
            .filter(|(k, _)| !overwritten_keys.contains(k))
            .collect()
    };

    let overlay_items: Vec<(&String, &Vec<u8>)> = if let Some(end) = end_opt {
        if start < end {
            return Vec::new();
        }
        overlay
            .range(end.clone()..=start.clone())
            .rev()
            .filter_map(|(k, v)| v.as_ref().map(|val| (k, val)))
            .collect()
    } else {
        overlay
            .range(..=start.clone())
            .rev()
            .filter_map(|(k, v)| v.as_ref().map(|val| (k, val)))
            .collect()
    };

    let mut merged = Vec::with_capacity(store_items.len() + overlay_items.len());
    let mut i = 0;
    let mut j = 0;

    while i < store_items.len() && j < overlay_items.len() {
        if limit.is_some_and(|lim| merged.len() >= lim) {
            break;
        }
        if store_items[i].0 >= overlay_items[j].0 {
            merged.push(KeyValue {
                key: store_items[i].0.clone(),
                value: store_items[i].1.clone(),
            });
            i += 1;
        } else {
            merged.push(KeyValue {
                key: overlay_items[j].0.clone(),
                value: overlay_items[j].1.clone(),
            });
            j += 1;
        }
    }

    while i < store_items.len() {
        if limit.is_some_and(|lim| merged.len() >= lim) {
            break;
        }
        merged.push(KeyValue {
            key: store_items[i].0.clone(),
            value: store_items[i].1.clone(),
        });
        i += 1;
    }

    while j < overlay_items.len() {
        if limit.is_some_and(|lim| merged.len() >= lim) {
            break;
        }
        merged.push(KeyValue {
            key: overlay_items[j].0.clone(),
            value: overlay_items[j].1.clone(),
        });
        j += 1;
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_store() -> BTreeStore {
        BTreeStore::default()
    }

    async fn populate_store(store: &mut BTreeStore) {
        let data = vec![
            ("a1", "apple"),
            ("a2", "apricot"),
            ("b1", "banana"),
            ("b2", "blueberry"),
            ("c1", "cherry"),
        ];
        for (k, v) in data {
            store.set_bytes(k, v.as_bytes()).await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_iterator_on_empty_store() {
        let store = BTreeStore::default();

        assert!(
            store
                .gets_bytes(None, Direction::Next, (None, None))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .gets_bytes(Some(5), Direction::Next, (None, None))
                .await
                .unwrap()
                .is_empty()
        );

        let _ = store
            .gets_bytes(
                Some(10),
                Direction::Next,
                (Some("z".to_string()), Some("a".to_string())),
            )
            .await
            .unwrap();

        assert!(
            store
                .gets_bytes(None, Direction::Prev, (None, None))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_set() {
        let mut store = new_store();
        let inserted = store.set_bytes("key1", b"value1").await.unwrap();
        assert_eq!(inserted, None);

        let val = store.get_bytes("key1").await.unwrap();
        assert_eq!(val, Some(b"value1".to_vec()));

        // Update existing key — should return the previous value
        let updated = store.set_bytes("key1", b"new_value").await.unwrap();
        assert_eq!(updated, Some(b"value1".to_vec()));
    }

    #[tokio::test]
    async fn test_set_missing_key() {
        let mut store = new_store();

        // Setting a missing key should return None (it's a new insertion)
        let set_missing = store.set_bytes("missing", b"anything").await.unwrap();
        assert_eq!(set_missing, None);
    }

    #[tokio::test]
    async fn test_update() {
        let mut store = new_store();
        store.set_bytes("key1", b"old").await.unwrap();

        // set_bytes on existing key returns the previous value (was an update)
        let updated = store.set_bytes("key1", b"new").await.unwrap();
        assert_eq!(updated, Some(b"old".to_vec()));
        let val = store.get_bytes("key1").await.unwrap();
        assert_eq!(val, Some(b"new".to_vec()));

        // set_bytes on missing key returns None (it's a new insertion)
        let updated_missing = store.set_bytes("missing", b"anything").await.unwrap();
        assert_eq!(updated_missing, None);
    }

    #[tokio::test]
    async fn test_delete() {
        let mut store = new_store();
        store.set_bytes("key1", b"value").await.unwrap();

        let deleted = store.delete("key1").await.unwrap();
        assert!(deleted);
        let val = store.get_bytes("key1").await.unwrap();
        assert_eq!(val, None);

        let deleted_missing = store.delete("missing").await.unwrap();
        assert!(!deleted_missing);
    }

    #[tokio::test]
    async fn test_gets_all() {
        let mut store = new_store();
        populate_store(&mut store).await;
        let result = store
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["a1", "a2", "b1", "b2", "c1"]);
    }

    #[tokio::test]
    async fn test_gets_with_limit() {
        let mut store = new_store();
        populate_store(&mut store).await;
        let result = store
            .gets_bytes(Some(2), Direction::Next, (None, None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["a1", "a2"]);
    }

    #[tokio::test]
    async fn test_gets_range_inclusive() {
        let mut store = new_store();
        populate_store(&mut store).await;
        let result = store
            .gets_bytes(
                None,
                Direction::Next,
                (Some("a2".to_string()), Some("b2".to_string())),
            )
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["a2", "b1", "b2"]);
    }

    #[tokio::test]
    async fn test_gets_range_start_only() {
        let mut store = new_store();
        populate_store(&mut store).await;
        let result = store
            .gets_bytes(None, Direction::Next, (Some("b1".to_string()), None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["b1", "b2", "c1"]);
    }

    #[tokio::test]
    async fn test_gets_range_end_only() {
        let mut store = new_store();
        populate_store(&mut store).await;
        let result = store
            .gets_bytes(None, Direction::Next, (None, Some("b1".to_string())))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["a1", "a2", "b1"]);
    }

    #[tokio::test]
    async fn test_gets_prev_without_start_returns_empty() {
        let mut store = new_store();
        populate_store(&mut store).await;
        let result = store
            .gets_bytes(None, Direction::Prev, (None, None))
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_gets_prev_with_valid_range() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // start > end, so walking backward from "b2" to "a2" inclusive
        let result = store
            .gets_bytes(
                None,
                Direction::Prev,
                (Some("b2".to_string()), Some("a2".to_string())),
            )
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["b2", "b1", "a2"]);
    }

    #[tokio::test]
    async fn test_gets_prev_with_start_only() {
        let mut store = new_store();
        populate_store(&mut store).await;
        let result = store
            .gets_bytes(None, Direction::Prev, (Some("b1".to_string()), None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["b1", "a2", "a1"]);
    }

    #[tokio::test]
    async fn test_gets_prev_with_start_less_than_end_returns_empty() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // start < end, invalid for Prev should be empty
        let result = store
            .gets_bytes(
                None,
                Direction::Prev,
                (Some("a2".to_string()), Some("b2".to_string())),
            )
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_transaction_commit() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Start a transaction and make changes
        let mut tx = store.begin_tx().unwrap();
        tx.set_bytes("key1", b"updated_in_tx").await.unwrap();
        tx.commit().await.unwrap();

        // Verify the change is visible after commit
        let val = store.get_bytes("key1").await.unwrap();
        assert_eq!(val, Some(b"updated_in_tx".to_vec()));
    }

    #[tokio::test]
    async fn test_transaction_rollback() {
        let mut store = new_store();

        // Start a transaction and make changes that don't affect existing keys
        let mut tx = store.begin_tx().unwrap();
        tx.set_bytes("new_key", b"will_be_rolled_back")
            .await
            .unwrap();

        // Verify the uncommitted change is visible within the transaction
        let val_in_tx = tx.get_bytes("new_key").await.unwrap();
        assert_eq!(val_in_tx, Some(b"will_be_rolled_back".to_vec()));

        // Rollback the transaction — changes should be discarded
        tx.rollback().await.unwrap();

        // After rollback, the key should not exist (change was discarded)
        let val_after_rollback = store.get_bytes("new_key").await.unwrap();
        assert_eq!(val_after_rollback, None);
    }

    #[tokio::test]
    async fn test_transaction_isolation() {
        // Note: redb uses MVCC which provides isolation differently than HashMap snapshot.
        // In redb, changes within a transaction are not visible to concurrent read transactions
        // until the transaction is committed. This test verifies that behavior.
        let mut store = new_store();

        // Start a write transaction and make changes
        let mut tx = store.begin_tx().unwrap();
        tx.set_bytes("key1", b"isolation_test_value").await.unwrap();

        // Try to read in the same transaction — should see the uncommitted change
        let val_in_tx = tx.get_bytes("key1").await.unwrap();
        assert_eq!(val_in_tx, Some(b"isolation_test_value".to_vec()));

        // Commit the transaction
        tx.commit().await.unwrap();

        // Now read from outside — should see the committed value
        let val_after_commit = store.get_bytes("key1").await.unwrap();
        assert_eq!(val_after_commit, Some(b"isolation_test_value".to_vec()));
    }

    /// Tests basic operations on a freshly created (empty) store.
    #[tokio::test]
    async fn test_gets_from_empty_store() {
        let store = new_store();
        // Reading from an empty store should return None for any key
        let val = store.get_bytes("nonexistent").await.unwrap();
        assert_eq!(val, None);

        let exists = store.exists("nonexistent").await.unwrap();
        assert!(!exists);
    }

    /// Tests that deleting a non-existent key returns false and doesn't error.
    #[tokio::test]
    async fn test_delete_non_existent() {
        let mut store = new_store();
        // Deleting from an empty store should return false without panicking
        let deleted = store.delete("does_not_exist").await.unwrap();
        assert!(!deleted);
    }

    /// Tests that `exists` returns correct values across set/delete operations.
    #[tokio::test]
    async fn test_exists_after_set_and_delete() {
        let mut store = new_store();
        // Key does not exist yet
        assert!(!store.exists("key1").await.unwrap());

        // After set, key should exist
        store.set_bytes("key1", b"value1").await.unwrap();
        assert!(store.exists("key1").await.unwrap());

        // After delete, key should not exist again
        store.delete("key1").await.unwrap();
        assert!(!store.exists("key1").await.unwrap());
    }

    /// Tests `BTreeStore::new()` constructor creates a valid in-memory database.
    #[tokio::test]
    async fn test_store_new() {
        let store = BTreeStore::default();
        // A freshly constructed store should behave like an empty store
        assert!(store.get_bytes("any").await.unwrap().is_none());
        assert!(!store.exists("any").await.unwrap());
    }

    /// Tests transactional `BTreeTx` operations: `get_bytes`, `exists`, `delete`, `set_bytes` inside tx.
    #[tokio::test]
    async fn test_btree_tx_operations() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Begin a transaction and use all GetSet methods on BTreeTx directly
        let mut tx = store.begin_tx().unwrap();

        // get_bytes in tx should see existing data
        let val = tx.get_bytes("a1").await.unwrap();
        assert_eq!(val, Some(b"apple".to_vec()));

        // exists in tx
        assert!(tx.exists("b2").await.unwrap());
        assert!(!tx.exists("missing").await.unwrap());

        // delete in tx — should return true and the value is gone within this tx scope
        let del = tx.delete("c1").await.unwrap();
        assert!(del);
        assert_eq!(tx.get_bytes("c1").await.unwrap(), None);

        // set_bytes in tx
        let prev = tx.set_bytes("new_key", b"hello").await.unwrap();
        assert_eq!(prev, None);
        assert_eq!(
            tx.get_bytes("new_key").await.unwrap(),
            Some(b"hello".to_vec())
        );
    }

    /// Tests transaction commit makes `BTreeTx` changes visible to the outer store.
    #[tokio::test]
    async fn test_tx_commit_direct() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let mut tx = store.begin_tx().unwrap();
        // Modify an existing key inside the transaction
        let old_val = tx.set_bytes("a1", b"changed").await.unwrap();
        assert_eq!(old_val, Some(b"apple".to_vec()));

        // Before commit, outer store still sees original value (MVCC isolation)
        assert_eq!(
            store.get_bytes("a1").await.unwrap(),
            Some(b"apple".to_vec())
        );

        tx.commit().await.unwrap();

        // After commit, outer store sees updated value
        assert_eq!(
            store.get_bytes("a1").await.unwrap(),
            Some(b"changed".to_vec())
        );
    }

    /// Tests transaction rollback discards `BTreeTx` changes and keeps outer store consistent.
    #[tokio::test]
    async fn test_tx_rollback_direct() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let mut tx = store.begin_tx().unwrap();
        // Add a brand-new key inside the transaction
        tx.set_bytes("secret", b"top_secret").await.unwrap();

        // Inside tx, we can see our change
        assert_eq!(
            tx.get_bytes("secret").await.unwrap(),
            Some(b"top_secret".to_vec())
        );

        // Outside tx, it's not visible yet (MVCC isolation)
        assert_eq!(store.get_bytes("secret").await.unwrap(), None);

        tx.rollback().await.unwrap();

        // After rollback, the key should never have existed outside the tx
        assert_eq!(store.get_bytes("secret").await.unwrap(), None);
    }

    /// Tests committing a delete inside a transaction propagates to the outer store.
    #[tokio::test]
    async fn test_tx_commit_delete() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Insert a key that will be deleted in the transaction
        store
            .set_bytes("to_delete", b"should_disappear")
            .await
            .unwrap();

        let mut tx = store.begin_tx().unwrap();
        assert_eq!(
            tx.get_bytes("to_delete").await.unwrap(),
            Some(b"should_disappear".to_vec())
        );

        // Delete inside the transaction — not yet visible outside (MVCC isolation)
        assert_eq!(
            store.get_bytes("to_delete").await.unwrap(),
            Some(b"should_disappear".to_vec())
        );

        tx.delete("to_delete").await.unwrap();

        // Outside tx, key still exists until commit (MVCC isolation)
        let val = store.get_bytes("to_delete").await.unwrap();
        assert_eq!(val, Some(b"should_disappear".to_vec()));

        tx.commit().await.unwrap();

        // After commit, the outer store must reflect the deletion
        assert_eq!(store.get_bytes("to_delete").await.unwrap(), None);
    }

    /// Tests rolling back a delete inside a transaction restores the original key.
    #[tokio::test]
    async fn test_tx_rollback_delete() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Insert an existing key — it will be deleted in the transaction and rolled back
        store
            .set_bytes("protected_key", b"important_data")
            .await
            .unwrap();

        let mut tx = store.begin_tx().unwrap();
        assert_eq!(
            tx.get_bytes("protected_key").await.unwrap(),
            Some(b"important_data".to_vec())
        );

        // Outside tx, key still exists (MVCC isolation)
        assert_eq!(
            store.get_bytes("protected_key").await.unwrap(),
            Some(b"important_data".to_vec())
        );

        // Delete inside the transaction
        tx.delete("protected_key").await.unwrap();

        // Outside tx, key is still visible until commit/rollback
        let val = store.get_bytes("protected_key").await.unwrap();
        assert_eq!(val, Some(b"important_data".to_vec()));

        // Rollback — delete should be undone, original value restored
        tx.rollback().await.unwrap();

        // After rollback, the key must still exist with its original value
        let val_after_rollback = store.get_bytes("protected_key").await.unwrap();
        assert_eq!(val_after_rollback, Some(b"important_data".to_vec()));
    }

    /// Tests transaction isolation: concurrent reads don't see uncommitted writes.
    #[tokio::test]
    async fn test_tx_isolation_direct() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Start a write tx and insert a key
        let mut writer = store.begin_tx().unwrap();
        writer
            .set_bytes("isolated_key", b"writer_value")
            .await
            .unwrap();

        // The outer store must NOT see this uncommitted change (MVCC isolation)
        assert_eq!(store.get_bytes("isolated_key").await.unwrap(), None);
        assert!(!store.exists("isolated_key").await.unwrap());

        writer.commit().await.unwrap();

        // Now the outer store sees it
        assert_eq!(
            store.get_bytes("isolated_key").await.unwrap(),
            Some(b"writer_value".to_vec())
        );
    }

    /// Tests `gets`  — range next with start > end returns empty.
    #[tokio::test]
    async fn test_gets_next_invalid_range() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // start > end for Next means invalid ascending range → should return empty
        let result = store
            .gets_bytes(
                None,
                Direction::Next,
                (Some("b2".to_string()), Some("a2".to_string())),
            )
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    /// Tests `gets`  — range prev with start < end returns empty.
    #[tokio::test]
    async fn test_gets_prev_invalid_range() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // start < end for Prev means invalid descending range → should return empty
        let result = store
            .gets_bytes(
                None,
                Direction::Prev,
                (Some("a2".to_string()), Some("b2".to_string())),
            )
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    /// Tests `gets_bytes` Prev direction with limit on a populated store.
    #[tokio::test]
    async fn test_gets_prev_with_limit() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // From "c1" backwards, limited to 2 items → should return ["c1", "b2"]
        let result = store
            .gets_bytes(Some(2), Direction::Prev, (Some("c1".to_string()), None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["c1", "b2"]);
    }

    /// Tests `gets_bytes` Next direction with limit on a populated store.
    #[tokio::test]
    async fn test_gets_next_with_limit() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // From "b2" forward, limited to 1 item → should return ["b2"]
        let result = store
            .gets_bytes(Some(1), Direction::Next, (Some("b2".to_string()), None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["b2"]);
    }

    /// Tests that getting a value with an empty value string works correctly.
    #[tokio::test]
    async fn test_set_get_empty_value() {
        let mut store = new_store();
        store.set_bytes("empty", b"").await.unwrap();
        let val = store.get_bytes("empty").await.unwrap();
        assert_eq!(val, Some(b"".to_vec()));
    }

    /// Tests that `delete` returns false when the key does not exist in a transaction.
    #[tokio::test]
    async fn test_delete_non_existent_in_tx() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let mut tx = store.begin_tx().unwrap();
        let result = tx.delete("nonexistent").await.unwrap();
        assert!(!result);
    }

    /// Tests that `set_bytes` with an existing key updates the value and returns previous.
    #[tokio::test]
    async fn test_update_returns_previous_value() {
        let mut store = new_store();
        store.set_bytes("k", b"v1").await.unwrap();
        let prev = store.set_bytes("k", b"v2").await.unwrap();
        assert_eq!(prev, Some(b"v1".to_vec()));
    }

    /// Tests that `gets_bytes` Prev direction with `start_only` returns items in descending order.
    #[tokio::test]
    async fn test_gets_prev_start_only_ordering() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // From "c1" backwards, no limit → full reverse traversal
        let result = store
            .gets_bytes(None, Direction::Prev, (Some("c1".to_string()), None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["c1", "b2", "b1", "a2", "a1"]);
    }

    /// Tests `gets_bytes` Next direction with full range and limit.
    #[tokio::test]
    async fn test_gets_range_full_limit() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // All items, limited to 3 → should return ["a1", "a2", "b1"]
        let result = store
            .gets_bytes(Some(3), Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(result.len(), 3);
    }

    /// Tests `begin_tx` returns a valid transaction handle that can be committed.
    #[tokio::test]
    async fn test_begin_tx_and_commit() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let mut tx = store.begin_tx().unwrap(); // previously untested direct path
        tx.set_bytes("tx_key", b"tx_value").await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(
            store.get_bytes("tx_key").await.unwrap(),
            Some(b"tx_value".to_vec())
        );
    }

    /// Tests `begin_tx` returns a valid transaction handle that can be rolled back.
    #[tokio::test]
    async fn test_begin_tx_and_rollback() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let mut tx = store.begin_tx().unwrap(); // previously untested direct path
        tx.set_bytes("tx_key", b"tx_value").await.unwrap();
        tx.rollback().await.unwrap();

        assert_eq!(store.get_bytes("tx_key").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_gets_limit_on_empty_store() {
        let store = new_store();
        // Empty store with limit should return no items
        let result = store
            .gets_bytes(Some(10), Direction::Next, (None, None))
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    /// Tests getting a range in Prev direction on an empty store returns empty.
    #[tokio::test]
    async fn test_gets_prev_on_empty_store() {
        let store = new_store();
        // Empty store with Prev should return no items for any cursor config
        let result = store
            .gets_bytes(None, Direction::Prev, (Some("z".to_string()), None))
            .await
            .unwrap();
        assert!(result.is_empty());

        let result2 = store
            .gets_bytes(None, Direction::Prev, (None, Some("z".to_string())))
            .await
            .unwrap();
        // Prev with end-only on empty should also be empty
        assert!(result2.is_empty());
    }
}
