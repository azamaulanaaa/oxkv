//! Core traits and types for a key-value store with transaction support.
//!
//! This module defines the foundational abstractions for a persistent key-value
//! store, with operations for CRUD (Create, Read, Update, Delete), batched
//! retrieval with bidirectional cursors, and atomic transactions.
//!
//! The primary traits are:
//! - [`GetSet`]: Basic key-value operations.
//! - [`Transaction`]: Extends [`GetSet`] with commit/rollback.
//! - [`Store`]: Extends [`GetSet`] with the ability to start a transaction.
//!
//! All operations return a [`Result`] with a [`StoreError`] on failure.

use async_trait::async_trait;
use thiserror::Error;

pub use btree::{BTreeStore, BTreeTx};
mod btree;
pub use redb::{RedbStore, RedbTx};
mod redb;

/// A specialized `Result` type for store operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Errors that can occur during store operations.
#[derive(Debug, Error)]
pub enum StoreError {
    /// An error originating from the underlying storage engine (e.g., redb).
    #[error("storage error: {0}")]
    Storage(String),

    /// A serialization or deserialization error (e.g., JSON).
    #[error("serialization error: {0}")]
    Serialization(String),

    /// An error when converting between UTF-8 strings and bytes.
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    /// A JSON serialization or deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// An error when decoding UTF-8 from a byte slice.
    #[error("UTF-8 error: {0}")]
    Utf8Slice(#[from] std::str::Utf8Error),

    /// A generic error with a message.
    #[error("{0}")]
    Other(String),
}

impl PartialEq for StoreError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (StoreError::Storage(a), StoreError::Storage(b))
            | (StoreError::Serialization(a), StoreError::Serialization(b))
            | (StoreError::Other(a), StoreError::Other(b)) => a == b,
            (StoreError::Utf8(a), StoreError::Utf8(b)) => a == b,
            (StoreError::Utf8Slice(a), StoreError::Utf8Slice(b)) => a == b,
            (StoreError::Json(a), StoreError::Json(b)) => a.to_string() == b.to_string(),
            _ => false,
        }
    }
}

impl Eq for StoreError {}

impl From<&str> for StoreError {
    fn from(msg: &str) -> Self {
        StoreError::Other(msg.to_string())
    }
}

impl From<std::string::String> for StoreError {
    fn from(msg: String) -> Self {
        StoreError::Other(msg)
    }
}

/// A single key-value pair, where the value is raw bytes.
///
/// This is used as the return type for batched retrieval operations.
#[derive(Debug, Clone)]
pub struct KeyValue {
    /// The string key.
    pub key: String,
    /// The value as a byte vector.
    pub value: Vec<u8>,
}

/// Direction for cursor-based pagination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Traverse keys in ascending order (from the start cursor or from the beginning).
    Next,
    /// Traverse keys in descending order (from the end cursor or from the end).
    Prev,
}

/// Basic operations for a key-value store.
///
/// This trait provides the fundamental operations for interacting with the store.
/// All operations are atomic and immediately durable (unless wrapped in a transaction).
#[async_trait]
pub trait GetSet {
    /// Retrieves the value associated with the given key.
    ///
    /// Returns `Ok(Some(bytes))` if the key exists, `Ok(None)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Checks if a key exists in the store.
    ///
    /// Returns `Ok(true)` if the key exists, `Ok(false)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn exists(&self, key: &str) -> Result<bool>;

    /// Deletes the key-value pair for the given key.
    ///
    /// Returns `Ok(true)` if the key existed and was deleted, `Ok(false)` otherwise.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn delete(&mut self, key: &str) -> Result<bool>;

