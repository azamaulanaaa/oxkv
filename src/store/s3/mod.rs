//! S3-backed LSM store — probe + epoch fencing scaffold.
//!
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use object_store::path::Path;
use object_store::{ObjectStore, PutMode, PutPayload, PutResult, UpdateVersion};

use crate::store::{Result, StoreError};

/// S3-backed store (incremental — probe ships first).
#[derive(Debug)]
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
    /// Mirrors `celld diagnose` — validates `If-None-Match` / `If-Match`
    /// conditional writes. Returns `Ok(())` only on
    /// `ok (create, reject-create, reject-stale)`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if conditional writes are not enforced.
    pub async fn probe(store: Arc<dyn ObjectStore>, prefix: &Path) -> Result<()> {
        probe_store(store, prefix).await
    }

    /// Returns the underlying object store (for tests).
    #[cfg(test)]
    pub fn inner_store(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.inner)
    }

    /// Returns the prefix.
    #[cfg(test)]
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
            .finish()
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

    /// Skips the startup storage probe (`CELLD_STORAGE_PROBE=0` equivalent).
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

fn probe_path(prefix: &Path) -> Path {
    let base = if prefix.as_ref().is_empty() {
        Path::from("probe")
    } else {
        prefix.child("probe")
    };
    base.child("canary")
}

async fn probe_store(store: Arc<dyn ObjectStore>, prefix: &Path) -> Result<()> {
    let path = probe_path(prefix);

    // 1. create must succeed
    let first: PutResult = store
        .put_opts(
            &path,
            PutPayload::from_static(b"probe"),
            PutMode::Create.into(),
        )
        .await
        .map_err(|e| StoreError::Storage(format!("probe create failed: {e}")))?;

    // 2. create again must fail with AlreadyExists
    let second = store
        .put_opts(
            &path,
            PutPayload::from_static(b"probe2"),
            PutMode::Create.into(),
        )
        .await;
    match second {
        Err(object_store::Error::AlreadyExists { .. }) => {}
        Ok(_) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(
                "probe store accepted duplicate create — conditional writes not enforced (needs S3/R2/GCS/Azure, not B2/Hetzner)".to_string(),
            ));
        }
        Err(e) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(format!(
                "probe second create unexpected error (expected AlreadyExists): {e}"
            )));
        }
    }

    // 3. stale update must fail with Precondition (If-Match stale etag)
    let stale = UpdateVersion {
        e_tag: Some("\"stale-etag-should-not-match\"".to_string()),
        version: None,
    };
    let third = store
        .put_opts(
            &path,
            PutPayload::from_static(b"probe3"),
            PutMode::Update(stale).into(),
        )
        .await;
    match third {
        Err(object_store::Error::Precondition { .. }) => {}
        Ok(_) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(
                "probe store accepted stale If-Match — conditional overwrite not enforced"
                    .to_string(),
            ));
        }
        Err(e) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(format!(
                "probe stale update unexpected error (expected Precondition): {e}"
            )));
        }
    }

    // 4. valid update with current etag must succeed (proves If-Match works)
    let valid = UpdateVersion {
        e_tag: first.e_tag.clone(),
        version: first.version.clone(),
    };
    store
        .put_opts(
            &path,
            PutPayload::from_static(b"probe-ok"),
            PutMode::Update(valid).into(),
        )
        .await
        .map_err(|e| StoreError::Storage(format!("probe valid update failed: {e}")))?;

    // cleanup
    store
        .delete(&path)
        .await
        .map_err(|e| StoreError::Storage(format!("probe cleanup failed: {e}")))?;

    Ok(())
}

