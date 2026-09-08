//! Manifest persisted at `{prefix}/manifest.json`.
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]
//!
//! The manifest is the single consistent point for the store. Writers CAS it
//! with `If-Match: etag`; readers poll `ETag` with a 1 s TTL.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::store::storage::{GetOptions, ObjectPath, ObjectVersion, PutMode, Storage};
use crate::store::{Result, StoreError, now_millis};

/// Metadata for one SST file recorded in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SstMeta {
    /// Object id, e.g. `e000007/sst/L0/000000123.sst`.
    pub id: String,
    /// Level `0` (overlapping) or `1` (non-overlapping).
    pub level: u8,
    /// Minimum key (inclusive).
    #[serde(rename = "minKey")]
    pub min_key: String,
    /// Maximum key (inclusive).
    #[serde(rename = "maxKey")]
    pub max_key: String,
    /// Size in bytes.
    pub size: u64,
}

/// Manifest stored at `{prefix}/manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Monotonic version — bumped on every CAS.
    pub version: u64,
    /// Epoch that owns this manifest.
    pub epoch: u64,
    /// WAL files (epoch-scoped, e.g. `e000007/wal/00000042.log`).
    pub wal: Vec<String>,
    /// SST files.
    pub sst: Vec<SstMeta>,
}

impl Manifest {
    /// Empty manifest for `epoch`.
    #[must_use]
    pub fn empty(epoch: u64) -> Self {
        Self {
            version: 0,
            epoch,
            wal: Vec::new(),
            sst: Vec::new(),
        }
    }
}

/// Path for `manifest.json`.
#[must_use]
pub(crate) fn manifest_path(prefix: &ObjectPath) -> ObjectPath {
    if prefix.is_empty() {
        ObjectPath::from("manifest.json")
    } else {
        prefix.child("manifest.json")
    }
}

/// Reads manifest at `prefix/manifest.json`; `None` if not found.
pub(crate) async fn read_manifest(
    store: Arc<dyn Storage>,
    prefix: &ObjectPath,
) -> Result<Option<(Manifest, String)>> {
    let path = manifest_path(prefix);
    match store.get(&path).await {
        Ok(out) => {
            let etag = out.e_tag.clone().unwrap_or_default();
            let manifest: Manifest = serde_json::from_slice(&out.bytes)
                .map_err(|e| StoreError::Storage(format!("parse manifest: {e}")))?;
            Ok(Some((manifest, etag)))
        }
        Err(e) if e.to_string().contains("not found") => Ok(None),
        Err(e) => Err(StoreError::Storage(format!("get manifest failed: {e}"))),
    }
}

/// CAS writes `manifest` via `If-Match` (or `Create` if no etag).
///
/// `expected_etag` is `None` for create, `Some(etag)` for update.
/// On success returns the new etag.
pub(crate) async fn cas_manifest(
    store: Arc<dyn Storage>,
    prefix: &ObjectPath,
    manifest: &Manifest,
    expected_etag: Option<String>,
) -> Result<String> {
    let path = manifest_path(prefix);
    let payload = serde_json::to_vec(manifest)
        .map_err(|e| StoreError::Storage(format!("serialize manifest: {e}")))?;
    let mode = match expected_etag {
        None => PutMode::Create,
        Some(etag) => PutMode::Update(ObjectVersion {
            e_tag: Some(etag),
            version: None,
        }),
    };
    let out = store.put_opts(&path, payload, mode).await.map_err(|e| {
        if e.to_string().contains("CAS conflict") {
            StoreError::Storage(format!("manifest CAS conflict: {e}"))
        } else {
            StoreError::Storage(format!("put manifest failed: {e}"))
        }
    })?;
    Ok(out.e_tag.unwrap_or_default())
}

/// In-memory cache with `ETag` + TTL.
#[derive(Debug)]
pub(crate) struct ManifestCache {
    entry: Option<CachedEntry>,
}

#[derive(Debug, Clone)]
struct CachedEntry {
    manifest: Manifest,
    etag: String,
    fetched_at_ms: u64,
}

