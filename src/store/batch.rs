//! Batch and prefix operations for a [`Store`].
//!
//! This module adds three operations through the [`BatchExt`] trait:
//!
//! - [`BatchExt::get_many`] reads several keys in one call.
//! - [`BatchExt::write_batch`] applies several writes in one transaction.
//! - [`BatchExt::delete_prefix_chunked`] removes every key under a prefix in
//!   bounded chunks.
//!
//! [`BatchOp`] describes one write for [`BatchExt::write_batch`].

use async_trait::async_trait;

use super::{Direction, GetSet, KeyPrefix, Result, ScanExt, Store, StoreError, Transaction};

/// One operation in a [`BatchExt::write_batch`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOp {
    /// Writes a value under a key.
    Set {
        /// The key to write.
        key: String,
        /// The value to write.
        value: Vec<u8>,
    },
    /// Removes a key.
    Delete {
        /// The key to remove.
        key: String,
    },
}

impl BatchOp {
    /// Creates a [`BatchOp::Set`] operation.
    #[must_use]
    pub fn set(key: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        Self::Set {
            key: key.into(),
            value: value.into(),
        }
    }

    /// Creates a [`BatchOp::Delete`] operation.
    #[must_use]
    pub fn delete(key: impl Into<String>) -> Self {
        Self::Delete { key: key.into() }
    }
}

/// Batch and prefix operations for a [`Store`].
///
/// Every [`Store`] implements this trait.
#[async_trait]
pub trait BatchExt: Store {
    /// Reads several keys in one call.
    ///
    /// The result uses the input order. `None` means that the key is absent.
    /// Duplicate keys return duplicate results.
    ///
    /// # Cost
    ///
    /// The method reads the keys one at a time. It calls [`GetSet::get_bytes`]
    /// once per key. It is not a server-side multi-get and it does not coalesce
    /// reads. The cost grows with the number of keys.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if a read fails.
    async fn get_many(&self, keys: &[&str]) -> Result<Vec<Option<Vec<u8>>>>;

    /// Applies all operations in one transaction.
    ///
    /// The whole slice is atomic. The method stages every operation in one
    /// transaction and commits it once. If any operation fails to stage, no
    /// operation commits. An empty slice does nothing.
    ///
    /// The method opens its own transaction. It cannot include reads in that
    /// transaction. To read and write in one transaction, call
    /// [`Store::begin_tx`] directly.
    ///
    /// There is no fixed maximum batch size. Memory bounds the slice. Chunk
    /// large batches at the caller.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if a write fails or a validator rejects an
    /// operation. The store is unchanged in that case.
    async fn write_batch(&self, ops: &[BatchOp]) -> Result<()>;

    /// Deletes every key that starts with `prefix` in chunks.
    ///
    /// The method deletes keys in chunks. Each chunk runs in one transaction
    /// with at most `batch` operations. Each chunk is atomic. The whole prefix
    /// is not one atomic unit. The method can stop between chunks. A failure
    /// can leave some keys deleted.
    ///
    /// A `batch` value bounds each scan. The loop continues until the prefix
    /// is empty. The method does not cover the whole prefix in one
    /// transaction.
    ///
    /// The method uses an exclusive upper bound for the prefix. It accepts an
    /// empty prefix. It accepts UTF-8 prefixes.
    ///
    /// Returns the number of keys deleted.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Other`] when `batch` is 0. Returns a
    /// [`StoreError`] if a scan or a write fails.
    async fn delete_prefix_chunked(&self, prefix: &str, batch: usize) -> Result<usize>;

    /// Deletes every key that starts with `prefix` in chunks.
    ///
    /// This method is deprecated. Use [`BatchExt::delete_prefix_chunked`].
    ///
    /// The delete is not atomic across chunks. The method can stop between
    /// chunks and leave some keys deleted.
    ///
    /// Returns the number of keys deleted.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Other`] when `batch` is 0. Returns a
    /// [`StoreError`] if a scan or a write fails.
    #[deprecated(note = "use `delete_prefix_chunked`; the delete is not atomic across chunks")]
    async fn delete_prefix(&self, prefix: &str, batch: usize) -> Result<usize> {
        self.delete_prefix_chunked(prefix, batch).await
    }

