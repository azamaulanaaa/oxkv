//! Transaction-level conditional writes.
//!
//! [`TxCas`] stages a write and a precondition in one transaction. The
//! transaction commits only when every precondition matches the committed
//! state. This trait covers several keys in one atomic commit. [`Cas`] covers
//! one key and completes before the call returns.
//!
//! # Contract
//!
//! Each method stages the write and its precondition. The method returns
//! `Ok(())` when the staging succeeds. The method does not read the committed
//! value and does not report a conflict. The outcome is known only at
//! [`commit`](Transaction::commit).
//!
//! The commit runs the precondition check and the overlay apply under one
//! lock or one write gate:
//!
//! - `BTreeTx` holds the shared map write lock.
//! - `OxKvTx` holds the store write gate.
//!
//! The check reads the committed state, not the transaction overlay. A
//! precondition therefore cannot be satisfied by a write that the same
//! transaction staged. If one precondition fails, the commit returns
//! [`StoreError::TxConflict`] and applies no staged write.
//!
//! ```
//! # #[cfg(feature = "btree")]
//! # mod example {
//! use oxkv::{BTreeStore, GetSet, Store, StoreError, Transaction, TxCas};
//!
//! # #[tokio::main(flavor = "current_thread")] async fn main() {
//! let store = BTreeStore::default();
//! store.put_bytes("entity", b"one").await.unwrap();
//!
//! let tx = store.begin_tx().unwrap();
//! tx.set_if_absent("claim:email", b"owner").await.unwrap();
//! tx.compare_and_set("entity", Some(b"one"), b"two").await.unwrap();
//! tx.commit().await.unwrap();
//!
//! let tx = store.begin_tx().unwrap();
//! tx.set_if_absent("claim:email", b"other").await.unwrap();
//! let error = tx.commit().await.unwrap_err();
//! assert!(matches!(error, StoreError::TxConflict { .. }));
//! assert_eq!(
//!     store.get_bytes("claim:email").await.unwrap(),
//!     Some(b"owner".to_vec())
//! );
//! # }
//! # }
//! ```
//!
//! # Limits
//!
//! The precondition list holds one entry for each key. A later call for the
//! same key replaces the earlier precondition. The overlay keeps the last
//! write for the key.
//!
//! [`Cas`]: super::Cas
//! [`StoreError::TxConflict`]: super::StoreError::TxConflict
//! [`Transaction::commit`]: super::Transaction::commit

use async_trait::async_trait;

use super::{Result, Transaction};

/// Conditional writes that apply at transaction commit.
///
/// Each method stages a write and a precondition. The precondition compares
/// the committed value at commit time. The method returns `Result<()>`. The
/// outcome is known only at [`commit`](Transaction::commit).
///
/// A failed precondition returns
/// [`StoreError::TxConflict`](super::StoreError::TxConflict) from `commit`.
/// No staged write applies in that case.
#[async_trait]
pub trait TxCas: Transaction {
    /// Stages a write of `value` that requires `key` to be absent.
    ///
    /// The precondition reads the committed state, not the transaction
    /// overlay. The outcome is known only at
    /// [`commit`](Transaction::commit).
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if a validator rejects the
    /// value. A conflict is not an error at this call. The commit reports it
    /// as [`StoreError::TxConflict`](super::StoreError::TxConflict).
    async fn set_if_absent(&self, key: &str, value: &[u8]) -> Result<()>;

    /// Stages a write of `new` that requires the committed value of `key` to
    /// equal `expected`.
    ///
    /// `expected = None` requires an absent key. The method then acts as a
    /// create-if-absent.
    ///
    /// The precondition reads the committed state, not the transaction
    /// overlay. The outcome is known only at
    /// [`commit`](Transaction::commit).
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if a validator rejects the
    /// value. A conflict is not an error at this call. The commit reports it
    /// as [`StoreError::TxConflict`](super::StoreError::TxConflict).
    async fn compare_and_set(&self, key: &str, expected: Option<&[u8]>, new: &[u8]) -> Result<()>;

