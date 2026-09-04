//! S3-backed LSM store.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use object_store::ObjectStore;
use object_store::path::Path;

use crate::store::{Result, StoreError};

mod probe;

pub(crate) use probe::probe_store;

/// S3-backed store.
#[derive(Debug)]
#[allow(dead_code)]
pub struct S3Store {
    inner: Arc<dyn ObjectStore>,
    prefix: Path,
}

impl S3Store {
    /// Creates a new store builder.
    #[must_use]
    pub fn builder() -> S3StoreBuilder {
        S3StoreBuilder {
            inner: None,
            prefix: Path::default(),
            skip_probe: false,
        }
    }

    /// Runs the storage probe against `store` at `prefix/probe/canary`.
    ///
    /// validates `If-None-Match` / `If-Match` conditional writes.
    /// Returns `Ok(())` only on `ok (create, reject-create, reject-stale)`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if conditional writes are not enforced.
    pub async fn probe(store: Arc<dyn ObjectStore>, prefix: &Path) -> Result<()> {
        probe_store(store, prefix).await
    }

    /// Returns the underlying object store (for tests).
    #[cfg(test)]
    #[must_use]
    pub fn inner_store(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.inner)
    }

    /// Returns the prefix.
    #[cfg(test)]
    #[must_use]
    pub fn prefix(&self) -> &Path {
        &self.prefix
    }
}

/// Builder for [`S3Store`].
#[derive(Default)]
pub struct S3StoreBuilder {
    inner: Option<Arc<dyn ObjectStore>>,
    prefix: Path,
    skip_probe: bool,
}

impl std::fmt::Debug for S3StoreBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3StoreBuilder")
            .field("prefix", &self.prefix)
            .field("skip_probe", &self.skip_probe)
            .field("has_store", &self.inner.is_some())
            .finish_non_exhaustive()
    }
}

impl S3StoreBuilder {
    /// Sets the backing [`ObjectStore`] (use `Arc::new(InMemory::new())` in tests,
    /// `AmazonS3Builder` / `parse_url` in prod).
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn ObjectStore>) -> Self {
        self.inner = Some(store);
        self
    }

    /// Sets the key prefix inside the bucket (e.g. `Path::from("oxkv")`).
    #[must_use]
    pub fn with_prefix(mut self, prefix: Path) -> Self {
        self.prefix = prefix;
        self
    }

    /// Skips the startup storage probe.
    #[must_use]
    pub fn skip_probe(mut self, skip: bool) -> Self {
        self.skip_probe = skip;
        self
    }

    /// Whether the probe will be skipped.
    #[must_use]
    pub fn is_skip_probe(&self) -> bool {
        self.skip_probe
    }

    /// Builds the store, running the probe unless skipped.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails or the store is misconfigured.
    pub async fn build(self) -> Result<S3Store> {
        let store = self.inner.ok_or_else(|| {
            StoreError::Storage("S3Store requires an ObjectStore via with_store()".to_string())
        })?;

        if !self.skip_probe {
            probe_store(Arc::clone(&store), &self.prefix).await?;
        }

        Ok(S3Store {
            inner: store,
            prefix: self.prefix,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    pub(crate) fn new_in_memory() -> Arc<dyn ObjectStore> {
        Arc::new(InMemory::new())
    }

    #[tokio::test]
    async fn probe_ok_on_in_memory() {
        let store = new_in_memory();
        S3Store::probe(Arc::clone(&store), &Path::default())
            .await
            .expect("probe must pass on InMemory");
    }

    #[tokio::test]
    async fn builder_runs_probe_by_default() {
        let store = new_in_memory();
        let s = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv"))
            .build()
            .await
            .expect("builder with InMemory must pass probe");
        assert_eq!(s.prefix().as_ref(), "oxkv");
    }

    #[tokio::test]
    async fn builder_skip_probe_flag() {
        assert!(!S3Store::builder().is_skip_probe());
        assert!(S3Store::builder().skip_probe(true).is_skip_probe());
    }
}
