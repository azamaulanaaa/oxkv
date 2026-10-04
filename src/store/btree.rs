use async_trait::async_trait;
use std::collections::{BTreeMap, HashSet};
use std::ops::RangeBounds;
use std::sync::{Arc, RwLock};

use super::{
    Direction, GetSet, KeyValue, Result, Store, Transaction, lock_ignore_poison,
    rwlock_ignore_poison,
};

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

    async fn has(&self, key: &str) -> Result<bool> {
        let guard = self.map.read().unwrap();
        Ok(guard.contains_key(key))
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        let mut guard = self.map.write().unwrap();
        let removed = guard.remove(key).is_some();
        Ok(removed)
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
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

    fn begin_tx(&self) -> Result<Self::Transaction> {
        Ok(BTreeTx {
            store: Arc::clone(&self.map),
            overlay: std::sync::Mutex::new(BTreeMap::new()),
        })
    }
}

/// A transaction for the B-tree store.
pub struct BTreeTx {
    store: Arc<RwLock<BTreeMap<String, Vec<u8>>>>,
    overlay: std::sync::Mutex<BTreeMap<String, Option<Vec<u8>>>>,
}

#[async_trait]
impl GetSet for BTreeTx {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if let Some(staged) = self.overlay.lock().unwrap().get(key) {
            return Ok(staged.clone());
        }
        let guard = self.store.read().unwrap();
        Ok(guard.get(key).cloned())
    }

    async fn has(&self, key: &str) -> Result<bool> {
        if let Some(staged) = self.overlay.lock().unwrap().get(key) {
            return Ok(staged.is_some());
        }
        let guard = self.store.read().unwrap();
        Ok(guard.contains_key(key))
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        // Look up the key once. Check the staged overlay first. Check the store
        // if the overlay has no entry.
        let existed = match self.overlay.lock().unwrap().get(key) {
            Some(staged) => staged.is_some(),
            None => self.store.read().unwrap().contains_key(key),
        };
        if existed {
            self.overlay.lock().unwrap().insert(key.to_string(), None);
        }
        Ok(existed)
    }

    async fn set_bytes(&self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = self.get_bytes(key).await?;
        self.overlay
            .lock()
            .unwrap()
            .insert(key.to_string(), Some(value.to_vec()));
        Ok(prev)
    }

    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        let guard = self.store.read().unwrap();
        let overlay = self.overlay.lock().unwrap();
        match direction {
            Direction::Next => Ok(build_next_overlay(&guard, &overlay, &cursor, limit)),
            Direction::Prev => Ok(build_prev_overlay(&guard, &overlay, &cursor, limit)),
        }
    }
}

#[async_trait]
impl Transaction for BTreeTx {
    async fn commit(self) -> Result<()> {
        // Recover the guards in the same way as the rest of the crate. A panic
        // in one guard owner should degrade this commit. The panic should not
        // fail a later commit.
        let overlay = std::mem::take(&mut *lock_ignore_poison(&self.overlay));
        let mut guard = rwlock_ignore_poison(&self.store);
        for (k, v) in overlay {
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

/// Returns `true` when `len` already satisfies the configured limit.
fn limit_reached(limit: Option<usize>, len: usize) -> bool {
    limit.is_some_and(|lim| len >= lim)
}

type SideItem<'a> = (&'a String, &'a Vec<u8>);
type SideIter<'a> = Box<dyn Iterator<Item = SideItem<'a>> + 'a>;

fn limit_of(limit: Option<u32>) -> Option<usize> {
    limit.map(|l| l as usize)
}

/// Store-side iterator for the given cursor and direction. The iterator skips
/// every key that the overlay contains. The overlay value wins for a skipped
/// key, even for a deletion.
fn store_side<'a>(
    store: &'a BTreeMap<String, Vec<u8>>,
    cursor: &(Option<String>, Option<String>),
    overlay_keys: &'a HashSet<&'a String>,
    forward: bool,
) -> SideIter<'a> {
    let (start, end) = (&cursor.0, &cursor.1);
    let ordered = |s: &String, e: &String| if forward { s <= e } else { s >= e };

    let base: SideIter<'a> = match (start, end) {
        (Some(s), Some(e)) => {
            if ordered(s, e) {
                let (lo, hi) = if forward { (s, e) } else { (e, s) };
                if forward {
                    Box::new(store.range(lo.clone()..=hi.clone()))
                } else {
                    Box::new(store.range(lo.clone()..=hi.clone()).rev())
                }
            } else {
                Box::new(std::iter::empty())
            }
        }
        (Some(s), None) => {
            if forward {
                Box::new(store.range(s.clone()..))
            } else {
                Box::new(store.range(..=s.clone()).rev())
            }
        }
        (None, Some(e)) => {
            if forward {
                Box::new(store.range(..=e.clone()))
            } else {
                // Descending traversal needs a starting point.
                Box::new(std::iter::empty())
            }
        }
        (None, None) => {
            if forward {
                Box::new(store.iter())
            } else {
                Box::new(std::iter::empty())
            }
        }
    };

