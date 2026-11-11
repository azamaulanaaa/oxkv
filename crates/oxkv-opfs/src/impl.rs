use oxkv::{GetOptions, GetOutput, ObjectPath, PutMode, PutOutcome, Result, Storage, StoreError};

/// Announces that a browser-only test will check nothing on this host.
///
/// The Node harness has no `window`. The Chrome CI job runs the real test.
#[cfg(all(test, target_arch = "wasm32"))]
fn announce_skip(test: &str, reason: &str) {
    let message = format!("SKIPPED {test}: {reason}\n");
    if write_host_stderr(&message) {
        return;
    }
    web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&message));
}

/// Writes `message` to the host process's stderr.
///
/// Returns `false` when the host process has no stderr.
#[cfg(all(test, target_arch = "wasm32"))]
fn write_host_stderr(message: &str) -> bool {
    fn property(
        object: &wasm_bindgen::JsValue,
        key: &str,
    ) -> std::result::Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue> {
        js_sys::Reflect::get(object, &wasm_bindgen::JsValue::from_str(key))
    }
    let Ok(process) = property(&js_sys::global(), "process") else {
        return false;
    };
    let Ok(stderr) = property(&process, "stderr") else {
        return false;
    };
    let Ok(write) =
        property(&stderr, "write").and_then(wasm_bindgen::JsCast::dyn_into::<js_sys::Function>)
    else {
        return false;
    };
    write
        .call1(&stderr, &wasm_bindgen::JsValue::from_str(message))
        .is_ok()
}

/// `/`-delimited object path, e.g. `e000007/wal/00000042.log`.
#[cfg(target_arch = "wasm32")]
use sha2::{Digest, Sha256};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;

#[cfg(target_arch = "wasm32")]
mod opfs_lock;

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

/// Origin-private-file-system [`Storage`] for browsers (`wasm32` only).
///
/// Persists objects as real OPFS files under one `oxkv` root directory. The
/// contents survive page reloads. This backend supports the main thread only.
/// Async file handles work on the main thread. Sync access handles are
/// worker-only.
///
/// OPFS offers no conditional-write primitive on the main thread. The code
/// checks the `Create` and `Update` preconditions with a read, then with a
/// write. The read and the write that it guards run inside a named
/// [Web Lock](https://developer.mozilla.org/en-US/docs/Web/API/Web_Locks_API)
/// (see [`opfs_lock`]). Tabs of this origin then serialise per object instead
/// of resolving last-writer-wins. The `delete` method takes the same lock. The
/// `get` method takes no lock. OPFS swaps a file atomically when its writable
/// stream closes.
///
/// If `navigator.locks` is missing (older Safari, no browser), the code skips
/// the guard. The preconditions then degrade to a best-effort read, then write.
/// See [`OpfsStorage::cross_tab_cas_is_atomic`].
///
/// An `ETag` and a version are hex `SHA-256` content hashes. The hashes are
/// stable across reloads. Each hash is unique per byte content. The code uses
/// no sidecar files.
#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
pub struct OpfsStorage {
    root: web_sys::FileSystemDirectoryHandle,
}

/// Hex `SHA-256` of `bytes`. The etag and the version stay stable across
/// reloads.
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

