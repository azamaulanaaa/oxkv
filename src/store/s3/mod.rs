//! S3-backed LSM store.
#![cfg(not(target_arch = "wasm32"))]
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]

use std::sync::Arc;

use object_store::ObjectStore;
use object_store::path::Path;

use crate::store::{Result, StoreError};

mod ownership;
mod probe;

pub(crate) use ownership::acquire_ownership;
#[cfg(test)]
pub(crate) use ownership::{epoch_prefix, ownership_path, wal_path};
pub(crate) use probe::probe_store;

/// S3-backed store.
#[derive(Debug)]
#[allow(dead_code)]
pub struct S3Store {
    inner: Arc<dyn ObjectStore>,
    prefix: Path,
    epoch: u64,
    session: String,
}

impl S3Store {
    /// Creates a new store builder.
    #[must_use]
    pub fn builder() -> S3StoreBuilder {
        S3StoreBuilder {
            inner: None,
            prefix: Path::default(),
            skip_probe: false,
            session: None,
        }
    }

    /// Runs the storage probe against `store` at `prefix/probe/canary`.
    pub async fn probe(store: Arc<dyn ObjectStore>, prefix: &Path) -> Result<()> {
        probe_store(store, prefix).await
    }

    #[cfg(test)]
    #[must_use]
    pub fn inner_store(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.inner)
    }

    #[cfg(test)]
    #[must_use]
    pub fn prefix(&self) -> &Path {
        &self.prefix
    }

    #[cfg(test)]
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    #[cfg(test)]
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }
}

/// Builder for [`S3Store`].
#[derive(Default)]
pub struct S3StoreBuilder {
    inner: Option<Arc<dyn ObjectStore>>,
    prefix: Path,
    skip_probe: bool,
    session: Option<String>,
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
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn ObjectStore>) -> Self {
        self.inner = Some(store);
        self
    }

    #[must_use]
    pub fn with_prefix(mut self, prefix: Path) -> Self {
        self.prefix = prefix;
        self
    }

    #[must_use]
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    #[must_use]
    pub fn skip_probe(mut self, skip: bool) -> Self {
        self.skip_probe = skip;
        self
    }

    #[must_use]
    pub fn is_skip_probe(&self) -> bool {
        self.skip_probe
    }

    /// Builds the store, running the probe unless skipped, then CAS-acquires ownership.
    pub async fn build(self) -> Result<S3Store> {
        let store = self.inner.ok_or_else(|| {
            StoreError::Storage("S3Store requires an ObjectStore via with_store()".to_string())
        })?;

        if !self.skip_probe {
            probe_store(Arc::clone(&store), &self.prefix).await?;
        }

        let session = self.session.unwrap_or_else(|| {
            format!(
                "sess-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            )
        });
        let rec = acquire_ownership(Arc::clone(&store), &self.prefix, &session).await?;

        Ok(S3Store {
            inner: store,
            prefix: self.prefix,
            epoch: rec.epoch,
            session,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;
    use object_store::path::Path;

    pub(crate) fn new_in_memory() -> Arc<dyn ObjectStore> {
        Arc::new(InMemory::new())
    }

    #[tokio::test]
    async fn probe_ok_on_in_memory() {
        let store = new_in_memory();
        S3Store::probe(Arc::clone(&store), &Path::default())
            .await
            .expect("probe must pass");
    }

    #[test]
    fn ownership_path_no_prefix() {
        assert_eq!(ownership_path(&Path::default()).as_ref(), "ownership.json");
    }

    #[test]
    fn manifest_path_and_epoch_prefix_formatting() {
        assert_eq!(epoch_prefix(&Path::default(), 7).as_ref(), "e000007");
        assert_eq!(
            wal_path(&Path::from("oxkv"), 7, 42).as_ref(),
            "oxkv/e000007/wal/00000042.log"
        );
    }

    #[tokio::test]
    async fn fencing_acquire_increments_epoch() {
        let store = new_in_memory();
        let prefix = Path::from("oxkv");
        let r1 = acquire_ownership(Arc::clone(&store), &prefix, "node-a")
            .await
            .unwrap();
        assert_eq!(r1.epoch, 1);
        let r2 = acquire_ownership(Arc::clone(&store), &prefix, "node-b")
            .await
            .unwrap();
        assert_eq!(r2.epoch, 2);
    }
}
