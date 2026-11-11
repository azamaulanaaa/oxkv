//! Batch and prefix operations for a [`Store`].
//!
//! This module adds three operations through the [`BatchExt`] trait:
//!
//! - [`BatchExt::get_many`] reads several keys in one call.
//! - [`BatchExt::write_batch`] applies several writes in one transaction.
//! - [`BatchExt::delete_prefix`] removes every key under a prefix in bounded
//!   chunks.
//!
//! [`BatchOp`] describes one write for [`BatchExt::write_batch`].

use async_trait::async_trait;

use super::{Direction, GetSet, Result, Store, StoreError, Transaction};

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
    /// Duplicate keys return duplicate results. This method reads the keys
    /// sequentially.
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
    /// There is no fixed maximum batch size. Memory bounds the slice. Chunk
    /// large batches at the caller.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if a write fails or a validator rejects an
    /// operation. The store is unchanged in that case.
    async fn write_batch(&self, ops: &[BatchOp]) -> Result<()>;

    /// Deletes every key that starts with `prefix`.
    ///
    /// The method deletes keys in chunks. Each chunk runs in one transaction
    /// with at most `batch` operations. Each chunk is atomic. The whole prefix
    /// is not one atomic unit. A failure can leave earlier chunks deleted.
    ///
    /// A `batch` value larger than `u32::MAX` caps each scan at `u32::MAX`
    /// keys. The loop continues until the prefix is empty. The method does not
    /// cover the whole prefix in one transaction.
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
    async fn delete_prefix(&self, prefix: &str, batch: usize) -> Result<usize>;
}

#[async_trait]
impl<T: Store + ?Sized> BatchExt for T {
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

    async fn delete_prefix(&self, prefix: &str, batch: usize) -> Result<usize> {
        if batch == 0 {
            return Err(StoreError::Other(
                "delete_prefix batch size must be greater than 0".to_string(),
            ));
        }

        let upper = prefix_upper_bound(prefix);
        let limit = u32::try_from(batch).unwrap_or(u32::MAX);
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
        let mut start = Some(prefix.to_string());
        let mut deleted = 0usize;

        loop {
            let rows = self
                .gets_bytes(Some(limit), Direction::Next, (start.clone(), None))
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

            if scanned < limit_usize {
                break;
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

    async fn assert_get_many_preserves_order<S: Store + ?Sized>(store: &S) {
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

    async fn assert_write_batch_applies_all_ops<S: Store + ?Sized>(store: &S) {
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

    async fn assert_write_batch_is_atomic<S: Store + ?Sized>(store: &S) {
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

    async fn assert_delete_prefix<S: Store + ?Sized>(store: &S) {
        for key in ["a:1", "a:2", "a:b:1", "b:1"] {
            store.set_bytes(key, b"v").await.unwrap();
        }

        let deleted = store.delete_prefix("a:", 2).await.unwrap();
        assert_eq!(deleted, 3);
        for key in ["a:1", "a:2", "a:b:1"] {
            assert!(!store.has(key).await.unwrap(), "must delete {key:?}");
        }
        assert!(store.has("b:1").await.unwrap(), "must keep b:1");

        assert_eq!(store.delete_prefix("zzz", 2).await.unwrap(), 0);

        for index in 0..5 {
            let key = format!("p:{index}");
            store.set_bytes(&key, b"v").await.unwrap();
        }
        assert_eq!(store.delete_prefix("p:", 1).await.unwrap(), 5);
        for index in 0..5 {
            let key = format!("p:{index}");
            assert!(!store.has(&key).await.unwrap(), "must delete {key:?}");
        }

        // A batch above `u32::MAX` caps each scan. The loop must continue
        // until the prefix is empty.
        for index in 0..3 {
            let key = format!("q:{index}");
            store.set_bytes(&key, b"v").await.unwrap();
        }
        assert_eq!(store.delete_prefix("q:", usize::MAX).await.unwrap(), 3);
        for index in 0..3 {
            let key = format!("q:{index}");
            assert!(!store.has(&key).await.unwrap(), "must delete {key:?}");
        }

        let error = store
            .delete_prefix("a", 0)
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

    async fn assert_delete_prefix_boundaries<S: Store + ?Sized>(store: &S) {
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

            let deleted = store.delete_prefix(prefix, 2).await.unwrap();
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

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_batch_operations() {
        let store = crate::store::BTreeStore::default();
        assert_get_many_preserves_order(&store).await;
        assert_write_batch_applies_all_ops(&store).await;
        assert_delete_prefix(&store).await;
        assert_delete_prefix_boundaries(&store).await;

        let store = crate::store::BTreeStore::default();
        store.add_validator(RejectBad);
        assert_write_batch_is_atomic(&store).await;
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
        assert_delete_prefix(&store).await;
        assert_delete_prefix_boundaries(&store).await;

        let store = build().await.unwrap();
        store.add_validator(RejectBad);
        assert_write_batch_is_atomic(&store).await;
    }
}