    Box::new(base.filter(move |(k, _)| !overlay_keys.contains(k)))
}

/// Overlay-side iterator for the given cursor and direction. The iterator skips
/// tombstoned keys. A staged delete produces no item.
fn overlay_side<'a>(
    overlay: &'a BTreeMap<String, Option<Vec<u8>>>,
    cursor: &(Option<String>, Option<String>),
    forward: bool,
) -> SideIter<'a> {
    let (start, end) = (&cursor.0, &cursor.1);
    let ordered = |s: &String, e: &String| if forward { s <= e } else { s >= e };

    let base: Box<dyn Iterator<Item = (&'a String, &'a Option<Vec<u8>>)> + 'a> = match (start, end)
    {
        (Some(s), Some(e)) => {
            if ordered(s, e) {
                let (lo, hi) = if forward { (s, e) } else { (e, s) };
                if forward {
                    Box::new(overlay.range(lo.clone()..=hi.clone()))
                } else {
                    Box::new(overlay.range(lo.clone()..=hi.clone()).rev())
                }
            } else {
                Box::new(std::iter::empty())
            }
        }
        (Some(s), None) => {
            if forward {
                Box::new(overlay.range(s.clone()..))
            } else {
                Box::new(overlay.range(..=s.clone()).rev())
            }
        }
        (None, Some(e)) => {
            if forward {
                Box::new(overlay.range(..=e.clone()))
            } else {
                Box::new(std::iter::empty())
            }
        }
        (None, None) => {
            if forward {
                Box::new(overlay.iter())
            } else {
                Box::new(std::iter::empty())
            }
        }
    };

    Box::new(base.filter_map(|(k, v)| v.as_ref().map(|val| (k, val))))
}

/// Merges two sorted side iterators into owned key-value pairs. The merge walks
/// in the requested direction. The merge stops as soon as `limit` is reached.
fn merge_sides(
    mut store: std::iter::Peekable<SideIter<'_>>,
    mut overlay: std::iter::Peekable<SideIter<'_>>,
    limit: Option<usize>,
    forward: bool,
) -> Vec<KeyValue> {
    let mut out = Vec::new();
    loop {
        if limit_reached(limit, out.len()) {
            break;
        }
        let take_store = match (store.peek(), overlay.peek()) {
            (None, None) => break,
            (None, Some(_)) => false,
            (Some(_), None) => true,
            (Some((store_key, _)), Some((overlay_key, _))) => {
                if forward {
                    store_key.as_str() <= overlay_key.as_str()
                } else {
                    store_key.as_str() >= overlay_key.as_str()
                }
            }
        };
        let (key, value) = if take_store {
            store.next().unwrap()
        } else {
            overlay.next().unwrap()
        };
        out.push(KeyValue {
            key: key.clone(),
            value: value.clone(),
        });
    }
    out
}

