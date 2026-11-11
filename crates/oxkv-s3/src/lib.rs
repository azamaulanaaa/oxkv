#![allow(clippy::multiple_crate_versions)]

//! `object_store` adapter for the oxkv key-value store.
//!
//! [`ObjectStoreStorage`] wraps an [`object_store::ObjectStore`] and
//! implements [`oxkv::Storage`]. Pass it to
//! [`OxKvStoreBuilder::with_store`](oxkv::OxKvStoreBuilder::with_store).
//! This crate is native-only.
//!
//! The adapter keeps the error contract of the LSM engine:
//!
//! - A missing object maps to [`StoreError::Storage`] with `not found` in the
//!   message.
//! - A failed conditional write maps to [`StoreError::CasConflict`].
//! - An `ETag` match on a conditional read maps to
//!   [`StoreError::NotModified`].

use std::sync::Arc;

use async_trait::async_trait;
use oxkv::{GetOptions, GetOutput, ObjectPath, PutMode, PutOutcome, Result, Storage, StoreError};

/// [`Storage`] adapter over an `object_store` backend.
///
/// The type supports S3, GCS, Azure, and local files. The wrapped
/// [`object_store::ObjectStore`] controls the endpoint, the credentials, and
/// the request policy.
#[derive(Clone)]
pub struct ObjectStoreStorage {
    inner: Arc<dyn object_store::ObjectStore>,
}

impl ObjectStoreStorage {
    /// Wraps an `object_store` backend.
    #[must_use]
    pub fn new(inner: Arc<dyn object_store::ObjectStore>) -> Self {
        Self { inner }
    }

    /// Borrows the wrapped backend.
    #[must_use]
    pub fn inner(&self) -> &Arc<dyn object_store::ObjectStore> {
        &self.inner
    }
}

impl From<Arc<dyn object_store::ObjectStore>> for ObjectStoreStorage {
    fn from(inner: Arc<dyn object_store::ObjectStore>) -> Self {
        Self::new(inner)
    }
}

#[async_trait]
impl Storage for ObjectStoreStorage {
    async fn get(&self, path: &ObjectPath) -> Result<GetOutput> {
        use object_store::path::Path;
        let path_ref = Path::from(path.as_str());
        let res = self
            .inner
            .as_ref()
            .get(&path_ref)
            .await
            .map_err(|e| map_get_error(path, e))?;
        let e_tag = res.meta.e_tag.clone();
        let version = res.meta.version.clone();
        let bytes = res
            .bytes()
            .await
            .map_err(|e| StoreError::Storage(format!("read {path} failed: {e}")))?
            .to_vec();
        Ok(GetOutput {
            bytes,
            e_tag,
            version,
        })
    }

    async fn get_opts(&self, path: &ObjectPath, options: GetOptions) -> Result<GetOutput> {
        use object_store::path::Path;
        let path_ref = Path::from(path.as_str());
        let opts = object_store::GetOptions {
            if_none_match: options.if_none_match,
            ..Default::default()
        };
        let res = self
            .inner
            .as_ref()
            .get_opts(&path_ref, opts)
            .await
            .map_err(|e| map_get_error(path, e))?;
        let e_tag = res.meta.e_tag.clone();
        let version = res.meta.version.clone();
        let bytes = res
            .bytes()
            .await
            .map_err(|e| StoreError::Storage(format!("read {path} failed: {e}")))?
            .to_vec();
        Ok(GetOutput {
            bytes,
            e_tag,
            version,
        })
    }

    async fn put_opts(
        &self,
        path: &ObjectPath,
        payload: Vec<u8>,
        mode: PutMode,
    ) -> Result<PutOutcome> {
        use object_store::path::Path;
        let path_ref = Path::from(path.as_str());
        let payload = object_store::PutPayload::from(payload);
        let mode = match mode {
            PutMode::Create => object_store::PutMode::Create,
            PutMode::Update(expected) => {
                object_store::PutMode::Update(object_store::UpdateVersion {
                    e_tag: expected.e_tag,
                    version: expected.version,
                })
            }
        };
        let opts = object_store::PutOptions {
            mode,
            ..Default::default()
        };
        let res = self
            .inner
            .as_ref()
            .put_opts(&path_ref, payload, opts)
            .await
            .map_err(|e| map_put_error(path, e))?;
        Ok(PutOutcome {
            e_tag: res.e_tag,
            version: res.version,
        })
    }

