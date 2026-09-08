//! Storage abstraction for the LSM engine.
//!
//! The engine speaks only to [`Storage`], so the same manifest/SST/blob logic
//! runs on S3 (via `object_store`, native-only), on a pure-Rust in-memory
//! store ([`MemStorage`], every target including `wasm32`), or on future
//! browser storage (OPFS/IndexedDB). The wire format (`OXKV` snapshot, SST,
//! blob pointer) stays identical across all implementations.
//!
//! # Error contracts
//!
//! Backends report two conditions so the engine's CAS-retry logic works
//! uniformly without backend-specific types:
//! - missing object: [`StoreError::Storage`] whose message contains `not found`
//! - conditional-write conflict: [`StoreError::CasConflict`]
//!
//! Conditional reads map `ETag` matches to [`StoreError::NotModified`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::store::{Result, StoreError};

/// `/`-delimited object path, e.g. `e000007/wal/00000042.log`.
///
/// Owned replacement for `object_store::Path`. [`Display`](std::fmt::Display)
/// output is the canonical string form also stored in manifests and blob
/// pointers, so snapshots stay portable across backends.
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
    /// Expected `ETag`; `None` skips the etag check.
    pub e_tag: Option<String>,
    /// Expected backend version; `None` skips the version check.
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
    /// Returns a `not found` [`StoreError::Storage`] when the object is missing.
    async fn get(&self, path: &ObjectPath) -> Result<GetOutput>;

    /// Fetches with `options` (e.g. `If-None-Match` for manifest polling).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotModified`] on etag match, `not found` when missing.
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

    /// Deletes the object at `path`; missing objects are `Ok`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Storage`] on backend failure.
    async fn delete(&self, path: &ObjectPath) -> Result<()>;
}

/// Pure-Rust in-memory [`Storage`].
///
/// Works on every target, including `wasm32`: the `JsLsmStore` binds the LSM
/// engine to this backend, and native tests use it instead of a cloud crate.
/// `ETag`s are per-write sequence numbers; versions are sequence strings.
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
        let obj = inner
            .objects
            .get(path.as_str())
            .ok_or_else(|| StoreError::Storage(format!("not found: {path}")))?;
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
}

/// `Arc<dyn ObjectStore>` implements [`Storage`], keeping `object_store` as
/// a native S3/memory/localfs backend behind the `oxkv-s3` feature.
#[cfg(all(not(target_arch = "wasm32"), feature = "oxkv-s3"))]
#[async_trait::async_trait]
impl Storage for Arc<dyn object_store::ObjectStore> {
    async fn get(&self, path: &ObjectPath) -> Result<GetOutput> {
        use object_store::path::Path;
        let path_ref = Path::from(path.as_str());
        let res = self
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
        match self.as_ref().delete(&path_ref).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(StoreError::Storage(format!("delete {path} failed: {e}"))),
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "oxkv-s3"))]
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

#[cfg(all(not(target_arch = "wasm32"), feature = "oxkv-s3"))]
fn map_put_error(path: &ObjectPath, err: object_store::Error) -> StoreError {
    match err {
        object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. } => {
            StoreError::CasConflict(format!("{path}: {err}"))
        }
        other => StoreError::Storage(format!("put {path} failed: {other}")),
    }
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
        store
            .put_opts(&path, b"1".to_vec(), PutMode::Create)
            .await
            .expect("create");
        store.delete(&path).await.expect("delete");
        assert!(store.get(&path).await.is_err());
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn object_path_strings_match_object_store() {
        let prefix = ObjectPath::new("my-app/oxkv");
        let wal = prefix.child("e000007").child("wal").child("00000042.log");
        assert_eq!(wal.to_string(), "my-app/oxkv/e000007/wal/00000042.log");
        assert!(ObjectPath::new("").is_empty());
        assert_eq!(
            ObjectPath::new("").child("manifest.json").to_string(),
            "manifest.json"
        );
        // Byte-identical canonical form to `object_store::Path` for engine paths.
        #[cfg(all(not(target_arch = "wasm32"), feature = "oxkv-s3"))]
        {
            let os = object_store::path::Path::from("my-app/oxkv/e000007/wal/00000042.log");
            assert_eq!(wal.to_string(), os.to_string());
        }
    }

    /// The `object_store` adapter satisfies the same contracts as [`MemStorage`].
    #[cfg(all(not(target_arch = "wasm32"), feature = "oxkv-s3"))]
    #[tokio::test]
    async fn object_store_impl_satisfies_contracts() {
        use object_store::memory::InMemory;

        let inner: Arc<dyn object_store::ObjectStore> = Arc::new(InMemory::new());
        let store: Arc<dyn Storage> = Arc::new(inner);
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
        let stale = ObjectVersion {
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
        store.delete(&path).await.expect("delete");
        store.delete(&path).await.expect("delete idempotent");
        let err = store.get(&path).await.expect_err("missing get fails");
        assert!(err.to_string().contains("not found"), "{err}");
    }
}