    /// Deletes every key that starts with `prefix` and is less than `cutoff`.
    ///
    /// `cutoff` is an exclusive lexicographic bound. A key that is equal to
    /// `cutoff` or above it is kept. The caller encodes time in the key, for
    /// example `timer/000001700000000000/`.
    ///
    /// The method deletes keys in chunks. Each chunk runs in one transaction
    /// with at most `batch` deletes. Each chunk is atomic. The whole range is
    /// not one atomic unit. A failure between chunks leaves the earlier
    /// chunks deleted. The method returns the number of keys deleted.
    ///
    /// This method is a cleanup helper. It is not a TTL. The store has no
    /// expiry metadata and no background expiry job. A caller must run the
    /// method on a schedule.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Other`] when `batch` is 0. Returns a
    /// [`StoreError`] if a scan or a write fails.
    async fn delete_prefix_before(&self, prefix: &str, cutoff: &str, batch: usize)
    -> Result<usize>;
}

#[async_trait]
impl<T: Store + Send + Sync + ?Sized> BatchExt for T {
    async fn get_many(&self, keys: &[&str]) -> Result<Vec<Option<Vec<u8>>>> {
        let mut values = Vec::with_capacity(keys.len());
        for key in keys {
            values.push(self.get_bytes(key).await?);
        }
        Ok(values)
    }

    async fn write_batch(&self, ops: &[BatchOp]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }

        let tx = self.begin_tx()?;
        for op in ops {
            match op {
                BatchOp::Set { key, value } => {
                    tx.set_bytes(key, value).await?;
                }
                BatchOp::Delete { key } => {
                    tx.delete(key).await?;
                }
            }
        }
        tx.commit().await
    }

    async fn delete_prefix_chunked(&self, prefix: &str, batch: usize) -> Result<usize> {
        if batch == 0 {
            return Err(StoreError::Other(
                "delete_prefix_chunked batch size must be greater than 0".to_string(),
            ));
        }

        let upper = prefix_upper_bound(prefix);
        let mut start = Some(prefix.to_string());
        let mut deleted = 0usize;

        loop {
            let rows = self
                .gets_bytes(Some(batch), Direction::Next, (start.clone(), None))
                .await?;
            if rows.is_empty() {
                break;
            }

            let scanned = rows.len();
            let mut keys = Vec::new();
            for row in rows {
                if upper.as_deref().is_some_and(|u| row.key.as_str() >= u) {
                    break;
                }
                if !row.key.starts_with(prefix) {
                    break;
                }
                keys.push(row.key);
            }
            if keys.is_empty() {
                break;
            }

            let ops: Vec<BatchOp> = keys
                .iter()
                .map(|key| BatchOp::delete(key.as_str()))
                .collect();
            self.write_batch(&ops).await?;
            deleted += keys.len();
            // The commit removed every key in `keys`. The next scan starts at
            // the last deleted key. The inclusive cursor is safe because that
            // key is now gone. The scan can return fewer keys than it scanned
            // when it stops at the prefix upper bound.
            start = keys.last().cloned();

            if scanned < batch {
                break;
            }
        }

        Ok(deleted)
    }

    async fn delete_prefix_before(
        &self,
        prefix: &str,
        cutoff: &str,
        batch: usize,
    ) -> Result<usize> {
        if batch == 0 {
            return Err(StoreError::Other(
                "delete_prefix_before batch size must be greater than 0".to_string(),
            ));
        }

        let mut after: Option<String> = None;
        let mut deleted = 0usize;

        loop {
            let page = self
                .scan_older_than(KeyPrefix::new(prefix), cutoff, after, batch)
                .await?;
            if page.items.is_empty() {
                break;
            }

            let ops: Vec<BatchOp> = page
                .items
                .iter()
                .map(|key| BatchOp::delete(key.as_str()))
                .collect();
            self.write_batch(&ops).await?;
            deleted += ops.len();

            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }

        Ok(deleted)
    }
}

