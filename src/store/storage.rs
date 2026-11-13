//! Storage abstraction for the LSM engine.
//!
//! The engine speaks only to [`Storage`]. The same manifest/SST/blob logic
//! runs on S3, GCS, Azure, and local files (via the separate `oxkv-s3` crate).
//! It also runs on the pure-Rust in-memory store ([`MemStorage`], every target
//! including `wasm32`). The separate `oxkv-opfs` crate adds browser storage.
//! The wire format (`OXKV` snapshot, SST, blob pointer) stays identical across
//! all implementations.
//!
//! # Error contracts
//!
//! Backends report two conditions. These conditions let the engine's CAS-retry
//! logic work uniformly without backend-specific types:
//! - missing object: [`StoreError::Storage`] with
//!   [`StorageCause::NotFound`] and `not found` in `message`
//! - conditional-write conflict: [`StoreError::CasConflict`]
//!
//! Conditional reads map `ETag` matches to [`StoreError::NotModified`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::store::{Result, StoreError};

/// `/`-delimited object path, e.g. `e000007/wal/00000042.log`.
///
/// Portable path type for storage backends. [`Display`](std::fmt::Display)
/// output is the canonical string form. Manifests and blob pointers also store
/// this form. As a result, snapshots stay portable across backends.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ObjectPath(String);

impl ObjectPath {
    /// Creates a path from its canonical string form.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    /// Returns `true` when this is the empty (root) path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Appends one segment, e.g. `child("wal")`.
    #[must_use]
    pub fn child(&self, segment: &str) -> Self {
        if self.0.is_empty() {
            Self(segment.to_string())
        } else {
            Self(format!("{}/{}", self.0, segment))
        }
    }

    /// Borrows the canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ObjectPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ObjectPath {
    fn from(path: &str) -> Self {
        Self(path.to_string())
    }
}

impl From<String> for ObjectPath {
    fn from(path: String) -> Self {
        Self(path)
    }
}

/// Conditional-write mode for [`Storage::put_opts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutMode {
    /// Fail when the object already exists (`If-None-Match: *`).
    Create,
    /// Overwrite only when the current version matches (`If-Match`).
    Update(ObjectVersion),
}

/// Version precondition for [`PutMode::Update`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObjectVersion {
    /// Expected `ETag`. `None` skips the etag check.
    pub e_tag: Option<String>,
    /// Expected backend version. `None` skips the version check.
    pub version: Option<String>,
}

/// Read options for [`Storage::get_opts`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GetOptions {
    /// Return [`StoreError::NotModified`] when the remote etag matches.
    pub if_none_match: Option<String>,
}

/// Conditional put outcome.
#[derive(Debug, Clone)]
pub struct PutOutcome {
    /// New `ETag` after a successful put.
    pub e_tag: Option<String>,
    /// New version.
    pub version: Option<String>,
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

/// Coarse cause of a storage failure.
///
/// The cause names the kind of failure. It does not change retry
/// classification. Every cause maps to
/// [`ErrorKind::Transient`](crate::store::ErrorKind::Transient).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageCause {
    /// The object is absent.
    NotFound,
    /// A network request failed. A retry can succeed.
    Network,
    /// An I/O operation failed. A retry can succeed.
    Io,
    /// The object bytes are not valid. This includes a parse failure and a
    /// checksum failure.
    Corrupt,
    /// The group-commit leader stopped before it reported the outcome.
    LeaderLost,
    /// The failure has no finer cause.
    Other,
}

/// A storage failure with a structured cause.
///
/// The cause does not change the retry class. See [`StorageCause`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageError {
    /// Context for the failure. This includes the operation and the object
    /// path where the code knows them.
    pub message: String,
    /// Coarse cause of the failure.
    pub cause: StorageCause,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Minimal object-store surface required by the LSM engine.