/// In-memory store helper for tests.
#[cfg(test)]
pub(crate) fn new_in_memory() -> Arc<dyn ObjectStore> {
    Arc::new(object_store::memory::InMemory::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::ObjectStore;

    #[tokio::test]
    async fn probe_ok_on_in_memory() {
        let store = new_in_memory();
        probe_store(Arc::clone(&store), &Path::default())
            .await
            .expect("InMemory must pass probe");
    }

    #[tokio::test]
    async fn probe_ok_with_prefix() {
        let store = new_in_memory();
        probe_store(Arc::clone(&store), &Path::from("oxkv"))
            .await
            .expect("prefixed probe must pass");
    }

    #[tokio::test]
    async fn probe_rejects_b2_like_store() {
        // Wraps InMemory but ignores PutMode, always overwrites — simulates B2/Hetzner
        #[derive(Debug)]
        struct NoConditionStore {
            inner: Arc<dyn ObjectStore>,
        }

        impl std::fmt::Display for NoConditionStore {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "NoConditionStore")
            }
        }

        #[async_trait::async_trait]
        impl ObjectStore for NoConditionStore {
            async fn put_opts(
                &self,
                location: &Path,
                payload: PutPayload,
                _opts: object_store::PutOptions,
            ) -> object_store::Result<PutResult> {
                // ignore mode — always overwrite
                self.inner.put(location, payload).await
            }

            async fn put_multipart_opts(
                &self,
                _location: &Path,
                _opts: object_store::PutMultipartOptions,
            ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
                unimplemented!()
            }

            async fn get_opts(
                &self,
                location: &Path,
                options: object_store::GetOptions,
            ) -> object_store::Result<object_store::GetResult> {
                self.inner.get_opts(location, options).await
            }

            async fn delete(&self, location: &Path) -> object_store::Result<()> {
                self.inner.delete(location).await
            }

            fn list(
                &self,
                prefix: Option<&Path>,
            ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
            {
                self.inner.list(prefix)
            }

            async fn list_with_delimiter(
                &self,
                prefix: Option<&Path>,
            ) -> object_store::Result<object_store::ListResult> {
                self.inner.list_with_delimiter(prefix).await
            }

            async fn copy(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy(from, to).await
            }

            async fn copy_if_not_exists(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy_if_not_exists(from, to).await
            }
        }

        let inner = new_in_memory();
        let bad: Arc<dyn ObjectStore> = Arc::new(NoConditionStore { inner });
        let err = probe_store(bad, &Path::default())
            .await
            .expect_err("must reject");
        assert!(
            err.to_string().contains("conditional writes not enforced")
                || err.to_string().contains("stale If-Match"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn builder_runs_probe_by_default() {
        let store = new_in_memory();
        let built = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv"))
            .build()
            .await
            .expect("builder with InMemory must pass probe");
        assert_eq!(built.prefix().as_ref(), "oxkv");
    }

    #[tokio::test]
    async fn builder_skip_probe_flag() {
        assert!(!S3Store::builder().is_skip_probe());
        assert!(S3Store::builder().skip_probe(true).is_skip_probe());
    }

    #[tokio::test]
    async fn builder_skip_probe_allows_b2_like_store() {
        #[derive(Debug)]
        struct NoConditionStore {
            inner: Arc<dyn ObjectStore>,
        }
        impl std::fmt::Display for NoConditionStore {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "NoConditionStore")
            }
        }
        #[async_trait::async_trait]
        impl ObjectStore for NoConditionStore {
            async fn put_opts(
                &self,
                location: &Path,
                payload: PutPayload,
                _opts: object_store::PutOptions,
            ) -> object_store::Result<PutResult> {
                self.inner.put(location, payload).await
            }
            async fn put_multipart_opts(
                &self,
                _location: &Path,
                _opts: object_store::PutMultipartOptions,
            ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
                unimplemented!()
            }
            async fn get_opts(
                &self,
                location: &Path,
                options: object_store::GetOptions,
            ) -> object_store::Result<object_store::GetResult> {
                self.inner.get_opts(location, options).await
            }
            async fn delete(&self, location: &Path) -> object_store::Result<()> {
                self.inner.delete(location).await
            }
            fn list(
                &self,
                prefix: Option<&Path>,
            ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
            {
                self.inner.list(prefix)
            }
            async fn list_with_delimiter(
                &self,
                prefix: Option<&Path>,
            ) -> object_store::Result<object_store::ListResult> {
                self.inner.list_with_delimiter(prefix).await
            }
            async fn copy(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy(from, to).await
            }
            async fn copy_if_not_exists(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy_if_not_exists(from, to).await
            }
        }

        let bad: Arc<dyn ObjectStore> = Arc::new(NoConditionStore {
            inner: new_in_memory(),
        });
        // without skip_probe -> must fail
        let err = S3Store::builder()
            .with_store(Arc::clone(&bad))
            .build()
            .await
            .expect_err("must reject without skip_probe");
        assert!(err.to_string().contains("conditional writes"));

        // with skip_probe -> must succeed (CELLD_STORAGE_PROBE=0 equiv)
        S3Store::builder()
            .with_store(bad)
            .skip_probe(true)
            .build()
            .await
            .expect("skip_probe must allow bad store");
    }

    #[tokio::test]
    async fn probe_static_entry_point() {
        let store = new_in_memory();
        S3Store::probe(store, &Path::default())
            .await
            .expect("static probe must pass");
    }
}
