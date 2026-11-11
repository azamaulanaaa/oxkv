//! Atomic single-key conditional writes. Applications also call these
//! compare-and-swap (CAS) writes.
//!
//! A conditional write reads the key and writes the key under one lock. Two
//! writers therefore cannot both satisfy the same precondition. Use these
//! methods for unique claims, counters, leases, and idempotency keys.
//!
//! # Guarantees
//!
//! - Each operation is atomic for one key.
//! - A failed precondition changes nothing. The operation does not write,
//!   does not touch the `MemTable`, and does not notify subscribers.
//! - A successful write runs the registered validators first. It notifies
//!   subscribers after the write completes. The event matches the shape of
//!   [`GetSet::set_bytes`](super::GetSet::set_bytes) and
//!   [`GetSet::delete`](super::GetSet::delete).
//!
//! # Limits
//!
//! - There is no atomicity across keys. Use a transaction for a rule that
//!   spans keys.
//! - There is no conflict detection against a separate transaction. A
//!   transaction that read the key earlier does not fail when a CAS changes
//!   the key. The commit re-runs validators only.
//! - The single-writer epoch fence still applies. A fenced LSM store returns
//!   [`StoreError::Fenced`](super::StoreError::Fenced) and does not write.
//! - The transaction types do not implement [`Cas`]. A transaction stages
//!   writes without a lock on the key. A CAS method there would not be
//!   atomic.

use async_trait::async_trait;

use super::Result;
#[cfg(any(feature = "btree", feature = "oxkv"))]
use super::{ChangeEvent, ChangeKind};

/// The result of [`Cas::compare_and_set`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasResult {
    /// The precondition matched and the write completed.
    ///
    /// The LSM backend makes the write durable under the WAL `RPO=0`
    /// contract. The B-tree backend is in memory only.
    Applied,
    /// The precondition did not match. The store did not change.
    Conflict {
        /// The current value of the key at the moment of the check.
        ///
        /// The value is `None` when the key is absent.
        current: Option<Vec<u8>>,
    },
}

/// Atomic conditional writes on a single key.
///
/// All methods are async. Each method keeps the read and the write of one key
/// under one lock. A successful write completes before the method returns.
///
/// # Durability
///
/// The LSM backend makes a successful write durable under the WAL `RPO=0`
/// contract. The B-tree backend is in memory only.
#[async_trait]
pub trait Cas: Send + Sync {
    /// Writes `value` only when `key` is absent.
    ///
    /// Returns `Ok(true)` when the write applies. Returns `Ok(false)` when the
    /// key exists. In that case the old value stays unchanged.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if a validator rejects the
    /// value or if the storage fails.
    async fn set_if_absent(&self, key: &str, value: &[u8]) -> Result<bool>;

    /// Writes `new` only when the current value of `key` equals `expected`.
    ///
    /// `expected = None` requires an absent key. The method then acts as a
    /// create-if-absent.
    ///
    /// Returns [`CasResult::Applied`] when the write applies. Returns
    /// [`CasResult::Conflict`] with the current value otherwise. A conflict
    /// changes nothing.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if a validator rejects the
    /// value or if the storage fails.
    async fn compare_and_set(
        &self,
        key: &str,
        expected: Option<&[u8]>,
        new: &[u8],
    ) -> Result<CasResult>;