///
/// See the module-level contracts above for how backends must report
/// missing objects and write conflicts.
#[async_trait::async_trait]
pub trait Storage: Send + Sync + 'static {
    /// Fetches the object at `path`.
    ///
    /// # Errors
    ///
    /// Returns a `not found` [`StoreError::Storage`] with
    /// [`StorageCause::NotFound`] when the object is missing.
    async fn get(&self, path: &ObjectPath) -> Result<GetOutput>;

    /// Fetches with `options` (e.g. `If-None-Match` for manifest polling).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotModified`] on an etag match. It returns
    /// `not found` with [`StorageCause::NotFound`] when the object is missing.
    async fn get_opts(&self, path: &ObjectPath, options: GetOptions) -> Result<GetOutput>;

    /// Puts `payload` at `path` with `mode` (`Create` = `If-None-Match`,
    /// `Update` = `If-Match`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::CasConflict`] when the precondition fails.
    async fn put_opts(
        &self,
        path: &ObjectPath,
        payload: Vec<u8>,
        mode: PutMode,
    ) -> Result<PutOutcome>;

    /// Deletes the object at `path`. A missing object returns `Ok`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Storage`] on backend failure.
    async fn delete(&self, path: &ObjectPath) -> Result<()>;

    /// Lists every object key under `prefix`, recursively.
    ///
    /// An empty `prefix` lists the whole store. Keys are relative to the
    /// store root, in the same `/`-delimited form as [`ObjectPath`]. A
    /// directory is not an object, so it never appears in the result. The
    /// order of the keys is unspecified.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Storage`] on backend failure.
    async fn list(&self, prefix: &ObjectPath) -> Result<Vec<ObjectPath>>;
}

/// Pure-Rust in-memory [`Storage`].
///
/// Works on every target, including `wasm32`. The `JsLsmStore` binds the LSM
/// engine to this backend. Native tests use it instead of a cloud crate. An
/// `ETag` is a per-write sequence number. A version is a sequence string.
#[derive(Clone, Default)]
pub struct MemStorage {
    inner: Arc<futures::lock::Mutex<MemInner>>,
}

#[derive(Default)]
struct MemInner {
    objects: HashMap<String, MemObject>,
    seq: u64,
}

#[derive(Clone)]
struct MemObject {
    bytes: Vec<u8>,
    etag: String,
    version: u64,
}

impl MemStorage {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Assigns the next sequence number and its etag.
    fn next_version(inner: &mut MemInner) -> (String, u64) {
        inner.seq += 1;
        (format!("{:016x}", inner.seq), inner.seq)
    }
}

#[async_trait::async_trait]
impl Storage for MemStorage {
    async fn get(&self, path: &ObjectPath) -> Result<GetOutput> {
        let inner = self.inner.lock().await;
        let obj = inner.objects.get(path.as_str()).ok_or_else(|| {
            StoreError::storage_with_cause(format!("not found: {path}"), StorageCause::NotFound)
        })?;
        Ok(GetOutput {
            bytes: obj.bytes.clone(),
            e_tag: Some(obj.etag.clone()),
            version: Some(obj.version.to_string()),
        })
    }

    async fn get_opts(&self, path: &ObjectPath, options: GetOptions) -> Result<GetOutput> {
        let out = self.get(path).await?;
        if options.if_none_match.as_deref() == out.e_tag.as_deref() && out.e_tag.is_some() {
            return Err(StoreError::NotModified);
        }
        Ok(out)
    }

    async fn put_opts(
        &self,
        path: &ObjectPath,
        payload: Vec<u8>,
        mode: PutMode,
    ) -> Result<PutOutcome> {
        let mut inner = self.inner.lock().await;
        match mode {
            PutMode::Create => {
                if inner.objects.contains_key(path.as_str()) {
                    return Err(StoreError::CasConflict(format!("{path}: already exists")));
                }
            }
            PutMode::Update(expected) => {
                let current = inner.objects.get(path.as_str()).ok_or_else(|| {
                    StoreError::CasConflict(format!("{path}: missing for update"))
                })?;
                if expected.e_tag.as_deref() != Some(current.etag.as_str()) {
                    return Err(StoreError::CasConflict(format!("{path}: etag mismatch")));
                }
                if let Some(version) = expected.version.as_deref()
                    && version != current.version.to_string()
                {
                    return Err(StoreError::CasConflict(format!("{path}: version mismatch")));
                }
            }
        }
        let (etag, version) = Self::next_version(&mut inner);
        inner.objects.insert(
            path.as_str().to_string(),
            MemObject {
                bytes: payload,
                etag: etag.clone(),
                version,
            },
        );
        Ok(PutOutcome {
            e_tag: Some(etag),
            version: Some(version.to_string()),
        })
    }

    async fn delete(&self, path: &ObjectPath) -> Result<()> {
        self.inner.lock().await.objects.remove(path.as_str());
        Ok(())
    }

    async fn list(&self, prefix: &ObjectPath) -> Result<Vec<ObjectPath>> {
        let inner = self.inner.lock().await;
        let mut keys: Vec<ObjectPath> = inner
            .objects
            .keys()
            .filter(|key| under_prefix(key, prefix.as_str()))
            .map(|key| ObjectPath::from(key.as_str()))
            .collect();
        keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(keys)
    }
}