#[cfg(target_arch = "wasm32")]
use sha2::{Digest, Sha256};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;

/// Origin-private-file-system [`Storage`] for browsers (`wasm32` only).
///
/// Persists objects as real OPFS files under one `oxkv` root directory, so
/// contents survive page reloads. Main-thread only: async file handles work
/// on the main thread, while sync access handles are worker-only.
///
/// Cross-tab races resolve last-writer-wins — OPFS offers no
/// conditional-write primitive on the main thread, so `Create`/`Update`
/// preconditions are checked read-then-write. Single-tab behavior (probe,
/// fencing, CAS) is exact.
///
/// `ETag`s and versions are hex `SHA-256` content hashes: stable across
/// reloads, unique per byte content, no sidecar files.
#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
pub struct OpfsStorage {
    root: web_sys::FileSystemDirectoryHandle,
}

/// Hex `SHA-256` of `bytes` — stable etag/version across reloads.
#[cfg(target_arch = "wasm32")]
fn content_etag(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut buf = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as writer;
        let _ = writer::write_fmt(&mut buf, format_args!("{byte:02x}"));
    }
    buf
}

/// Reports whether a JS rejection is a `NotFoundError` DOM exception.
#[cfg(target_arch = "wasm32")]
fn is_not_found(err: &wasm_bindgen::JsValue) -> bool {
    err.clone()
        .dyn_into::<web_sys::DomException>()
        .is_ok_and(|d| d.name() == "NotFoundError")
}

/// Wraps a JS rejection as [`StoreError::Storage`] with context.
#[cfg(target_arch = "wasm32")]
fn js_err(context: &str, err: wasm_bindgen::JsValue) -> StoreError {
    let detail = err.dyn_into::<web_sys::DomException>().map_or_else(
        |e| format!("{e:?}"),
        |d| format!("{}: {}", d.name(), d.message()),
    );
    StoreError::Storage(format!("{context}: {detail}"))
}

/// Drives a JS promise on the local task queue, bridging `!Send` JS futures
/// into the `Send`-required [`Storage`] methods.
///
/// `wasm_bindgen_futures::JsFuture` is `!Send` on single-threaded wasm, so it
/// can never be awaited directly here. Instead the promise runs in a
/// `spawn_local` task and the result crosses back through a `Send` oneshot.
#[cfg(target_arch = "wasm32")]
async fn js_await(
    promise: js_sys::Promise,
) -> std::result::Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue> {
    let (tx, rx) = futures::channel::oneshot::channel();
    wasm_bindgen_futures::spawn_local(async move {
        let _ = tx.send(wasm_bindgen_futures::JsFuture::from(promise).await);
    });
    rx.await
        .map_err(|_| wasm_bindgen::JsValue::from_str("JS task cancelled"))?
}

#[cfg(target_arch = "wasm32")]
impl OpfsStorage {
    /// Opens (creating) the `oxkv` root directory in origin private storage.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Storage`] when OPFS is unavailable or denied.
    pub async fn open() -> Result<Self> {
        let window = web_sys::window()
            .ok_or_else(|| StoreError::Storage("OPFS requires a window context".to_string()))?;
        let origin_js = js_await(window.navigator().storage().get_directory())
            .await
            .map_err(|e| js_err("OPFS getDirectory failed", e))?;
        let origin: web_sys::FileSystemDirectoryHandle = origin_js
            .dyn_into()
            .map_err(|e| js_err("OPFS root is not a directory", e))?;
        let opts = web_sys::FileSystemGetDirectoryOptions::new();
        opts.set_create(true);
        let root_js = js_await(origin.get_directory_handle_with_options("oxkv", &opts))
            .await
            .map_err(|e| js_err("OPFS create oxkv root failed", e))?;
        let root = root_js
            .dyn_into()
            .map_err(|e| js_err("OPFS oxkv root is not a directory", e))?;
        Ok(Self { root })
    }

    /// Splits `path` into parent segments and the file name.
    fn split(path: &ObjectPath) -> Result<(Vec<&str>, &str)> {
        if path.is_empty() {
            return Err(StoreError::Storage("empty object path".to_string()));
        }
        let mut segments: Vec<&str> = path.as_str().split('/').collect();
        let name = segments.pop().unwrap_or_default();
        if name.is_empty() {
            return Err(StoreError::Storage(format!("invalid object path: {path}")));
        }
        Ok((segments, name))
    }