/// Returns the smallest string that is greater than every string with
/// `prefix`.
///
/// Returns `None` when no such string exists. That case needs a prefix that
/// ends with the maximum code point and has no smaller character to increase.
fn prefix_upper_bound(prefix: &str) -> Option<String> {
    for (index, ch) in prefix.char_indices().rev() {
        if let Some(next) = next_char(ch) {
            let mut upper = String::with_capacity(index + next.len_utf8());
            upper.push_str(&prefix[..index]);
            upper.push(next);
            return Some(upper);
        }
    }
    None
}

/// Returns the next code point after `ch`. It skips the surrogate range.
fn next_char(ch: char) -> Option<char> {
    let code = u32::from(ch) + 1;
    let code = if code == 0xD800 { 0xE000 } else { code };
    char::from_u32(code)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use async_trait::async_trait;

    use super::{BatchExt, BatchOp};
    use crate::store::{Hooks, Result, Scope, Store, StoreError, StoreView, Validator};

    struct RejectBad;

    #[async_trait]
    impl Validator for RejectBad {
        fn scope(&self) -> Scope {
            Scope::Exact("bad".to_string())
        }

        async fn validate(&self, _ctx: &dyn StoreView, key: &str, value: &[u8]) -> Result<()> {
            if value == b"reject" {
                return Err(StoreError::Rejected {
                    key: key.to_string(),
                    reason: "test validator rejects this key".to_string(),
                });
            }
            Ok(())
        }
    }

    async fn assert_get_many_preserves_order<S: Store + Send + Sync + ?Sized>(store: &S) {
        store.set_bytes("a", b"1").await.unwrap();
        store.set_bytes("c", b"3").await.unwrap();

        let values = store.get_many(&["c", "missing", "a", "c"]).await.unwrap();
        assert_eq!(
            values,
            vec![
                Some(b"3".to_vec()),
                None,
                Some(b"1".to_vec()),
                Some(b"3".to_vec()),
            ]
        );
    }

    async fn assert_write_batch_applies_all_ops<S: Store + Send + Sync + ?Sized>(store: &S) {
        store.set_bytes("keep", b"old").await.unwrap();
        store.set_bytes("drop", b"old").await.unwrap();

        store
            .write_batch(&[
                BatchOp::set("keep", b"new".to_vec()),
                BatchOp::set("added", b"value".to_vec()),
                BatchOp::delete("drop"),
                BatchOp::delete("absent"),
            ])
            .await
            .unwrap();

        assert_eq!(
            store.get_bytes("keep").await.unwrap(),
            Some(b"new".to_vec())
        );
        assert_eq!(
            store.get_bytes("added").await.unwrap(),
            Some(b"value".to_vec())
        );
        assert_eq!(store.get_bytes("drop").await.unwrap(), None);
        assert_eq!(store.get_bytes("absent").await.unwrap(), None);

        store.write_batch(&[]).await.unwrap();
    }

    async fn assert_write_batch_is_atomic<S: Store + Send + Sync + ?Sized>(store: &S) {
        store.set_bytes("good", b"old").await.unwrap();
        store.set_bytes("bad", b"old").await.unwrap();

        let error = store
            .write_batch(&[
                BatchOp::set("good", b"new".to_vec()),
                BatchOp::set("bad", b"reject".to_vec()),
                BatchOp::set("after", b"new".to_vec()),
            ])
            .await
            .expect_err("the validator must reject the batch");
        assert!(
            matches!(error, StoreError::Rejected { .. }),
            "got {error:?}"
        );

        assert_eq!(
            store.get_bytes("good").await.unwrap(),
            Some(b"old".to_vec())
        );
        assert_eq!(store.get_bytes("bad").await.unwrap(), Some(b"old".to_vec()));
        assert_eq!(store.get_bytes("after").await.unwrap(), None);
    }

    async fn assert_delete_prefix_chunked<S: Store + Send + Sync + ?Sized>(store: &S) {
        for key in ["a:1", "a:2", "a:b:1", "b:1"] {
            store.set_bytes(key, b"v").await.unwrap();
        }

        let deleted = store.delete_prefix_chunked("a:", 2).await.unwrap();
        assert_eq!(deleted, 3);
        for key in ["a:1", "a:2", "a:b:1"] {
            assert!(!store.has(key).await.unwrap(), "must delete {key:?}");
        }
        assert!(store.has("b:1").await.unwrap(), "must keep b:1");

        assert_eq!(store.delete_prefix_chunked("zzz", 2).await.unwrap(), 0);

        for index in 0..5 {
            let key = format!("p:{index}");
            store.set_bytes(&key, b"v").await.unwrap();
        }
        assert_eq!(store.delete_prefix_chunked("p:", 1).await.unwrap(), 5);
        for index in 0..5 {
            let key = format!("p:{index}");
            assert!(!store.has(&key).await.unwrap(), "must delete {key:?}");
        }

        // A `usize::MAX` batch bounds each scan. The loop must continue
        // until the prefix is empty.
        for index in 0..3 {
            let key = format!("q:{index}");
            store.set_bytes(&key, b"v").await.unwrap();
        }
        assert_eq!(
            store.delete_prefix_chunked("q:", usize::MAX).await.unwrap(),
            3
        );
        for index in 0..3 {
            let key = format!("q:{index}");
            assert!(!store.has(&key).await.unwrap(), "must delete {key:?}");
        }

        let error = store
            .delete_prefix_chunked("a", 0)
            .await
            .expect_err("batch 0 must fail");
        assert!(matches!(error, StoreError::Other(_)), "got {error:?}");
    }

    const BOUNDARY_KEYS: [&str; 6] = ["", "a", "a:", "a:b", "ab", "b"];
    const REMAIN_A: [&str; 2] = ["", "b"];
    const REMAIN_A_COLON: [&str; 4] = ["", "a", "ab", "b"];
    const REMAIN_A_COLON_B: [&str; 5] = ["", "a", "a:", "ab", "b"];
    const REMAIN_AB: [&str; 5] = ["", "a", "a:", "a:b", "b"];
    const REMAIN_NONE: [&str; 0] = [];

    async fn assert_delete_prefix_chunked_boundaries<S: Store + Send + Sync + ?Sized>(store: &S) {
        let cases: [(&str, usize, &[&str]); 5] = [
            ("a", 4, &REMAIN_A),
            ("a:", 2, &REMAIN_A_COLON),
            ("a:b", 1, &REMAIN_A_COLON_B),
            ("ab", 1, &REMAIN_AB),
            ("", 6, &REMAIN_NONE),
        ];

        for (prefix, expected, remaining) in cases {
            let existing = store
                .gets_bytes(None, crate::store::Direction::Next, (None, None))
                .await
                .unwrap();
            for row in existing {
                store.delete(&row.key).await.unwrap();
            }
            for key in BOUNDARY_KEYS {
                store.set_bytes(key, b"v").await.unwrap();
            }

            let deleted = store.delete_prefix_chunked(prefix, 2).await.unwrap();
            assert_eq!(deleted, expected, "prefix {prefix:?}");
            for key in remaining {
                assert!(
                    store.has(key).await.unwrap(),
                    "prefix {prefix:?} must keep {key:?}"
                );
            }
            for key in BOUNDARY_KEYS {
                if !remaining.contains(&key) {
                    assert!(
                        !store.has(key).await.unwrap(),
                        "prefix {prefix:?} must delete {key:?}"
                    );
                }
            }
        }
    }

    #[allow(deprecated)]
    async fn assert_delete_prefix_chunked_and_alias<S: Store + Send + Sync + ?Sized>(store: &S) {
        for key in ["same:1", "same:2", "keep:1"] {
            store.set_bytes(key, b"v").await.unwrap();
        }

        let chunked = store.delete_prefix_chunked("same:", 1).await.unwrap();
        assert_eq!(chunked, 2);
        for key in ["same:1", "same:2"] {
            assert!(!store.has(key).await.unwrap(), "must delete {key:?}");
        }
        assert!(store.has("keep:1").await.unwrap(), "must keep keep:1");

        // Refill the same prefix. The deprecated alias must delete the same
        // keys.
        for key in ["same:1", "same:2"] {
            store.set_bytes(key, b"v").await.unwrap();
        }
        let alias = store.delete_prefix("same:", 1).await.unwrap();
        assert_eq!(alias, 2);
        for key in ["same:1", "same:2"] {
            assert!(!store.has(key).await.unwrap(), "must delete {key:?}");
        }
        assert!(store.has("keep:1").await.unwrap(), "must keep keep:1");
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_batch_operations() {
        let store = crate::store::BTreeStore::default();
        assert_get_many_preserves_order(&store).await;
        assert_write_batch_applies_all_ops(&store).await;
        assert_delete_prefix_chunked(&store).await;
        assert_delete_prefix_chunked_boundaries(&store).await;
        assert_delete_prefix_chunked_and_alias(&store).await;

        let store = crate::store::BTreeStore::default();
        store.add_validator(RejectBad);
        assert_write_batch_is_atomic(&store).await;
    }

    async fn assert_delete_prefix_before<S: Store + Send + Sync + ?Sized>(store: &S) {
        for key in [
            "timer/0001",
            "timer/0002",
            "timer/0003",
            "timer/0004",
            "timer/0005",
            "timer/0009",
            "timerx/0001",
            "keep",
        ] {
            store.set_bytes(key, b"v").await.unwrap();
        }

        // The cutoff is exclusive. The keys go in chunks of two.
        let deleted = store
            .delete_prefix_before("timer/", "timer/0005", 2)
            .await
            .unwrap();
        assert_eq!(deleted, 4);
        for key in ["timer/0001", "timer/0002", "timer/0003", "timer/0004"] {
            assert!(!store.has(key).await.unwrap(), "must delete {key:?}");
        }
        for key in ["timer/0005", "timer/0009", "timerx/0001", "keep"] {
            assert!(store.has(key).await.unwrap(), "must keep {key:?}");
        }

        // A second run has nothing to delete.
        assert_eq!(
            store
                .delete_prefix_before("timer/", "timer/0005", 2)
                .await
                .unwrap(),
            0
        );

        // The rest of the prefix goes in later chunks.
        assert_eq!(
            store
                .delete_prefix_before("timer/", "timer/9999", 2)
                .await
                .unwrap(),
            2
        );
        assert!(!store.has("timer/0005").await.unwrap());
        assert!(!store.has("timer/0009").await.unwrap());
        assert!(store.has("timerx/0001").await.unwrap());

        // A `usize::MAX` batch bounds each scan and the loop continues.
        for index in 0..3 {
            let key = format!("huge/{index}");
            store.set_bytes(&key, b"v").await.unwrap();
        }
        assert_eq!(
            store
                .delete_prefix_before("huge/", "huge/9", usize::MAX)
                .await
                .unwrap(),
            3
        );

        let error = store
            .delete_prefix_before("timer/", "timer/9999", 0)
            .await
            .expect_err("batch 0 must fail");
        assert!(matches!(error, StoreError::Other(_)), "got {error:?}");
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_delete_prefix_before_uses_chunks() {
        let store = crate::store::BTreeStore::default();
        assert_delete_prefix_before(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_batch_operations() {
        use std::sync::Arc;

        use crate::store::{MemStorage, OxKvStore};

        let build = || {
            OxKvStore::builder()
                .with_store(Arc::new(MemStorage::new()))
                .skip_probe(true)
                .build()
        };

        let store = build().await.unwrap();
        assert_get_many_preserves_order(&store).await;
        assert_write_batch_applies_all_ops(&store).await;
        assert_delete_prefix_chunked(&store).await;
        assert_delete_prefix_chunked_boundaries(&store).await;
        assert_delete_prefix_chunked_and_alias(&store).await;

        let store = build().await.unwrap();
        store.add_validator(RejectBad);
        assert_write_batch_is_atomic(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_delete_prefix_before_uses_chunks() {
        use std::sync::Arc;

        use crate::store::{MemStorage, OxKvStore};

        let store = OxKvStore::builder()
            .with_store(Arc::new(MemStorage::new()))
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        assert_delete_prefix_before(&store).await;
    }
}