    /// Stages a delete of `key` that requires its committed value to equal
    /// `expected`.
    ///
    /// The precondition reads the committed state, not the transaction
    /// overlay. The outcome is known only at
    /// [`commit`](Transaction::commit).
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the storage fails. A
    /// conflict is not an error at this call. The commit reports it as
    /// [`StoreError::TxConflict`](super::StoreError::TxConflict).
    async fn delete_if(&self, key: &str, expected: &[u8]) -> Result<()>;
}

#[cfg(all(test, any(feature = "btree", feature = "oxkv")))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::TxCas;
    use crate::store::{GetSet, Store, StoreError, Transaction};

    #[cfg(feature = "btree")]
    use crate::store::BTreeStore;
    #[cfg(feature = "oxkv")]
    use crate::store::OxKvStore;

    async fn assert_set_if_absent_applies<S>(store: &S)
    where
        S: Store + Sync,
        S::Transaction: TxCas,
    {
        let tx = store.begin_tx().unwrap();
        tx.set_if_absent("claim", b"one").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            store.get_bytes("claim").await.unwrap(),
            Some(b"one".to_vec()),
            "the first claim must apply"
        );

        let tx = store.begin_tx().unwrap();
        tx.set_if_absent("claim", b"two").await.unwrap();
        let error = tx.commit().await.unwrap_err();
        assert_eq!(
            error,
            StoreError::TxConflict {
                key: "claim".to_string(),
                current: Some(b"one".to_vec()),
            },
            "the second claim must conflict"
        );
        assert_eq!(
            store.get_bytes("claim").await.unwrap(),
            Some(b"one".to_vec()),
            "the first claim must stay"
        );
    }

    async fn assert_compare_and_set_applies<S>(store: &S)
    where
        S: Store + Sync,
        S::Transaction: TxCas,
    {
        // `None` means create-if-absent.
        let tx = store.begin_tx().unwrap();
        tx.compare_and_set("k", None, b"one").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(store.get_bytes("k").await.unwrap(), Some(b"one".to_vec()));

        let tx = store.begin_tx().unwrap();
        tx.compare_and_set("k", Some(b"one"), b"two").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(store.get_bytes("k").await.unwrap(), Some(b"two".to_vec()));

        let tx = store.begin_tx().unwrap();
        tx.compare_and_set("k", Some(b"stale"), b"three")
            .await
            .unwrap();
        let error = tx.commit().await.unwrap_err();
        assert_eq!(
            error,
            StoreError::TxConflict {
                key: "k".to_string(),
                current: Some(b"two".to_vec()),
            }
        );
        assert_eq!(
            store.get_bytes("k").await.unwrap(),
            Some(b"two".to_vec()),
            "a conflict must not change the value"
        );
    }

    async fn assert_delete_if_applies<S>(store: &S)
    where
        S: Store + Sync,
        S::Transaction: TxCas,
    {
        store.put_bytes("k", b"v1").await.unwrap();

        let tx = store.begin_tx().unwrap();
        tx.delete_if("k", b"stale").await.unwrap();
        let error = tx.commit().await.unwrap_err();
        assert_eq!(
            error,
            StoreError::TxConflict {
                key: "k".to_string(),
                current: Some(b"v1".to_vec()),
            }
        );
        assert_eq!(
            store.get_bytes("k").await.unwrap(),
            Some(b"v1".to_vec()),
            "a stale delete must not remove the key"
        );

        let tx = store.begin_tx().unwrap();
        tx.delete_if("k", b"v1").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(store.get_bytes("k").await.unwrap(), None);

        store.put_bytes("k", b"v2").await.unwrap();
        let tx = store.begin_tx().unwrap();
        tx.delete_if("missing", b"v2").await.unwrap();
        let error = tx.commit().await.unwrap_err();
        assert_eq!(
            error,
            StoreError::TxConflict {
                key: "missing".to_string(),
                current: None,
            }
        );
    }

    async fn assert_one_conflict_applies_no_write<S>(store: &S)
    where
        S: Store + Sync,
        S::Transaction: TxCas,
    {
        store.put_bytes("a", b"1").await.unwrap();
        store.put_bytes("b", b"2").await.unwrap();

        let tx = store.begin_tx().unwrap();
        tx.compare_and_set("a", Some(b"1"), b"10").await.unwrap();
        tx.compare_and_set("b", Some(b"stale"), b"20")
            .await
            .unwrap();
        tx.set_if_absent("c", b"3").await.unwrap();
        let error = tx.commit().await.unwrap_err();
        assert_eq!(
            error,
            StoreError::TxConflict {
                key: "b".to_string(),
                current: Some(b"2".to_vec()),
            }
        );

        assert_eq!(
            store.get_bytes("a").await.unwrap(),
            Some(b"1".to_vec()),
            "the valid write must not apply"
        );
        assert_eq!(
            store.get_bytes("b").await.unwrap(),
            Some(b"2".to_vec()),
            "the conflicting key must not change"
        );
        assert_eq!(
            store.get_bytes("c").await.unwrap(),
            None,
            "the unrelated write must not apply"
        );
    }

    async fn assert_the_check_reads_committed_state<S>(store: &S)
    where
        S: Store + Sync,
        S::Transaction: TxCas,
    {
        let tx = store.begin_tx().unwrap();
        tx.set_bytes("k", b"staged").await.unwrap();
        // The committed state is still absent. The overlay value does not
        // satisfy the precondition.
        tx.compare_and_set("k", None, b"committed").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            store.get_bytes("k").await.unwrap(),
            Some(b"committed".to_vec())
        );
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_set_if_absent_applies_at_commit() {
        let store = BTreeStore::default();
        assert_set_if_absent_applies(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_compare_and_set_applies_at_commit() {
        let store = BTreeStore::default();
        assert_compare_and_set_applies(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_delete_if_applies_at_commit() {
        let store = BTreeStore::default();
        assert_delete_if_applies(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_one_conflict_applies_no_write() {
        let store = BTreeStore::default();
        assert_one_conflict_applies_no_write(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_the_check_reads_committed_state() {
        let store = BTreeStore::default();
        assert_the_check_reads_committed_state(&store).await;
    }

    #[cfg(feature = "oxkv")]
    async fn new_oxkv() -> OxKvStore {
        OxKvStore::builder()
            .with_store(crate::store::lsm::new_in_memory())
            .skip_probe(true)
            .build()
            .await
            .unwrap()
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_set_if_absent_applies_at_commit() {
        let store = new_oxkv().await;
        assert_set_if_absent_applies(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_compare_and_set_applies_at_commit() {
        let store = new_oxkv().await;
        assert_compare_and_set_applies(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_delete_if_applies_at_commit() {
        let store = new_oxkv().await;
        assert_delete_if_applies(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_one_conflict_applies_no_write() {
        let store = new_oxkv().await;
        assert_one_conflict_applies_no_write(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_the_check_reads_committed_state() {
        let store = new_oxkv().await;
        assert_the_check_reads_committed_state(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn concurrent_tx_claims_yield_one_winner() {
        let store = std::sync::Arc::new(new_oxkv().await);
        let writers = (0..8).map(|i| {
            let store = std::sync::Arc::clone(&store);
            async move {
                let tx = store.begin_tx().unwrap();
                tx.set_if_absent("claim", format!("writer-{i}").as_bytes())
                    .await
                    .unwrap();
                tx.commit().await
            }
        });
        let results = futures::future::join_all(writers).await;

        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "exactly one writer must win"
        );
        assert!(
            results
                .iter()
                .filter(|result| result.is_err())
                .all(|result| {
                    matches!(
                        result,
                        Err(StoreError::TxConflict { key, .. }) if key == "claim"
                    )
                }),
            "every loser must report a conflict"
        );
        assert!(
            store.get_bytes("claim").await.unwrap().is_some(),
            "the winner must leave a value"
        );
    }
}