    /// Deletes `key` only when its current value equals `expected`.
    ///
    /// Returns `Ok(true)` when the delete applies. Returns `Ok(false)` when
    /// the key is absent or the value differs. In that case the value stays
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`](super::StoreError) if the storage fails.
    async fn delete_if(&self, key: &str, expected: &[u8]) -> Result<bool>;
}

#[cfg(feature = "btree")]
#[async_trait]
impl Cas for super::BTreeStore {
    async fn set_if_absent(&self, key: &str, value: &[u8]) -> Result<bool> {
        let validators = self.hooks.validators();
        crate::store::hooks::validate_key(&validators, self, key, value).await?;

        // The check and the insert share one write lock. A read lock would
        // release before the insert and allow a lost update.
        let applied = {
            let mut map = super::rwlock_ignore_poison(&self.map);
            if map.contains_key(key) {
                false
            } else {
                map.insert(key.to_string(), value.to_vec());
                true
            }
        };

        if applied {
            crate::store::hooks::notify(
                &self.hooks.subscribers(),
                self,
                ChangeEvent {
                    key: key.to_string(),
                    kind: ChangeKind::Set,
                    old_value: None,
                    new_value: Some(value.to_vec()),
                    seq: 0,
                },
            )
            .await;
        }
        Ok(applied)
    }

    async fn compare_and_set(
        &self,
        key: &str,
        expected: Option<&[u8]>,
        new: &[u8],
    ) -> Result<CasResult> {
        let validators = self.hooks.validators();
        crate::store::hooks::validate_key(&validators, self, key, new).await?;

        // The check and the write share one write lock.
        let current = {
            let mut map = super::rwlock_ignore_poison(&self.map);
            let current = map.get(key).cloned();
            if current.as_deref() == expected {
                map.insert(key.to_string(), new.to_vec());
            }
            current
        };

        if current.as_deref() != expected {
            return Ok(CasResult::Conflict { current });
        }

        crate::store::hooks::notify(
            &self.hooks.subscribers(),
            self,
            ChangeEvent {
                key: key.to_string(),
                kind: ChangeKind::Set,
                old_value: current,
                new_value: Some(new.to_vec()),
                seq: 0,
            },
        )
        .await;
        Ok(CasResult::Applied)
    }

    async fn delete_if(&self, key: &str, expected: &[u8]) -> Result<bool> {
        // The check and the delete share one write lock.
        let current = {
            let mut map = super::rwlock_ignore_poison(&self.map);
            let current = map.get(key).cloned();
            if current.as_deref() == Some(expected) {
                map.remove(key);
            }
            current
        };

        if current.as_deref() != Some(expected) {
            return Ok(false);
        }

        crate::store::hooks::notify(
            &self.hooks.subscribers(),
            self,
            ChangeEvent {
                key: key.to_string(),
                kind: ChangeKind::Delete,
                old_value: current,
                new_value: None,
                seq: 0,
            },
        )
        .await;
        Ok(true)
    }
}

#[cfg(feature = "oxkv")]
#[async_trait]
impl<C> Cas for super::OxKvStore<C>
where
    C: super::Cache<String, std::sync::Arc<super::SstFile>>,
{
    async fn set_if_absent(&self, key: &str, value: &[u8]) -> Result<bool> {
        let validators = self.hooks.validators();
        crate::store::hooks::validate_key(&validators, self, key, value).await?;

        // The gate makes the read and the append one step. `put_bytes` would
        // take the gate again and deadlock.
        let gate = self.write_gate.lock().await;
        // Anything staged earlier belongs ahead of this record in the WAL.
        self.drain_staged_locked().await?;
        if super::OxKvStore::<C>::get_bytes(self, key).await?.is_some() {
            return Ok(false);
        }
        let update = (key.to_string(), Some(value.to_vec()));
        let payload = super::OxKvStore::<C>::encode_ops(std::slice::from_ref(&update))?;
        self.append_wal_locked(payload, std::slice::from_ref(&update))
            .await?;
        drop(gate);

        crate::store::hooks::notify(
            &self.hooks.subscribers(),
            self,
            ChangeEvent {
                key: key.to_string(),
                kind: ChangeKind::Set,
                old_value: None,
                new_value: Some(value.to_vec()),
                seq: 0,
            },
        )
        .await;
        Ok(true)
    }

    async fn compare_and_set(
        &self,
        key: &str,
        expected: Option<&[u8]>,
        new: &[u8],
    ) -> Result<CasResult> {
        let validators = self.hooks.validators();
        crate::store::hooks::validate_key(&validators, self, key, new).await?;

        let gate = self.write_gate.lock().await;
        self.drain_staged_locked().await?;
        let current = super::OxKvStore::<C>::get_bytes(self, key).await?;
        if current.as_deref() != expected {
            drop(gate);
            return Ok(CasResult::Conflict { current });
        }
        let update = (key.to_string(), Some(new.to_vec()));
        let payload = super::OxKvStore::<C>::encode_ops(std::slice::from_ref(&update))?;
        self.append_wal_locked(payload, std::slice::from_ref(&update))
            .await?;
        drop(gate);

        crate::store::hooks::notify(
            &self.hooks.subscribers(),
            self,
            ChangeEvent {
                key: key.to_string(),
                kind: ChangeKind::Set,
                old_value: current,
                new_value: Some(new.to_vec()),
                seq: 0,
            },
        )
        .await;
        Ok(CasResult::Applied)
    }

    async fn delete_if(&self, key: &str, expected: &[u8]) -> Result<bool> {
        let gate = self.write_gate.lock().await;
        self.drain_staged_locked().await?;
        let current = super::OxKvStore::<C>::get_bytes(self, key).await?;
        if current.as_deref() != Some(expected) {
            drop(gate);
            return Ok(false);
        }
        let update = (key.to_string(), None);
        let payload = super::OxKvStore::<C>::encode_ops(std::slice::from_ref(&update))?;
        self.append_wal_locked(payload, std::slice::from_ref(&update))
            .await?;
        drop(gate);

        crate::store::hooks::notify(
            &self.hooks.subscribers(),
            self,
            ChangeEvent {
                key: key.to_string(),
                kind: ChangeKind::Delete,
                old_value: current,
                new_value: None,
                seq: 0,
            },
        )
        .await;
        Ok(true)
    }
}

#[cfg(all(test, any(feature = "btree", feature = "oxkv")))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{Cas, CasResult};

    #[cfg(feature = "btree")]
    use crate::store::BTreeStore;
    #[cfg(feature = "oxkv")]
    use crate::store::OxKvStore;
    use crate::store::{
        ChangeKind, GetSet, Hooks, Result, Scope, StoreError, StoreView, Validator,
    };

    /// One write, one winner, one value. A second call must not change it.
    async fn assert_set_if_absent<T>(store: &T)
    where
        T: Cas + GetSet,
    {
        assert!(
            store.set_if_absent("claim", b"one").await.unwrap(),
            "the first call must apply"
        );
        assert!(
            !store.set_if_absent("claim", b"two").await.unwrap(),
            "the second call must fail"
        );
        assert_eq!(
            store.get_bytes("claim").await.unwrap(),
            Some(b"one".to_vec()),
            "the first value must stay"
        );
    }

    async fn assert_compare_and_set<T>(store: &T)
    where
        T: Cas + GetSet + Hooks,
    {
        let mut rx = store.watch("k");

        // `None` means create-if-absent.
        assert_eq!(
            store.compare_and_set("k", None, b"one").await.unwrap(),
            CasResult::Applied
        );
        assert_eq!(rx.try_recv().unwrap().kind, ChangeKind::Set);

        assert_eq!(
            store
                .compare_and_set("k", Some(b"one"), b"two")
                .await
                .unwrap(),
            CasResult::Applied
        );
        let event = rx.try_recv().unwrap();
        assert_eq!(event.old_value, Some(b"one".to_vec()));
        assert_eq!(event.new_value, Some(b"two".to_vec()));

        let stale = store
            .compare_and_set("k", Some(b"stale"), b"three")
            .await
            .unwrap();
        assert_eq!(
            stale,
            CasResult::Conflict {
                current: Some(b"two".to_vec())
            }
        );
        assert_eq!(store.get_bytes("k").await.unwrap(), Some(b"two".to_vec()));
        assert!(
            rx.try_recv().unwrap_err().is_empty(),
            "a conflict must not notify"
        );

        // An absent key plus `None` also conflicts when the key exists.
        assert_eq!(
            store.compare_and_set("k", None, b"four").await.unwrap(),
            CasResult::Conflict {
                current: Some(b"two".to_vec())
            }
        );
    }

    async fn assert_delete_if<T>(store: &T)
    where
        T: Cas + GetSet + Hooks,
    {
        let mut rx = store.watch("k");
        store.set_bytes("k", b"v1").await.unwrap();
        let _ = rx.try_recv().unwrap();

        assert!(
            !store.delete_if("k", b"stale").await.unwrap(),
            "a different value must not delete"
        );
        assert_eq!(store.get_bytes("k").await.unwrap(), Some(b"v1".to_vec()));
        assert!(
            rx.try_recv().unwrap_err().is_empty(),
            "a failed delete must not notify"
        );

        assert!(
            store.delete_if("k", b"v1").await.unwrap(),
            "an exact match must delete"
        );
        assert_eq!(store.get_bytes("k").await.unwrap(), None);
        let event = rx.try_recv().unwrap();
        assert_eq!(event.kind, ChangeKind::Delete);
        assert_eq!(event.old_value, Some(b"v1".to_vec()));
        assert_eq!(event.new_value, None);

        assert!(
            !store.delete_if("k", b"v1").await.unwrap(),
            "an absent key must not delete"
        );
    }

    struct RejectAll;

    #[async_trait::async_trait]
    impl Validator for RejectAll {
        fn scope(&self) -> Scope {
            Scope::All
        }

        async fn validate(&self, _ctx: &dyn StoreView, key: &str, _value: &[u8]) -> Result<()> {
            Err(StoreError::Rejected {
                key: key.to_string(),
                reason: "rejected by the test validator".to_string(),
            })
        }
    }

    async fn assert_a_validator_rejection_keeps_the_value<T>(store: &T)
    where
        T: Cas + GetSet + Hooks,
    {
        store.set_bytes("k", b"old").await.unwrap();
        store.add_validator(RejectAll);

        assert!(
            store
                .compare_and_set("k", Some(b"old"), b"new")
                .await
                .is_err()
        );
        assert_eq!(store.get_bytes("k").await.unwrap(), Some(b"old".to_vec()));

        assert!(store.set_if_absent("fresh", b"new").await.is_err());
        assert_eq!(store.get_bytes("fresh").await.unwrap(), None);
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_set_if_absent_applies_once() {
        let store = BTreeStore::default();
        assert_set_if_absent(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_compare_and_set_checks_the_expected_value() {
        let store = BTreeStore::default();
        assert_compare_and_set(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_delete_if_checks_the_expected_value() {
        let store = BTreeStore::default();
        assert_delete_if(&store).await;
    }

    #[cfg(feature = "btree")]
    #[tokio::test]
    async fn btree_a_validator_rejection_keeps_the_value() {
        let store = BTreeStore::default();
        assert_a_validator_rejection_keeps_the_value(&store).await;
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
    async fn oxkv_set_if_absent_applies_once() {
        let store = new_oxkv().await;
        assert_set_if_absent(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_compare_and_set_checks_the_expected_value() {
        let store = new_oxkv().await;
        assert_compare_and_set(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_delete_if_checks_the_expected_value() {
        let store = new_oxkv().await;
        assert_delete_if(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn oxkv_a_validator_rejection_keeps_the_value() {
        let store = new_oxkv().await;
        assert_a_validator_rejection_keeps_the_value(&store).await;
    }

    #[cfg(feature = "oxkv")]
    #[tokio::test]
    async fn concurrent_set_if_absent_yields_one_winner() {
        let store = std::sync::Arc::new(new_oxkv().await);
        let writers = (0..8).map(|i| {
            let store = std::sync::Arc::clone(&store);
            async move {
                store
                    .set_if_absent("claim", format!("writer-{i}").as_bytes())
                    .await
            }
        });
        let results = futures::future::join_all(writers).await;

        assert!(
            results.iter().all(std::result::Result::is_ok),
            "no call may fail"
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Ok(true)))
                .count(),
            1,
            "exactly one writer must win"
        );
        assert!(
            store.get_bytes("claim").await.unwrap().is_some(),
            "the winner must leave a value"
        );
    }
}