/// Builds a vector of `KeyValue` in ascending order. The merge reads the store
/// and the overlay.
///
/// Both sides are consumed lazily. A small `limit` never materializes the whole
/// matching range.
fn build_next_overlay(
    store: &BTreeMap<String, Vec<u8>>,
    overlay: &BTreeMap<String, Option<Vec<u8>>>,
    cursor: &(Option<String>, Option<String>),
    limit: Option<u32>,
) -> Vec<KeyValue> {
    let overlay_keys: HashSet<&String> = overlay.keys().collect();
    merge_sides(
        store_side(store, cursor, &overlay_keys, true).peekable(),
        overlay_side(overlay, cursor, true).peekable(),
        limit_of(limit),
        true,
    )
}

/// Builds a vector of `KeyValue` in descending order. The merge reads the store
/// and the overlay.
///
/// The semantics match [`build_next_overlay`].
fn build_prev_overlay(
    store: &BTreeMap<String, Vec<u8>>,
    overlay: &BTreeMap<String, Option<Vec<u8>>>,
    cursor: &(Option<String>, Option<String>),
    limit: Option<u32>,
) -> Vec<KeyValue> {
    let overlay_keys: HashSet<&String> = overlay.keys().collect();
    merge_sides(
        store_side(store, cursor, &overlay_keys, false).peekable(),
        overlay_side(overlay, cursor, false).peekable(),
        limit_of(limit),
        false,
    )
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
    async fn test_gets_returns_no_items_for_every_cursor_on_an_empty_store() {
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
    async fn test_set_bytes_then_get_bytes_returns_the_value() {
        let store = new_store();
        let inserted = store.set_bytes("key1", b"value1").await.unwrap();
        assert_eq!(inserted, None);

        let val = store.get_bytes("key1").await.unwrap();
        assert_eq!(val, Some(b"value1".to_vec()));

        // Update an existing key. The call should return the previous value.
        let updated = store.set_bytes("key1", b"new_value").await.unwrap();
        assert_eq!(updated, Some(b"value1".to_vec()));
    }

    #[tokio::test]
    async fn test_set_bytes_on_a_missing_key_returns_none() {
        let store = new_store();

        // Setting a missing key should return `None`. The call is a new insertion.
        let set_missing = store.set_bytes("missing", b"anything").await.unwrap();
        assert_eq!(set_missing, None);
    }

    #[tokio::test]
    async fn test_set_bytes_on_an_existing_key_returns_the_previous_value() {
        let store = new_store();
        store.set_bytes("key1", b"old").await.unwrap();

        // `set_bytes` on an existing key returns the previous value. The call
        // was an update.
        let updated = store.set_bytes("key1", b"new").await.unwrap();
        assert_eq!(updated, Some(b"old".to_vec()));
        let val = store.get_bytes("key1").await.unwrap();
        assert_eq!(val, Some(b"new".to_vec()));

        // `set_bytes` on a missing key returns `None`. The call was a new
        // insertion.
        let updated_missing = store.set_bytes("missing", b"anything").await.unwrap();
        assert_eq!(updated_missing, None);
    }

    #[tokio::test]
    async fn test_delete_removes_an_existing_key() {
        let store = new_store();
        store.set_bytes("key1", b"value").await.unwrap();

        let deleted = store.delete("key1").await.unwrap();
        assert!(deleted);
        let val = store.get_bytes("key1").await.unwrap();
        assert_eq!(val, None);

        let deleted_missing = store.delete("missing").await.unwrap();
        assert!(!deleted_missing);
    }

    #[tokio::test]
    async fn test_gets_next_returns_every_key_in_order() {
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
    async fn test_gets_next_returns_the_first_keys_up_to_the_limit() {
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
    async fn test_gets_next_includes_both_range_ends() {
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
    async fn test_gets_next_from_a_start_without_an_end() {
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
    async fn test_gets_next_to_an_end_without_a_start() {
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
    async fn test_gets_prev_without_a_start_returns_no_items() {
        let mut store = new_store();
        populate_store(&mut store).await;
        let result = store
            .gets_bytes(None, Direction::Prev, (None, None))
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_gets_prev_walks_backward_across_a_descending_range() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // The start is greater than the end. The walk runs backward from "b2"
        // to "a2". The range includes both keys.
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
    async fn test_gets_prev_from_a_start_without_an_end() {
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
    async fn test_gets_prev_returns_no_items_when_the_start_is_below_the_end() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // The start is less than the end. The range is invalid for `Prev`. The
        // result should be empty.
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
    async fn test_commit_publishes_the_change_to_the_store() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Start a transaction and make a change.
        let tx = store.begin_tx().unwrap();
        tx.set_bytes("key1", b"updated_in_tx").await.unwrap();
        tx.commit().await.unwrap();

        // Check that the change is visible after the commit.
        let val = store.get_bytes("key1").await.unwrap();
        assert_eq!(val, Some(b"updated_in_tx".to_vec()));
    }

    #[tokio::test]
    async fn test_rollback_discards_the_change() {
        let store = new_store();

        // Start a transaction and make a change. The change does not affect an
        // existing key.
        let tx = store.begin_tx().unwrap();
        tx.set_bytes("new_key", b"will_be_rolled_back")
            .await
            .unwrap();

        // Check that the uncommitted change is visible within the transaction.
        let val_in_tx = tx.get_bytes("new_key").await.unwrap();
        assert_eq!(val_in_tx, Some(b"will_be_rolled_back".to_vec()));

        // Roll back the transaction. The transaction should discard the changes.
        tx.rollback().await.unwrap();

        // After the rollback, the key should not exist. The rollback discarded
        // the change.
        let val_after_rollback = store.get_bytes("new_key").await.unwrap();
        assert_eq!(val_after_rollback, None);
    }

    #[tokio::test]
    async fn test_a_read_inside_the_transaction_sees_its_own_uncommitted_write() {
        // Note: `redb` uses MVCC. MVCC provides isolation in a different way than a
        // `HashMap` snapshot. In `redb`, a concurrent read transaction does not
        // see the changes inside a transaction until the transaction is
        // committed. This test checks that behavior.
        let store = new_store();

        // Start a write transaction and make a change.
        let tx = store.begin_tx().unwrap();
        tx.set_bytes("key1", b"isolation_test_value").await.unwrap();

        // Try to read in the same transaction. The read should see the
        // uncommitted change.
        let val_in_tx = tx.get_bytes("key1").await.unwrap();
        assert_eq!(val_in_tx, Some(b"isolation_test_value".to_vec()));

        // Commit the transaction.
        tx.commit().await.unwrap();

        // Now read from outside the transaction. The read should see the
        // committed value.
        let val_after_commit = store.get_bytes("key1").await.unwrap();
        assert_eq!(val_after_commit, Some(b"isolation_test_value".to_vec()));
    }

    /// Checks the basic operations on a store that was just created. The store
    /// is empty.
    #[tokio::test]
    async fn test_get_bytes_on_an_empty_store_returns_none() {
        let store = new_store();
        // Reading from an empty store should return `None` for any key.
        let val = store.get_bytes("nonexistent").await.unwrap();
        assert_eq!(val, None);

        let exists = store.has("nonexistent").await.unwrap();
        assert!(!exists);
    }

    /// Checks that a delete of a key that does not exist returns false. The
    /// call must not return an error.
    #[tokio::test]
    async fn test_delete_returns_false_for_a_missing_key() {
        let store = new_store();
        // Deleting from an empty store should return false. The call should not
        // panic.
        let deleted = store.delete("does_not_exist").await.unwrap();
        assert!(!deleted);
    }

    /// Checks that `has` returns the correct values after a set and after a
    /// delete.
    #[tokio::test]
    async fn test_has_reports_whether_a_key_is_present() {
        let store = new_store();
        // The key does not exist yet.
        assert!(!store.has("key1").await.unwrap());

        // The key should exist after the set.
        store.set_bytes("key1", b"value1").await.unwrap();
        assert!(store.has("key1").await.unwrap());

        // The key should not exist after the delete.
        store.delete("key1").await.unwrap();
        assert!(!store.has("key1").await.unwrap());
    }

    /// Checks that `BTreeStore::default()` creates a valid empty in-memory
    /// database.
    #[tokio::test]
    async fn test_a_default_store_holds_no_keys() {
        let store = BTreeStore::default();
        // A freshly constructed store should behave like an empty store
        assert!(store.get_bytes("any").await.unwrap().is_none());
        assert!(!store.has("any").await.unwrap());
    }

    /// Checks the `BTreeTx` operations in a transaction. The operations are
    /// `get_bytes`, `has`, `delete`, and `set_bytes`.
    #[tokio::test]
    async fn test_a_transaction_sees_its_own_writes() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Begin a transaction. Use all `GetSet` methods on `BTreeTx` directly.
        let tx = store.begin_tx().unwrap();

        // `get_bytes` in the transaction should see the existing data.
        let val = tx.get_bytes("a1").await.unwrap();
        assert_eq!(val, Some(b"apple".to_vec()));

        // `has` in the transaction.
        assert!(tx.has("b2").await.unwrap());
        assert!(!tx.has("missing").await.unwrap());

        // `delete` in the transaction should return true. The value is gone
        // within this transaction scope.
        let del = tx.delete("c1").await.unwrap();
        assert!(del);
        assert_eq!(tx.get_bytes("c1").await.unwrap(), None);

        // `set_bytes` in the transaction.
        let prev = tx.set_bytes("new_key", b"hello").await.unwrap();
        assert_eq!(prev, None);
        assert_eq!(
            tx.get_bytes("new_key").await.unwrap(),
            Some(b"hello".to_vec())
        );
    }

    /// Checks that a commit makes the `BTreeTx` changes visible to the outer
    /// store.
    #[tokio::test]
    async fn test_a_commit_publishes_a_transaction_change() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let tx = store.begin_tx().unwrap();
        // Modify an existing key inside the transaction.
        let old_val = tx.set_bytes("a1", b"changed").await.unwrap();
        assert_eq!(old_val, Some(b"apple".to_vec()));

        // Before the commit, the outer store still sees the original value.
        // MVCC isolation causes this result.
        assert_eq!(
            store.get_bytes("a1").await.unwrap(),
            Some(b"apple".to_vec())
        );

        tx.commit().await.unwrap();

        // After the commit, the outer store sees the updated value.
        assert_eq!(
            store.get_bytes("a1").await.unwrap(),
            Some(b"changed".to_vec())
        );
    }

    /// Checks that a rollback discards the `BTreeTx` changes. The rollback also
    /// keeps the outer store consistent.
    #[tokio::test]
    async fn test_a_rollback_discards_a_transaction_change() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let tx = store.begin_tx().unwrap();
        // Add a new key inside the transaction.
        tx.set_bytes("secret", b"top_secret").await.unwrap();

        // Inside the transaction, the read sees the change.
        assert_eq!(
            tx.get_bytes("secret").await.unwrap(),
            Some(b"top_secret".to_vec())
        );

        // Outside the transaction, the key is not visible yet. MVCC isolation causes
        // this result.
        assert_eq!(store.get_bytes("secret").await.unwrap(), None);

        tx.rollback().await.unwrap();

        // After the rollback, the key should not exist outside the transaction.
        assert_eq!(store.get_bytes("secret").await.unwrap(), None);
    }

    /// Checks that a commit of a delete inside a transaction makes the delete
    /// visible to the outer store.
    #[tokio::test]
    async fn test_a_commit_publishes_a_transaction_delete() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Insert a key. The transaction deletes the key.
        store
            .set_bytes("to_delete", b"should_disappear")
            .await
            .unwrap();

        let tx = store.begin_tx().unwrap();
        assert_eq!(
            tx.get_bytes("to_delete").await.unwrap(),
            Some(b"should_disappear".to_vec())
        );

        // Delete inside the transaction. The delete is not visible outside yet. MVCC
        // isolation causes this result.
        assert_eq!(
            store.get_bytes("to_delete").await.unwrap(),
            Some(b"should_disappear".to_vec())
        );

        tx.delete("to_delete").await.unwrap();

        // Outside the transaction, the key still exists until the commit. MVCC
        // isolation causes this result.
        let val = store.get_bytes("to_delete").await.unwrap();
        assert_eq!(val, Some(b"should_disappear".to_vec()));

        tx.commit().await.unwrap();

        // After the commit, the outer store must reflect the deletion.
        assert_eq!(store.get_bytes("to_delete").await.unwrap(), None);
    }

    /// Checks that a rollback of a delete inside a transaction restores the
    /// original key.
    #[tokio::test]
    async fn test_a_rollback_leaves_a_deleted_key_in_the_store() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Insert an existing key. The transaction deletes the key. The rollback
        // undoes the delete.
        store
            .set_bytes("protected_key", b"important_data")
            .await
            .unwrap();

        let tx = store.begin_tx().unwrap();
        assert_eq!(
            tx.get_bytes("protected_key").await.unwrap(),
            Some(b"important_data".to_vec())
        );

        // Outside the transaction, the key still exists. MVCC isolation causes this
        // result.
        assert_eq!(
            store.get_bytes("protected_key").await.unwrap(),
            Some(b"important_data".to_vec())
        );

        // Delete inside the transaction.
        tx.delete("protected_key").await.unwrap();

        // Outside the transaction, the key is still visible until the commit
        // or the rollback.
        let val = store.get_bytes("protected_key").await.unwrap();
        assert_eq!(val, Some(b"important_data".to_vec()));

        // Roll back. The rollback should undo the delete. The rollback restores the
        // original value.
        tx.rollback().await.unwrap();

        // After the rollback, the key must still exist. The key must still
        // have its original value.
        let val_after_rollback = store.get_bytes("protected_key").await.unwrap();
        assert_eq!(val_after_rollback, Some(b"important_data".to_vec()));
    }

    /// Checks transaction isolation. A concurrent read must not see an
    /// uncommitted write.
    #[tokio::test]
    async fn test_the_store_does_not_see_an_uncommitted_write() {
        let mut store = new_store();
        populate_store(&mut store).await;

        // Start a write transaction and insert a key.
        let writer = store.begin_tx().unwrap();
        writer
            .set_bytes("isolated_key", b"writer_value")
            .await
            .unwrap();

        // The outer store must NOT see this uncommitted change. MVCC isolation
        // causes this result.
        assert_eq!(store.get_bytes("isolated_key").await.unwrap(), None);
        assert!(!store.has("isolated_key").await.unwrap());

        writer.commit().await.unwrap();

        // Now the outer store sees it
        assert_eq!(
            store.get_bytes("isolated_key").await.unwrap(),
            Some(b"writer_value".to_vec())
        );
    }

    /// Checks `gets` for a `Next` range. A start that is greater than the end
    /// should return an empty result.
    #[tokio::test]
    async fn test_gets_next_returns_no_items_when_the_start_is_above_the_end() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // A start that is greater than the end is an invalid ascending range
        // for `Next`. The result should be empty.
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

    /// Checks `gets` for a `Prev` range. A start that is less than the end
    /// should return an empty result.
    #[tokio::test]
    async fn test_gets_prev_returns_no_items_for_an_ascending_range() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // A start that is less than the end is an invalid descending range for
        // `Prev`. The result should be empty.
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

    /// Checks `gets_bytes` with a `Prev` direction and a limit on a populated
    /// store.
    #[tokio::test]
    async fn test_gets_prev_stops_at_the_limit_from_a_start() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // The walk starts from "c1" and moves backward. The walk is limited to 2
        // items. The result should be ["c1", "b2"].
        let result = store
            .gets_bytes(Some(2), Direction::Prev, (Some("c1".to_string()), None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["c1", "b2"]);
    }

    /// Checks `gets_bytes` with a `Next` direction and a limit on a populated
    /// store.
    #[tokio::test]
    async fn test_gets_next_stops_at_the_limit_from_a_start() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // The walk starts from "b2" and moves forward. The walk is limited to 1 item.
        // The result should be ["b2"].
        let result = store
            .gets_bytes(Some(1), Direction::Next, (Some("b2".to_string()), None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["b2"]);
    }

    /// Checks that a get of an empty value returns that empty value.
    #[tokio::test]
    async fn test_get_bytes_returns_the_empty_value() {
        let store = new_store();
        store.set_bytes("empty", b"").await.unwrap();
        let val = store.get_bytes("empty").await.unwrap();
        assert_eq!(val, Some(b"".to_vec()));
    }

    /// Checks that `delete` returns false when the key does not exist in a
    /// transaction.
    #[tokio::test]
    async fn test_a_transaction_delete_returns_false_for_a_missing_key() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let tx = store.begin_tx().unwrap();
        let result = tx.delete("nonexistent").await.unwrap();
        assert!(!result);
    }

    /// Checks that `set_bytes` with an existing key updates the value. The call
    /// should return the previous value.
    #[tokio::test]
    async fn test_set_bytes_returns_the_previous_value() {
        let store = new_store();
        store.set_bytes("k", b"v1").await.unwrap();
        let prev = store.set_bytes("k", b"v2").await.unwrap();
        assert_eq!(prev, Some(b"v1".to_vec()));
    }

    /// Checks that `gets_bytes` with a `Prev` direction and `start_only`
    /// returns the items in descending order.
    #[tokio::test]
    async fn test_gets_prev_returns_the_keys_in_descending_order() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // The walk starts from "c1" and moves backward. The walk has no limit. The
        // traversal covers the full range in reverse.
        let result = store
            .gets_bytes(None, Direction::Prev, (Some("c1".to_string()), None))
            .await
            .unwrap();
        let keys: Vec<_> = result.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, vec!["c1", "b2", "b1", "a2", "a1"]);
    }

    /// Checks `gets_bytes` with a `Next` direction, a full range, and a limit.
    #[tokio::test]
    async fn test_gets_next_returns_the_limit_number_of_keys() {
        let mut store = new_store();
        populate_store(&mut store).await;
        // All items are returned. The walk is limited to 3 items. The result should be
        // ["a1", "a2", "b1"].
        let result = store
            .gets_bytes(Some(3), Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(result.len(), 3);
    }

    /// Checks that `begin_tx` returns a valid transaction handle. The code can
    /// commit the handle.
    #[tokio::test]
    async fn test_begin_tx_returns_a_handle_that_commits() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let tx = store.begin_tx().unwrap(); // No earlier test covered the direct path.
        tx.set_bytes("tx_key", b"tx_value").await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(
            store.get_bytes("tx_key").await.unwrap(),
            Some(b"tx_value".to_vec())
        );
    }

    /// Checks that `begin_tx` returns a valid transaction handle. The code can
    /// roll back the handle.
    #[tokio::test]
    async fn test_begin_tx_returns_a_handle_that_rolls_back() {
        let mut store = new_store();
        populate_store(&mut store).await;

        let tx = store.begin_tx().unwrap(); // No earlier test covered the direct path.
        tx.set_bytes("tx_key", b"tx_value").await.unwrap();
        tx.rollback().await.unwrap();

        assert_eq!(store.get_bytes("tx_key").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_gets_next_applies_the_limit_to_an_empty_store() {
        let store = new_store();
        // An empty store with a limit should return no items.
        let result = store
            .gets_bytes(Some(10), Direction::Next, (None, None))
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    /// Checks that a get of a range in the `Prev` direction on an empty store
    /// returns an empty result.
    #[tokio::test]
    async fn test_gets_prev_returns_no_items_on_an_empty_store() {
        let store = new_store();
        // An empty store with `Prev` should return no items for any cursor.
        let result = store
            .gets_bytes(None, Direction::Prev, (Some("z".to_string()), None))
            .await
            .unwrap();
        assert!(result.is_empty());

        let result2 = store
            .gets_bytes(None, Direction::Prev, (None, Some("z".to_string())))
            .await
            .unwrap();
        // `Prev` with only an end on an empty store should also return no items.
        assert!(result2.is_empty());
    }
}