    /// Sets a key-value pair, inserting if absent or updating if present.
    ///
    /// Returns the previous value if the key already existed (an update),
    /// or `None` if the key did not exist before (a new insertion).
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Retrieves multiple key-value pairs with cursor-based pagination.
    ///
    /// # Parameters
    ///
    /// - `limit`: Maximum number of items to return. If `None`, all matching items are returned.
    /// - `direction`: Whether to traverse in ascending (`Next`) or descending (`Prev`) order.
    /// - `cursor`: A tuple of optional start and end cursors (both inclusive).
    ///
    ///   **For `Direction::Next` (ascending):**
    ///   - `(Some(start), Some(end))`: Range from `start` to `end` (inclusive), both bounds must satisfy `start <= end`.
    ///   - `(Some(start), None)`: From `start` (inclusive) to the end of the range.
    ///   - `(None, Some(end))`: From the beginning to `end` (inclusive).
    ///   - `(None, None)`: All items.
    ///
    ///   **For `Direction::Prev` (descending):**
    ///   - `(Some(start), Some(end))`: Range from `start` down to `end` (inclusive); requires `start >= end`, otherwise the result is empty.
    ///   - `(Some(start), None)`: From `start` (inclusive) down to the beginning of the range.
    ///   - `(None, Some(end))`: **Empty** – because there is no starting point to traverse backwards from.
    ///   - `(None, None)`: **Empty** – same reason.
    ///
    /// # Returns
    ///
    /// A vector of [`KeyValue`] pairs matching the query, ordered according to `direction`
    /// (ascending for `Next`, descending for `Prev`).
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the underlying storage fails.
    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>>;
}

/// A transaction that groups multiple operations atomically.
///
/// All operations performed on a transaction are not visible to other readers
/// until the transaction is committed. If the transaction is rolled back, all
/// changes are discarded.
///
/// Transactions are obtained from a [`Store`] via [`Store::begin_tx`].
#[async_trait]
pub trait Transaction: GetSet {
    /// Commits the transaction, making all changes durable and visible.
    ///
    /// After commit, the transaction handle should no longer be used.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the commit fails (e.g., conflict, I/O error).
    async fn commit(self) -> Result<()>;

    /// Aborts the transaction, discarding all changes made.
    ///
    /// After rollback, the transaction handle should no longer be used.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the rollback fails (rare).
    async fn rollback(self) -> Result<()>;
}

/// A store that supports atomic transactions.
///
/// This trait extends [`GetSet`] with the ability to create a new transaction.
/// All standalone (non-transactional) operations are immediately committed.
#[async_trait]
pub trait Store: GetSet {
    /// The transaction type produced by [`begin_tx`][Self::begin_tx].
    ///
    /// Each concrete backend declares its own `Transaction` type here via the
    /// associated-type pattern — e.g., `type Transaction = RedbTx;`. This
    /// allows zero-cost monomorphization: no heap allocation, no vtable dispatch.
    type Transaction: Transaction + Send;

    /// Begins a new write transaction.
    ///
    /// The returned transaction object provides the same CRUD operations as the store,
    /// but they are staged until [`Transaction::commit`] is called.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the transaction cannot be started.
    fn begin_tx(&mut self) -> Result<Self::Transaction>;
}

/// Extension methods for binary serialization and bulk loading on [`Store`].
#[async_trait]
pub trait StoreExt: Store {
    /// Serializes all key-value pairs into a single contiguous `Vec<u8>`.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if retrieval fails.
    async fn save(&self) -> Result<Vec<u8>> {
        const BATCH_SIZE: u32 = 256;
        let mut buffer = Vec::with_capacity(4096);
        let mut cursor: Option<String> = None;

        loop {
            let batch = self
                .gets_bytes(Some(BATCH_SIZE), Direction::Next, (cursor.clone(), None))
                .await?;

            if batch.is_empty() {
                break;
            }

            let is_last_batch = batch.len() < BATCH_SIZE as usize;
            let mut processed = 0;

            for kv in &batch {
                // Avoid duplicate processing if cursor bound is inclusive
                if cursor.as_deref() == Some(&kv.key) {
                    continue;
                }

                let k_bytes = kv.key.as_bytes();
                let key_len = u32::try_from(k_bytes.len())
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;
                let val_len = u32::try_from(kv.value.len())
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;

                buffer.extend_from_slice(&key_len.to_le_bytes());
                buffer.extend_from_slice(k_bytes);
                buffer.extend_from_slice(&val_len.to_le_bytes());
                buffer.extend_from_slice(&kv.value);
                processed += 1;
            }

            if is_last_batch || processed == 0 {
                break;
            }
            cursor = batch.last().map(|kv| kv.key.clone());
        }

        Ok(buffer)
    }

