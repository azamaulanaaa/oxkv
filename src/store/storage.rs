//! Storage abstraction for the LSM engine.
//!
//! The engine is generic over this trait so the same manifest/SST/blob logic
//! can run on S3 (via `object_store`), on local files, or on browser storage
//! (OPFS/IndexedDB). The wire format (`OXKV` snapshot, SST, blob pointer) stays
//! identical across all implementations.

use std::sync::Arc;

use object_store::path::Path;
use object_store::{GetOptions, ObjectStore, PutMode, PutPayload, PutResult};

use crate::store::{Result, StoreError};

/// Conditional put outcome.
#[derive(Debug, Clone)]
pub struct PutOutcome {
    /// New `ETag` after a successful put.
    pub e_tag: Option<String>,
    /// New version.
    pub version: Option<String>,
}

impl From<PutResult> for PutOutcome {
    fn from(value: PutResult) -> Self {
        Self {
            e_tag: value.e_tag,
            version: value.version,
        }
    }
}

/// Object metadata returned by `get`.
#[derive(Debug, Clone)]
pub struct GetOutput {
    /// Raw bytes.
    pub bytes: Vec<u8>,
    /// `ETag` if the store provides one.
    pub e_tag: Option<String>,
    /// Version if the store provides one.
    pub version: Option<String>,
}

/// Minimal object-store surface required by the LSM engine.
#[async_trait::async_trait]
pub trait Storage: Clone + Send + Sync + 'static {
    /// Fetches the object at `path`.
    async fn get(&self, path: &Path) -> Result<GetOutput>;

    /// Fetches with `GetOptions` (e.g. `If-None-Match` for manifest polling).
    async fn get_opts(&self, path: &Path, options: GetOptions) -> Result<GetOutput>;

    /// Puts `payload` at `path` with `mode` (`Create` = `If-None-Match`, `Update` = `If-Match`).
    async fn put_opts(&self, path: &Path, payload: PutPayload, mode: PutMode)
    -> Result<PutOutcome>;

    /// Deletes the object at `path`.
    async fn delete(&self, path: &Path) -> Result<()>;
}

/// `Arc<dyn ObjectStore>` implements [`Storage`] directly, keeping `object_store`
/// as the canonical `S3`/`memory`/`localfs` backend without extra wrapping.
#[async_trait::async_trait]
impl Storage for Arc<dyn ObjectStore> {
    async fn get(&self, path: &Path) -> Result<GetOutput> {
        let res = self
            .as_ref()
            .get(path)
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

    async fn get_opts(&self, path: &Path, options: GetOptions) -> Result<GetOutput> {
        let res = self
            .as_ref()
            .get_opts(path, options)
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
        path: &Path,
        payload: PutPayload,
        mode: PutMode,
    ) -> Result<PutOutcome> {
        let opts = object_store::PutOptions {
            mode,
            ..Default::default()
        };
        let res = self
            .as_ref()
            .put_opts(path, payload, opts)
            .await
            .map_err(|e| map_put_error(path, e))?;
        Ok(res.into())
    }

    async fn delete(&self, path: &Path) -> Result<()> {
        self.as_ref()
            .delete(path)
            .await
            .map_err(|e| StoreError::Storage(format!("delete {path} failed: {e}")))?;
        Ok(())
    }
}

fn map_get_error(path: &Path, err: object_store::Error) -> StoreError {
    match err {
        object_store::Error::NotFound { .. } => StoreError::Storage(format!("not found: {path}")),
        object_store::Error::NotModified { .. } => StoreError::NotModified,
        object_store::Error::Precondition { .. } => {
            StoreError::Storage(format!("precondition failed: {path}: {err}"))
        }
        other => StoreError::Storage(format!("get {path} failed: {other}")),
    }
}

fn map_put_error(path: &Path, err: object_store::Error) -> StoreError {
    match err {
        object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. } => {
            StoreError::Storage(format!("CAS conflict: {path}: {err}"))
        }
        other => StoreError::Storage(format!("put {path} failed: {other}")),
    }
}