    /// Resolves parent directories, creating them when `create` is set.
    /// Returns `None` when a directory is missing and `create` is unset.
    async fn parent(
        &self,
        segments: &[&str],
        create: bool,
    ) -> Result<Option<web_sys::FileSystemDirectoryHandle>> {
        let mut dir = self.root.clone();
        for segment in segments {
            let next = if create {
                let opts = web_sys::FileSystemGetDirectoryOptions::new();
                opts.set_create(true);
                js_await(dir.get_directory_handle_with_options(segment, &opts))
                    .await
                    .map_err(|e| js_err("OPFS create directory failed", e))?
            } else {
                match js_await(dir.get_directory_handle(segment)).await {
                    Ok(handle) => handle,
                    Err(e) if is_not_found(&e) => return Ok(None),
                    Err(e) => return Err(js_err("OPFS open directory failed", e)),
                }
            };
            dir = next
                .dyn_into()
                .map_err(|e| js_err("OPFS entry is not a directory", e))?;
        }
        Ok(Some(dir))
    }

    /// Reads bytes + etag, or `None` when the object (or its directory) is missing.
    async fn read_existing(&self, path: &ObjectPath) -> Result<Option<(Vec<u8>, String)>> {
        let (segments, name) = Self::split(path)?;
        let Some(dir) = self.parent(&segments, false).await? else {
            return Ok(None);
        };
        let handle = match js_await(dir.get_file_handle(name)).await {
            Ok(handle) => handle,
            Err(e) if is_not_found(&e) => return Ok(None),
            Err(e) => return Err(js_err("OPFS open file failed", e)),
        };
        let handle: web_sys::FileSystemFileHandle = handle
            .dyn_into()
            .map_err(|e| js_err("OPFS entry is not a file", e))?;
        let file: web_sys::File = js_await(handle.get_file())
            .await
            .map_err(|e| js_err("OPFS read file failed", e))?
            .dyn_into()
            .map_err(|e| js_err("OPFS entry is not a file", e))?;
        let buffer = js_await(file.array_buffer())
            .await
            .map_err(|e| js_err("OPFS read bytes failed", e))?;
        let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
        let etag = content_etag(&bytes);
        Ok(Some((bytes, etag)))
    }

    /// Writes `payload`, creating parent directories. Callers enforce preconditions.
    async fn write_new(&self, path: &ObjectPath, payload: &[u8]) -> Result<String> {
        let (segments, name) = Self::split(path)?;
        let dir = self
            .parent(&segments, true)
            .await?
            .ok_or_else(|| StoreError::Storage(format!("OPFS missing parent for {path}")))?;
        let file_opts = web_sys::FileSystemGetFileOptions::new();
        file_opts.set_create(true);
        let handle_js = js_await(dir.get_file_handle_with_options(name, &file_opts))
            .await
            .map_err(|e| js_err("OPFS create file failed", e))?;
        let handle: web_sys::FileSystemFileHandle = handle_js
            .dyn_into()
            .map_err(|e| js_err("OPFS entry is not a file", e))?;
        let stream_js = js_await(handle.create_writable())
            .await
            .map_err(|e| js_err("OPFS open writer failed", e))?;
        let stream: web_sys::FileSystemWritableFileStream = stream_js
            .dyn_into()
            .map_err(|e| js_err("OPFS writer is not writable", e))?;
        let write = stream
            .write_with_u8_array(payload)
            .map_err(|e| js_err("OPFS write failed", e))?;
        js_await(write)
            .await
            .map_err(|e| js_err("OPFS write failed", e))?;
        let writable: &web_sys::WritableStream = stream.unchecked_ref();
        js_await(writable.close())
            .await
            .map_err(|e| js_err("OPFS close failed", e))?;
        Ok(content_etag(payload))
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait]
impl Storage for OpfsStorage {
    async fn get(&self, path: &ObjectPath) -> Result<GetOutput> {
        match self.read_existing(path).await? {
            Some((bytes, etag)) => Ok(GetOutput {
                bytes,
                e_tag: Some(etag.clone()),
                version: Some(etag),
            }),
            None => Err(StoreError::Storage(format!("not found: {path}"))),
        }
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
        match mode {
            PutMode::Create => {
                if self.read_existing(path).await?.is_some() {
                    return Err(StoreError::CasConflict(format!("{path}: already exists")));
                }
            }
            PutMode::Update(expected) => {
                let Some((_, etag)) = self.read_existing(path).await? else {
                    return Err(StoreError::CasConflict(format!(
                        "{path}: missing for update"
                    )));
                };
                if expected.e_tag.as_deref() != Some(etag.as_str()) {
                    return Err(StoreError::CasConflict(format!("{path}: etag mismatch")));
                }
                if let Some(version) = expected.version.as_deref()
                    && version != etag
                {
                    return Err(StoreError::CasConflict(format!("{path}: version mismatch")));
                }
            }
        }
        let etag = self.write_new(path, &payload).await?;
        Ok(PutOutcome {
            e_tag: Some(etag.clone()),
            version: Some(etag),
        })
    }

    async fn delete(&self, path: &ObjectPath) -> Result<()> {
        let (segments, name) = Self::split(path)?;
        let Some(dir) = self.parent(&segments, false).await? else {
            return Ok(());
        };
        match js_await(dir.remove_entry(name)).await {
            Ok(_) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(js_err("OPFS delete failed", e)),
        }
    }
}