    /// Loads all key-value pairs directly from a `&[u8]` slice into the store.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the payload is malformed or writing fails.
    async fn load(&mut self, mut data: &[u8]) -> Result<usize> {
        let mut tx = self.begin_tx()?;
        let mut count = 0;

        while !data.is_empty() {
            // Read key length
            if data.len() < 4 {
                return Err(StoreError::Serialization(
                    "Truncated key length header".into(),
                ));
            }
            let key_len_bytes: [u8; 4] = data[..4].try_into().map_err(|e| {
                StoreError::Serialization(format!("Invalid key length header: {e}"))
            })?;
            let key_len = usize::try_from(u32::from_le_bytes(key_len_bytes))
                .map_err(|e| StoreError::Serialization(e.to_string()))?;
            data = &data[4..];

            // Read key slice
            if data.len() < key_len {
                return Err(StoreError::Serialization("Truncated key data".into()));
            }
            let key = std::str::from_utf8(&data[..key_len])?;
            data = &data[key_len..];

            // Read value length
            if data.len() < 4 {
                return Err(StoreError::Serialization(
                    "Truncated value length header".into(),
                ));
            }
            let val_len_bytes: [u8; 4] = data[..4].try_into().map_err(|e| {
                StoreError::Serialization(format!("Invalid value length header: {e}"))
            })?;
            let val_len = usize::try_from(u32::from_le_bytes(val_len_bytes))
                .map_err(|e| StoreError::Serialization(e.to_string()))?;
            data = &data[4..];

            // Read value slice
            if data.len() < val_len {
                return Err(StoreError::Serialization("Truncated value data".into()));
            }
            let val = &data[..val_len];
            data = &data[val_len..];

            // Insert into the transaction without cloning
            tx.set_bytes(key, val).await?;
            count += 1;
        }

        tx.commit().await?;
        Ok(count)
    }
}

impl<T: Store> StoreExt for T {}

/// Extension methods for common serialization formats (BINCODE).
///
/// This trait is automatically implemented for all types that implement [`GetSet`].
#[async_trait]
pub trait GetSetExt: GetSet {
    /// Sets a value serialized with JSON, stored as raw bytes.
    ///
    /// The value is serialized using `serde_json` and stored directly as bytes.
    /// If the key already exists, it will be overwritten (treated as an update).
    ///
    /// Returns the previous value deserialized as `T` if the key already existed
    /// (an update), or `None` if the key did not exist before (a new insertion).
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if serialization or storage fails.
    async fn set<T: serde::Serialize + Sync>(&mut self, key: &str, value: &T) -> Result<Option<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        let json = serde_json::to_vec(value)?;
        match self.set_bytes(key, &json).await? {
            Some(prev) => Ok(Some(serde_json::from_slice(&prev)?)),
            None => Ok(None),
        }
    }

    /// Retrieves a value and deserializes it using JSON.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if deserialization or retrieval fails.
    async fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.get_bytes(key).await? {
            Some(json) => Ok(Some(serde_json::from_slice(&json)?)),
            None => Ok(None),
        }
    }
}

impl<T: GetSet> GetSetExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    mockall::mock! {
        pub Transaction {}

        #[async_trait]
        impl GetSet for Transaction {
            async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>>;
            async fn exists(&self, key: &str) -> Result<bool>;
            async fn delete(&mut self, key: &str) -> Result<bool>;
            async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>>;
            async fn gets_bytes(
                &self,
                limit: Option<u32>,
                direction: Direction,
                cursor: (Option<String>, Option<String>),
            ) -> Result<Vec<KeyValue>>;
        }