    async fn delete(&self, path: &ObjectPath) -> Result<()> {
        use object_store::path::Path;
        let path_ref = Path::from(path.as_str());
        match self.inner.as_ref().delete(&path_ref).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(StoreError::Storage(format!("delete {path} failed: {e}"))),
        }
    }

    async fn list(&self, prefix: &ObjectPath) -> Result<Vec<ObjectPath>> {
        use futures::StreamExt;
        use object_store::path::Path;
        let path_ref = Path::from(prefix.as_str());
        let mut keys = Vec::new();
        // `object_store` returns one stream for the whole listing. Every page
        // must be drained. Stopping early would return a partial listing.
        let mut pages = self.inner.as_ref().list(Some(&path_ref));
        while let Some(item) = pages.next().await {
            let meta = item.map_err(|e| map_list_error(&e))?;
            keys.push(ObjectPath::from(meta.location.as_ref()));
        }
        Ok(keys)
    }
}

/// Maps an `object_store` read error to a [`StoreError`].
fn map_get_error(path: &ObjectPath, err: object_store::Error) -> StoreError {
    match err {
        object_store::Error::NotFound { .. } => StoreError::Storage(format!("not found: {path}")),
        object_store::Error::NotModified { .. } => StoreError::NotModified,
        object_store::Error::Precondition { .. } => {
            StoreError::Storage(format!("precondition failed: {path}: {err}"))
        }
        other => StoreError::Storage(format!("get {path} failed: {other}")),
    }
}

/// Maps an `object_store` list error to a [`StoreError`].
fn map_list_error(err: &object_store::Error) -> StoreError {
    StoreError::Storage(format!("list failed: {err}"))
}

/// Maps an `object_store` write error to a [`StoreError`].
fn map_put_error(path: &ObjectPath, err: object_store::Error) -> StoreError {
    match err {
        object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. } => {
            StoreError::CasConflict(format!("{path}: {err}"))
        }
        other => StoreError::Storage(format!("put {path} failed: {other}")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The adapter satisfies the same contracts as every other backend.
    #[tokio::test]
    async fn object_store_impl_satisfies_contracts() {
        use object_store::memory::InMemory;

        let inner: Arc<dyn object_store::ObjectStore> = Arc::new(InMemory::new());
        let store: Arc<dyn Storage> = Arc::new(ObjectStoreStorage::new(inner));
        let path = ObjectPath::new("contract/wal.log");
        let created = store
            .put_opts(&path, b"v1".to_vec(), PutMode::Create)
            .await
            .expect("create");
        assert!(created.e_tag.is_some());
        let err = store
            .put_opts(&path, b"v2".to_vec(), PutMode::Create)
            .await
            .expect_err("duplicate create conflicts");
        assert!(matches!(err, StoreError::CasConflict(_)), "{err:?}");
        let stale = oxkv::ObjectVersion {
            e_tag: Some("\"stale\"".to_string()),
            version: None,
        };
        let err = store
            .put_opts(&path, b"v2".to_vec(), PutMode::Update(stale))
            .await
            .expect_err("stale update conflicts");
        assert!(matches!(err, StoreError::CasConflict(_)), "{err:?}");
        let opts = GetOptions {
            if_none_match: created.e_tag,
        };
        let err = store
            .get_opts(&path, opts)
            .await
            .expect_err("etag match is NotModified");
        assert_eq!(err, StoreError::NotModified);

        // `list` must report the object it just wrote under its own prefix.
        let listed = store
            .list(&ObjectPath::from("contract"))
            .await
            .expect("list");
        assert_eq!(listed, vec![path.clone()], "the object is unlisted");

        store.delete(&path).await.expect("delete");
        store.delete(&path).await.expect("delete idempotent");
        let err = store.get(&path).await.expect_err("missing get fails");
        assert!(err.to_string().contains("not found"), "{err}");
    }

    /// The engine paths keep the canonical string form of `object_store`.
    #[test]
    fn object_paths_match_object_store_paths() {
        let prefix = ObjectPath::new("my-app/oxkv");
        let wal = prefix.child("e000007").child("wal").child("00000042.log");
        assert_eq!(wal.to_string(), "my-app/oxkv/e000007/wal/00000042.log");
        let os = object_store::path::Path::from("my-app/oxkv/e000007/wal/00000042.log");
        assert_eq!(wal.to_string(), os.to_string());
    }
}