/// Drives a JS promise on the local task queue. The function bridges `!Send`
/// JS futures into the `Send`-required [`Storage`] methods.
///
/// `wasm_bindgen_futures::JsFuture` is `!Send` on single-threaded wasm, so the
/// code can never await it directly here. Instead, the promise runs in a
/// `spawn_local` task. The result crosses back through a `Send` oneshot.
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

    /// Reports whether a Web Lock serialises conditional writes across tabs.
    ///
    /// The value is `true` in any browser with `navigator.locks` (Chrome 69+,
    /// Firefox 96+, Safari 15.4+). In that case, `put_opts` and `delete` check
    /// their preconditions and write while they hold a per-object lock. Tabs of
    /// this origin cannot interleave a write between the read and the write.
    ///
    /// The value is `false` when the preconditions degrade to a best-effort
    /// read, then write. The check stays exact within one tab. A concurrent tab
    /// can lose an update. The LSM layer's single-writer fencing is there to
    /// catch such an update.
    #[must_use]
    pub fn cross_tab_cas_is_atomic() -> bool {
        opfs_lock::locks_available()
    }

    /// Returns the number of [`put_opts`](Storage::put_opts) and `delete` calls
    /// that ran without a Web Lock since this page loaded.
    ///
    /// The count is non-zero exactly when
    /// [`OpfsStorage::cross_tab_cas_is_atomic`] was `false`. The count is the
    /// observable signal that the code exercised the degraded path.
    #[must_use]
    pub fn unlocked_operation_count() -> u64 {
        opfs_lock::unlocked_operation_count()
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

    /// Reads the bytes and the etag. Returns `None` when the object or its
    /// directory is missing.
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

    /// The function checks the precondition and writes for
    /// [`Storage::put_opts`]. The caller already holds the object's Web Lock.
    ///
    /// The function must not take the lock again. Web Locks are not reentrant.
    async fn put_guarded(
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

    /// Deletes the object at `path`. The caller already holds the object's Web
    /// Lock. The lock stops the delete from landing between another tab's
    /// precondition check and its write.
    ///
    /// The function must not take the lock again. Web Locks are not reentrant.
    async fn delete_guarded(&self, path: &ObjectPath) -> Result<()> {
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

    /// Walks the store tree and collects every file key under `prefix`.
    ///
    /// OPFS has no listing API on a path prefix. The walk therefore descends
    /// from the root one directory at a time. `base` is the key prefix of the
    /// directory the walk currently visits. The function keeps a work stack
    /// of unvisited directories. A recursive walk would need boxing for every
    /// level of the tree.
    async fn list_all(&self, prefix: &ObjectPath) -> Result<Vec<ObjectPath>> {
        let mut keys: Vec<ObjectPath> = Vec::new();
        let mut stack: Vec<(web_sys::FileSystemDirectoryHandle, String)> =
            vec![(self.root.clone(), String::new())];
        while let Some((dir, base)) = stack.pop() {
            let entries = dir.entries();
            loop {
                let next = entries.next().map_err(|e| js_err("OPFS list failed", e))?;
                let step = js_await(next)
                    .await
                    .map_err(|e| js_err("OPFS list failed", e))?;
                let done = js_sys::Reflect::get(&step, &wasm_bindgen::JsValue::from_str("done"))
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if done {
                    break;
                }
                let pair = js_sys::Reflect::get(&step, &wasm_bindgen::JsValue::from_str("value"))
                    .map_err(|e| js_err("OPFS list failed", e))?;
                let pair = js_sys::Array::from(&pair);
                let name = pair.get(0).as_string().unwrap_or_default();
                let handle = pair.get(1);
                let key = if base.is_empty() {
                    name.clone()
                } else {
                    format!("{base}/{name}")
                };
                match handle.dyn_into::<web_sys::FileSystemDirectoryHandle>() {
                    Ok(child) => {
                        // Descend only into a directory that can still hold the
                        // prefix. A directory that sits outside the prefix
                        // cannot hold it. A directory above it can.
                        if under_prefix(&key, prefix.as_str()) || prefix.as_str().starts_with(&key)
                        {
                            stack.push((child, key));
                        }
                    }
                    Err(handle) => {
                        if handle.dyn_into::<web_sys::FileSystemFileHandle>().is_err() {
                            return Err(StoreError::Storage(format!(
                                "OPFS entry is neither a file nor a directory: {key}"
                            )));
                        }
                        if under_prefix(&key, prefix.as_str()) {
                            keys.push(ObjectPath::from(key));
                        }
                    }
                }
            }
        }
        Ok(keys)
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
        // The code clones the store handle and the path into the lock callback.
        // A Web Lock callback must be `'static`. The callback outlives this
        // call. The browser's microtask queue drives the callback.
        let lock_path = path.clone();
        let store = self.clone();
        let guarded_path = path.clone();
        opfs_lock::with_object_lock(&lock_path, move || async move {
            store.put_guarded(&guarded_path, payload, mode).await
        })
        .await
    }

    async fn delete(&self, path: &ObjectPath) -> Result<()> {
        let lock_path = path.clone();
        let store = self.clone();
        let guarded_path = path.clone();
        opfs_lock::with_object_lock(&lock_path, move || async move {
            store.delete_guarded(&guarded_path).await
        })
        .await
    }

    async fn list(&self, prefix: &ObjectPath) -> Result<Vec<ObjectPath>> {
        self.list_all(prefix).await
    }
}

/// Contract tests for the OPFS backend. The tests mirror the [`MemStorage`]
/// suite above. The engine speaks only [`Storage`], so every backend must
/// provide the same `Create`, `Update`, `get_opts` and `delete` behaviour.
///
/// These tests need a real browser. OPFS is not available under
/// `wasm-pack test --node`. The tests announce the skip instead of passing
/// vacuously. See [`announce_skip`]. The CI Chrome job
/// (`wasm-pack test --headless --chrome`) executes them.
#[cfg(all(test, target_arch = "wasm32"))]
mod opfs_tests {
    use super::*;
    use oxkv::ObjectVersion;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Opens a store under a prefix of its own. This prevents tests from
    /// colliding with each other. It also prevents collisions with anything
    /// else that a previous run left in OPFS.
    async fn store_for(test: &str) -> Option<OpfsStorage> {
        if web_sys::window().is_none() {
            announce_skip(
                test,
                "no window/OPFS under Node; executed for real by CI's `wasm-pack test --headless --chrome` job",
            );
            return None;
        }
        Some(
            OpfsStorage::open()
                .await
                .unwrap_or_else(|e| panic!("{test}: open OPFS: {e}")),
        )
    }

    fn path(test: &str) -> ObjectPath {
        ObjectPath::new(format!("contract-{test}/object"))
    }

    #[wasm_bindgen_test]
    async fn opfs_contracts_mirror_mem_storage() {
        let Some(store) = store_for("contracts").await else {
            return;
        };
        let path = path("contracts");
        store.delete(&path).await.expect("clean slate");

        let created = store
            .put_opts(&path, b"v1".to_vec(), PutMode::Create)
            .await
            .expect("create");
        assert!(created.e_tag.is_some(), "OPFS must report an etag");
        assert_eq!(created.version, created.e_tag);

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

        let got = store.get(&path).await.expect("get");
        assert_eq!(got.bytes, b"v1", "conflicts must not overwrite");

        let err = store
            .get_opts(
                &path,
                GetOptions {
                    if_none_match: created.e_tag.clone(),
                },
            )
            .await
            .expect_err("etag match is NotModified");
        assert_eq!(err, StoreError::NotModified);

        let updated = store
            .put_opts(
                &path,
                b"v2".to_vec(),
                PutMode::Update(ObjectVersion {
                    e_tag: created.e_tag.clone(),
                    version: created.version.clone(),
                }),
            )
            .await
            .expect("matching update wins");
        assert_ne!(updated.e_tag, created.e_tag, "new content, new etag");
        assert_eq!(store.get(&path).await.expect("get").bytes, b"v2");

        // `list` must report the object it just wrote, and must not report
        // anything below an absent prefix.
        let listed = store
            .list(&ObjectPath::from("contract-contracts/object"))
            .await
            .expect("list");
        assert_eq!(listed, vec![path.clone()]);
        let empty = store
            .list(&ObjectPath::from("contract-contracts/absent"))
            .await
            .expect("list");
        assert!(empty.is_empty(), "an absent prefix lists nothing");

        store.delete(&path).await.expect("delete");
        store.delete(&path).await.expect("delete idempotent");
        let err = store.get(&path).await.expect_err("missing get fails");
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[wasm_bindgen_test]
    async fn opfs_rejects_empty_and_trailing_slash_paths() {
        let Some(store) = store_for("split").await else {
            return;
        };
        for bad in ["", "a/", "a/b/"] {
            let bad = ObjectPath::new(bad);
            assert!(
                store
                    .put_opts(&bad, b"x".to_vec(), PutMode::Create)
                    .await
                    .is_err()
            );
            assert!(store.get(&bad).await.is_err());
            assert!(
                store.delete(&bad).await.is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[wasm_bindgen_test]
    async fn opfs_conflict_inside_the_lock_still_releases_it() {
        let Some(store) = store_for("conflict-release").await else {
            return;
        };
        let path = path("conflict-release");
        store.delete(&path).await.expect("clean slate");
        let created = store
            .put_opts(&path, b"v1".to_vec(), PutMode::Create)
            .await
            .expect("create");

        // A conflict returns from inside the locked section. If that path
        // dropped the lock early, the next operation on this very path would
        // corrupt or hang. If that path kept the lock, the next operation
        // would also corrupt or hang. The next operation must simply work.
        let err = store
            .put_opts(
                &path,
                b"v2".to_vec(),
                PutMode::Update(ObjectVersion {
                    e_tag: Some("\"stale\"".to_string()),
                    version: None,
                }),
            )
            .await
            .expect_err("stale update conflicts");
        assert!(matches!(err, StoreError::CasConflict(_)), "{err:?}");
        let updated = store
            .put_opts(
                &path,
                b"v2".to_vec(),
                PutMode::Update(ObjectVersion {
                    e_tag: created.e_tag,
                    version: None,
                }),
            )
            .await
            .expect("the lock is free again after a conflict");
        assert!(updated.e_tag.is_some());
        store.delete(&path).await.expect("delete");
    }

    #[wasm_bindgen_test]
    async fn opfs_backend_failure_inside_the_lock_releases_it() {
        let Some(store) = store_for("error-release").await else {
            return;
        };
        let blocker = ObjectPath::new("contract-error-release/a");
        let nested = ObjectPath::new("contract-error-release/a/b");
        store.delete(&nested).await.expect("clean slate");
        store.delete(&blocker).await.expect("clean slate");
        store
            .put_opts(&blocker, b"file".to_vec(), PutMode::Create)
            .await
            .expect("create the blocking file");

        // `a` is a file, so the create of the directory `a/` fails from inside
        // the locked section. This is an operational error, not a precondition
        // conflict.
        let err = store
            .put_opts(&nested, b"x".to_vec(), PutMode::Create)
            .await
            .expect_err("a file cannot become a directory");
        assert!(!matches!(err, StoreError::CasConflict(_)), "{err:?}");

        // The lock name is the same as the lock name in the failed call. If the
        // error path had leaked the lock, this call would hang or corrupt. If
        // the error path had released the lock early, this call would also
        // hang or corrupt.
        store
            .delete(&blocker)
            .await
            .expect("delete the blocking file");
        store
            .put_opts(&nested, b"x".to_vec(), PutMode::Create)
            .await
            .expect("the lock is free again after an operational error");
        store.delete(&nested).await.expect("delete");
    }

    #[wasm_bindgen_test]
    async fn opfs_lock_is_available_wherever_opfs_is() {
        // OPFS itself works only in a browser. The Web Lock must be available
        // there too, or `put_opts` is an unguarded read, then write while it
        // claims otherwise. Outside a browser, the host may or may not ship
        // Web Locks. The detection test in `opfs_lock` pins this to
        // `navigator.locks`.
        if web_sys::window().is_none() {
            announce_skip(
                "opfs_lock_is_available_wherever_opfs_is",
                "no OPFS outside a browser; CI's Chrome job asserts the real thing",
            );
            return;
        }
        assert!(
            OpfsStorage::cross_tab_cas_is_atomic(),
            "a browser without Web Locks would make OpfsStorage's CAS best-effort"
        );
    }
}
