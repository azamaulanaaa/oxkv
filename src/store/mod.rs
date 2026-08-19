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

/// A specialized `Result` type for store operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Errors that can occur during store operations.
#[derive(Debug, Error)]
pub enum StoreError {
    /// An error originating from the underlying storage engine (e.g., redb).
    #[error("storage error: {0}")]
    Storage(String),

    /// A generic error with a message.
    #[error("{0}")]
    Other(String),
}

impl PartialEq for StoreError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (StoreError::Storage(a), StoreError::Storage(b))
            | (StoreError::Other(a), StoreError::Other(b)) => a == b,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