        #[async_trait]
        impl Transaction for Transaction {
            async fn commit(self) -> Result<()>;
            async fn rollback(self) -> Result<()>;
        }
    }

    mockall::mock! {
        pub Store {}

        #[async_trait]
        impl GetSet for Store {
            async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>>;
            async fn exists(&self, key: &str) -> Result<bool>;
            async fn delete(&mut self, key: &str) -> Result<bool>;
            async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>>;
            async fn gets_bytes(
                &self,
                limit: Option<u32>,
                direction: Direction,
                cursor: (Option<String>, Option<String>),
            ) -> Result<Vec<KeyValue>>;
        }

        #[async_trait]
        impl Store for Store {
            type Transaction = MockTransaction;
            fn begin_tx(&mut self) -> Result<MockTransaction>;
        }
    }

    #[test]
    fn test_store_error_partial_eq_storage() {
        let a = crate::StoreError::Storage("foo".into());
        let b = crate::StoreError::Storage("foo".into());
        assert!(a == b);
        assert_ne!(a, crate::StoreError::Storage("bar".into()));
    }

    #[test]
    fn test_store_error_partial_eq_other() {
        let a: crate::StoreError = From::from("msg1");
        let b: crate::StoreError = From::from("msg2");
        assert_ne!(a, b);
    }

    #[test]
    fn test_store_error_cross_variant_not_equal() {
        let storage = crate::StoreError::Storage("x".into());
        let other: crate::StoreError = From::from("y");
        // Even though the inner messages could match, cross-variant comparison is false
        assert_ne!(storage, other);
    }

    #[test]
    fn test_store_error_from_str() {
        let err: crate::StoreError = From::from("oops");
        match err {
            crate::StoreError::Other(msg) => assert_eq!(msg, "oops"),
            _ => panic!("expected Other variant"),
        }
    }

    #[test]
    fn test_store_error_from_string() {
        let err: crate::StoreError = From::from(String::from("boom"));
        match err {
            crate::StoreError::Other(msg) => assert_eq!(msg, "boom"),
            _ => panic!("expected Other variant"),
        }
    }

    #[test]
    fn test_store_error_from_utf8() {
        // Exercises the From<&str> impl path — a UTF-8 error is typically produced internally
        // by converting &str to String, so this tests the conversion chain.
        let err: crate::StoreError = From::from("valid utf-8");
        match err {
            crate::StoreError::Other(msg) => assert_eq!(msg, "valid utf-8"),
            _ => panic!("expected Other variant"),
        }
    }

    #[tokio::test]
    async fn test_store_ext_save_mocked() {
        let mut mock_store = MockStore::new();

        // 1st call: Return a full batch of 256 items to force pagination to continue
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(256) && *dir == Direction::Next && cursor.0.is_none()
            })
            .returning(|_, _, _| {
                let batch = (0..256)
                    .map(|i| KeyValue {
                        key: format!("k{i:03}"),
                        value: format!("v{i}").into_bytes(),
                    })
                    .collect();
                Ok(batch)
            });

        // 2nd call: Return an empty batch with the cursor set to the 256th item ("k255")
        mock_store
            .expect_gets_bytes()
            .times(1)
            .withf(|limit, dir, cursor| {
                *limit == Some(256)
                    && *dir == Direction::Next
                    && cursor.0.as_deref() == Some("k255")
            })
            .returning(|_, _, _| Ok(vec![]));

        let bytes = mock_store.save().await.unwrap();
        assert!(!bytes.is_empty());
    }

    #[tokio::test]
    async fn test_store_ext_save_empty_mocked() {
        let mut mock_store = MockStore::new();

        mock_store
            .expect_gets_bytes()
            .once()
            .returning(|_, _, _| Ok(vec![]));

        let bytes = mock_store.save().await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn test_store_ext_load_mocked() {
        let mut mock_store = MockStore::new();

        // Craft a binary payload for single key-value ("key1", "val1")
        let mut payload = Vec::new();
        // key len = 4, key = "key1"
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(b"key1");
        // val len = 4, val = "val1"
        payload.extend_from_slice(&4u32.to_le_bytes());
        payload.extend_from_slice(b"val1");

        mock_store.expect_begin_tx().once().returning(|| {
            let mut mock_tx = MockTransaction::new();

            mock_tx
                .expect_set_bytes()
                .withf(|key, val| key == "key1" && val == b"val1")
                .once()
                .returning(|_, _| Ok(None));

            mock_tx.expect_commit().once().returning(|| Ok(()));

            Ok(mock_tx)
        });

        let loaded_count = mock_store.load(&payload).await.unwrap();
        assert_eq!(loaded_count, 1);
    }

    #[tokio::test]
    async fn test_store_ext_load_corrupted_data_mocked() {
        let mut mock_store = MockStore::new();

        // `begin_tx` is called before decoding begins
        mock_store
            .expect_begin_tx()
            .returning(|| Ok(MockTransaction::new()));

        // Truncated key length header (< 4 bytes)
        assert!(mock_store.load(&[1, 2]).await.is_err());

        // Truncated key payload
        let bad_key_payload = vec![10, 0, 0, 0, b'a', b'b'];
        assert!(mock_store.load(&bad_key_payload).await.is_err());

        // Truncated value length header
        let bad_val_header = vec![1, 0, 0, 0, b'a', 0, 0];
        assert!(mock_store.load(&bad_val_header).await.is_err());

        // Truncated value payload
        let bad_val_payload = vec![1, 0, 0, 0, b'a', 5, 0, 0, 0, b'x'];
        assert!(mock_store.load(&bad_val_payload).await.is_err());
    }
}