/// Reports whether `key` sits under `prefix`.
///
/// A prefix matches whole segments only. Therefore `oxkv` matches
/// `oxkv/e000001` but not `oxkv2/e000001`. The empty prefix matches every
/// key.
fn under_prefix(key: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    key.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn mem_put_get_roundtrip() {
        let store = MemStorage::new();
        let path = ObjectPath::new("a/b.log");
        let out = store
            .put_opts(&path, b"data".to_vec(), PutMode::Create)
            .await
            .expect("create");
        assert!(out.e_tag.is_some());
        let got = store.get(&path).await.expect("get");
        assert_eq!(got.bytes, b"data");
        assert_eq!(got.e_tag, out.e_tag);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn mem_create_conflicts() {
        let store = MemStorage::new();
        let path = ObjectPath::new("x");
        store
            .put_opts(&path, b"1".to_vec(), PutMode::Create)
            .await
            .expect("first create");
        let err = store
            .put_opts(&path, b"2".to_vec(), PutMode::Create)
            .await
            .expect_err("second create conflicts");
        assert!(matches!(err, StoreError::CasConflict(_)), "{err:?}");
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn mem_update_checks_etag() {
        let store = MemStorage::new();
        let path = ObjectPath::new("x");
        let created = store
            .put_opts(&path, b"1".to_vec(), PutMode::Create)
            .await
            .expect("create");
        let stale = ObjectVersion {
            e_tag: Some("\"stale\"".to_string()),
            version: None,
        };
        let err = store
            .put_opts(&path, b"2".to_vec(), PutMode::Update(stale))
            .await
            .expect_err("stale update conflicts");
        assert!(matches!(err, StoreError::CasConflict(_)), "{err:?}");
        let valid = ObjectVersion {
            e_tag: created.e_tag,
            version: None,
        };
        store
            .put_opts(&path, b"2".to_vec(), PutMode::Update(valid))
            .await
            .expect("valid update");
        assert_eq!(store.get(&path).await.expect("get").bytes, b"2");
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn mem_get_opts_not_modified() {
        let store = MemStorage::new();
        let path = ObjectPath::new("x");
        let created = store
            .put_opts(&path, b"1".to_vec(), PutMode::Create)
            .await
            .expect("create");
        let opts = GetOptions {
            if_none_match: created.e_tag,
        };
        let err = store
            .get_opts(&path, opts)
            .await
            .expect_err("etag match is NotModified");
        assert_eq!(err, StoreError::NotModified);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn mem_delete_idempotent_and_missing() {
        let store = MemStorage::new();
        let path = ObjectPath::new("gone");
        store.delete(&path).await.expect("missing delete is Ok");
        let err = store.get(&path).await.expect_err("missing get fails");
        assert!(err.to_string().contains("not found"), "{err}");
        assert_eq!(err.storage_cause(), Some(StorageCause::NotFound));
        store
            .put_opts(&path, b"1".to_vec(), PutMode::Create)
            .await
            .expect("create");
        store.delete(&path).await.expect("delete");
        assert!(store.get(&path).await.is_err());
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn mem_list_matches_whole_segments() {
        let store = MemStorage::new();
        for key in [
            "oxkv/e000001/wal/00000000.log",
            "oxkv/e000001/sst/L0/000000000.sst",
            "oxkv2/e000001/wal/00000000.log",
            "other.txt",
        ] {
            store
                .put_opts(&ObjectPath::from(key), b"x".to_vec(), PutMode::Create)
                .await
                .expect("create");
        }
        let listed = store.list(&ObjectPath::from("oxkv")).await.expect("list");
        let listed: Vec<&str> = listed.iter().map(ObjectPath::as_str).collect();
        assert_eq!(
            listed,
            vec![
                "oxkv/e000001/sst/L0/000000000.sst",
                "oxkv/e000001/wal/00000000.log",
            ],
            "a prefix must not match a partial segment"
        );
        let all = store.list(&ObjectPath::default()).await.expect("list");
        assert_eq!(all.len(), 4, "the empty prefix lists the whole store");
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn object_path_strings_are_canonical() {
        let prefix = ObjectPath::new("my-app/oxkv");
        let wal = prefix.child("e000007").child("wal").child("00000042.log");
        assert_eq!(wal.to_string(), "my-app/oxkv/e000007/wal/00000042.log");
        assert!(ObjectPath::new("").is_empty());
        assert_eq!(
            ObjectPath::new("").child("manifest.json").to_string(),
            "manifest.json"
        );
    }
}