impl ManifestCache {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self { entry: None }
    }

    /// Returns cached manifest if `TTL` not expired.
    #[must_use]
    pub fn get_cached(&self, ttl: Duration) -> Option<(Manifest, String)> {
        let entry = self.entry.as_ref()?;
        if u128::from(now_millis().wrapping_sub(entry.fetched_at_ms)) < ttl.as_millis() {
            Some((entry.manifest.clone(), entry.etag.clone()))
        } else {
            None
        }
    }

    /// Updates cache with `manifest`+`etag` at now.
    pub fn update(&mut self, manifest: Manifest, etag: String) {
        self.entry = Some(CachedEntry {
            manifest,
            etag,
            fetched_at_ms: now_millis(),
        });
    }

    /// Clears the cache (e.g. on `412`).
    pub fn clear(&mut self) {
        self.entry = None;
    }

    /// Loads manifest with `ETag` poll: if cached `etag` matches remote and
    /// `TTL` not expired, uses `If-None-Match` to avoid re-fetching.
    ///
    /// If remote returns `NotModified`, returns cached.
    /// If `NotFound`, returns empty manifest for `epoch`.
    pub async fn load(
        &mut self,
        store: Arc<dyn Storage>,
        prefix: &ObjectPath,
        epoch: u64,
        ttl: Duration,
    ) -> Result<(Manifest, String)> {
        if let Some((manifest, etag)) = self.get_cached(ttl) {
            let path = manifest_path(prefix);
            let opts = GetOptions {
                if_none_match: Some(etag.clone()),
            };
            match store.get_opts(&path, opts).await {
                Ok(out) => {
                    let new_etag = out.e_tag.clone().unwrap_or_default();
                    let manifest: Manifest = serde_json::from_slice(&out.bytes)
                        .map_err(|e| StoreError::Storage(format!("parse manifest: {e}")))?;
                    self.update(manifest.clone(), new_etag.clone());
                    return Ok((manifest, new_etag));
                }
                Err(StoreError::NotModified) => {
                    return Ok((manifest, etag));
                }
                Err(e) if e.to_string().contains("not found") => {
                    let empty = Manifest::empty(epoch);
                    self.update(empty.clone(), String::new());
                    return Ok((empty, String::new()));
                }
                Err(e) => return Err(StoreError::Storage(format!("get manifest failed: {e}"))),
            }
        }

        match read_manifest(Arc::clone(&store), prefix).await? {
            Some((manifest, etag)) => {
                self.update(manifest.clone(), etag.clone());
                Ok((manifest, etag))
            }
            None => {
                let empty = Manifest::empty(epoch);
                self.update(empty.clone(), String::new());
                Ok((empty, String::new()))
            }
        }
    }
}

impl Default for ManifestCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStorage;
    use crate::store::sleep;

    fn test_store() -> Arc<dyn Storage> {
        Arc::new(MemStorage::new())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn manifest_empty_and_cas() {
        let store = test_store();
        let prefix = ObjectPath::from("oxkv");
        let manifest = Manifest {
            version: 0,
            epoch: 1,
            wal: vec!["e000001/wal/00000001.log".to_string()],
            sst: vec![],
        };
        let etag = cas_manifest(Arc::clone(&store), &prefix, &manifest, None)
            .await
            .expect("create");
        assert!(!etag.is_empty());
        let (loaded, etag2) = read_manifest(Arc::clone(&store), &prefix)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, manifest);
        assert_eq!(etag, etag2);

        let mut manifest2 = loaded.clone();
        manifest2.version = 1;
        let etag3 = cas_manifest(Arc::clone(&store), &prefix, &manifest2, Some(etag.clone()))
            .await
            .expect("update");
        assert_ne!(etag, etag3);

        let err = cas_manifest(Arc::clone(&store), &prefix, &manifest, Some(etag))
            .await
            .expect_err("stale must conflict");
        assert!(err.to_string().contains("CAS conflict"));
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn manifest_cache_etag_poll() {
        let store = test_store();
        let prefix = ObjectPath::default();
        let epoch = 7;
        let mut cache = ManifestCache::new();
        let ttl = Duration::from_secs(1);

        let (m1, e1) = cache
            .load(Arc::clone(&store), &prefix, epoch, ttl)
            .await
            .unwrap();
        assert_eq!(m1.version, 0);
        assert_eq!(m1.epoch, epoch);

        let manifest = Manifest {
            version: 1,
            epoch,
            wal: vec![],
            sst: vec![SstMeta {
                id: "e000007/sst/L0/000000001.sst".to_string(),
                level: 0,
                min_key: "a".to_string(),
                max_key: "z".to_string(),
                size: 1024,
            }],
        };
        let create_etag = if e1.is_empty() {
            None
        } else {
            Some(e1.clone())
        };
        cas_manifest(Arc::clone(&store), &prefix, &manifest, create_etag)
            .await
            .unwrap();

        cache.clear();
        let (m2, _e2) = cache
            .load(Arc::clone(&store), &prefix, epoch, ttl)
            .await
            .unwrap();
        assert_eq!(m2.version, 1);
        assert_eq!(m2.sst.len(), 1);

        let (m3, _e3) = cache
            .load(Arc::clone(&store), &prefix, epoch, ttl)
            .await
            .unwrap();
        assert_eq!(m3.version, 1);
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn manifest_cache_ttl_expiry() {
        let store = test_store();
        let prefix = ObjectPath::default();
        let mut cache = ManifestCache::new();
        let ttl = Duration::from_millis(10);
        let epoch = 1;

        let manifest = Manifest {
            version: 5,
            epoch,
            wal: vec![],
            sst: vec![],
        };
        cas_manifest(Arc::clone(&store), &prefix, &manifest, None)
            .await
            .unwrap();
        let (m1, _) = cache
            .load(Arc::clone(&store), &prefix, epoch, ttl)
            .await
            .unwrap();
        assert_eq!(m1.version, 5);
        sleep(Duration::from_millis(20)).await;
        assert!(cache.get_cached(ttl).is_none());
        let (m2, _) = cache
            .load(Arc::clone(&store), &prefix, epoch, ttl)
            .await
            .unwrap();
        assert_eq!(m2.version, 5);
    }

    #[test]
    fn manifest_empty_helper() {
        let m = Manifest::empty(3);
        assert_eq!(m.version, 0);
        assert_eq!(m.epoch, 3);
        assert!(m.wal.is_empty());
    }
}
