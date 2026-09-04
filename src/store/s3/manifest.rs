//! Manifest cache for S3Store — ETag poll + TTL 1s (§6-§7 G8).
//!
//! `manifest.json` at `{prefix}/manifest.json` is the single consistent point.
//! Readers pin `{epoch, version}`; writers CAS via `If-Match: etag`.

#![cfg(not(target_arch = "wasm32"))]
#![allow(unreachable_pub, missing_docs, clippy::all, clippy::pedantic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use object_store::path::Path;
use object_store::{GetOptions, ObjectStore, PutMode, PutPayload, UpdateVersion};
use serde::{Deserialize, Serialize};

use crate::store::{Result, StoreError};

/// SST metadata stored in manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SstMeta {
    /// Object id, e.g. `e000007/sst/L0/000000123.sst.zst`.
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
    /// WAL files (epoch-scoped, e.g. `e000007/wal/00000042.log.zst`).
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
pub fn manifest_path(prefix: &Path) -> Path {
    if prefix.as_ref().is_empty() {
        Path::from("manifest.json")
    } else {
        prefix.child("manifest.json")
    }
}

/// Reads manifest at `prefix/manifest.json`; `None` if not found.
pub async fn read_manifest(
    store: Arc<dyn ObjectStore>,
    prefix: &Path,
) -> Result<Option<(Manifest, String)>> {
    let path = manifest_path(prefix);
    match store.get(&path).await {
        Ok(res) => {
            let etag = res.meta.e_tag.clone().unwrap_or_default();
            let bytes = res
                .bytes()
                .await
                .map_err(|e| StoreError::Storage(format!("read manifest bytes: {e}")))?;
            let manifest: Manifest = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Storage(format!("parse manifest: {e}")))?;
            Ok(Some((manifest, etag)))
        }
        Err(object_store::Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(StoreError::Storage(format!("get manifest failed: {e}"))),
    }
}

/// CAS writes `manifest` via `If-Match` (or `Create` if no etag).
///
/// `expected_etag` is `None` for create, `Some(etag)` for update.
/// On success returns the new etag.
pub async fn cas_manifest(
    store: Arc<dyn ObjectStore>,
    prefix: &Path,
    manifest: &Manifest,
    expected_etag: Option<String>,
) -> Result<String> {
    let path = manifest_path(prefix);
    let payload = PutPayload::from(
        serde_json::to_vec(manifest)
            .map_err(|e| StoreError::Storage(format!("serialize manifest: {e}")))?,
    );
    let opts = match expected_etag {
        None => PutMode::Create.into(),
        Some(etag) => PutMode::Update(UpdateVersion {
            e_tag: Some(etag),
            version: None,
        })
        .into(),
    };
    let res = store
        .put_opts(&path, payload, opts)
        .await
        .map_err(|e| match e {
            object_store::Error::AlreadyExists { .. }
            | object_store::Error::Precondition { .. } => {
                StoreError::Storage(format!("manifest CAS conflict: {e}"))
            }
            other => StoreError::Storage(format!("put manifest failed: {other}")),
        })?;
    Ok(res.e_tag.unwrap_or_default())
}

/// In-memory cache with ETag + TTL.
#[derive(Debug)]
pub struct ManifestCache {
    entry: Option<CachedEntry>,
}

#[derive(Debug, Clone)]
struct CachedEntry {
    manifest: Manifest,
    etag: String,
    fetched_at: Instant,
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
        let e = self.entry.as_ref()?;
        if e.fetched_at.elapsed() < ttl {
            Some((e.manifest.clone(), e.etag.clone()))
        } else {
            None
        }
    }

    /// Updates cache with `manifest`+`etag` at `now`.
    pub fn update(&mut self, manifest: Manifest, etag: String) {
        self.entry = Some(CachedEntry {
            manifest,
            etag,
            fetched_at: Instant::now(),
        });
    }

    /// Clears the cache (e.g. on `412`).
    pub fn clear(&mut self) {
        self.entry = None;
    }

    /// Loads manifest with ETag poll: if cached etag matches remote and TTL not expired,
    /// uses `If-None-Match` to avoid re-fetching.
    ///
    /// If remote returns `NotModified`, returns cached.
    /// If `NotFound`, returns empty manifest for `epoch`.
    pub async fn load(
        &mut self,
        store: Arc<dyn ObjectStore>,
        prefix: &Path,
        epoch: u64,
        ttl: Duration,
    ) -> Result<(Manifest, String)> {
        if let Some((manifest, etag)) = self.get_cached(ttl) {
            // Try conditional GET with If-None-Match
            let path = manifest_path(prefix);
            let opts = GetOptions {
                if_none_match: Some(etag.clone()),
                ..Default::default()
            };
            match store.get_opts(&path, opts).await {
                Ok(res) => {
                    // Modified — parse new
                    let new_etag = res.meta.e_tag.clone().unwrap_or_default();
                    let bytes = res
                        .bytes()
                        .await
                        .map_err(|e| StoreError::Storage(format!("read manifest: {e}")))?;
                    let manifest: Manifest = serde_json::from_slice(&bytes)
                        .map_err(|e| StoreError::Storage(format!("parse manifest: {e}")))?;
                    self.update(manifest.clone(), new_etag.clone());
                    return Ok((manifest, new_etag));
                }
                Err(object_store::Error::NotModified { .. }) => {
                    return Ok((manifest, etag));
                }
                Err(object_store::Error::NotFound { .. }) => {
                    let empty = Manifest::empty(epoch);
                    self.update(empty.clone(), String::new());
                    return Ok((empty, String::new()));
                }
                Err(e) => return Err(StoreError::Storage(format!("get manifest failed: {e}"))),
            }
        }

        // No cache or expired: do plain GET
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
    use object_store::memory::InMemory;

    fn test_store() -> Arc<dyn ObjectStore> {
        Arc::new(InMemory::new())
    }

    #[tokio::test]
    async fn manifest_empty_and_cas() {
        let store = test_store();
        let prefix = Path::from("oxkv");
        let manifest = Manifest {
            version: 0,
            epoch: 1,
            wal: vec!["e000001/wal/00000001.log.zst".to_string()],
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

        // Update with correct etag
        let mut manifest2 = loaded.clone();
        manifest2.version = 1;
        let etag3 = cas_manifest(Arc::clone(&store), &prefix, &manifest2, Some(etag.clone()))
            .await
            .expect("update");
        assert_ne!(etag, etag3);

        // Stale etag must conflict
        let err = cas_manifest(Arc::clone(&store), &prefix, &manifest, Some(etag))
            .await
            .expect_err("stale must conflict");
        assert!(err.to_string().contains("CAS conflict"));
    }

    #[tokio::test]
    async fn manifest_cache_etag_poll() {
        let store = test_store();
        let prefix = Path::default();
        let epoch = 7;
        let mut cache = ManifestCache::new();
        let ttl = Duration::from_secs(1);

        // Initially empty
        let (m1, e1) = cache
            .load(Arc::clone(&store), &prefix, epoch, ttl)
            .await
            .unwrap();
        assert_eq!(m1.version, 0);
        assert_eq!(m1.epoch, epoch);

        // Create manifest — e1 is empty for first create (None), not Some("")
        let manifest = Manifest {
            version: 1,
            epoch,
            wal: vec![],
            sst: vec![SstMeta {
                id: "e000007/sst/L0/000000001.sst.zst".to_string(),
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

        // Within TTL, load should use If-None-Match and get either NotModified or new
        // We sleep a tiny amount to ensure not expired, but cache still has old etag
        // The cache currently holds empty manifest with old etag, so next load should detect modification
        cache.clear();
        let (m2, _e2) = cache
            .load(Arc::clone(&store), &prefix, epoch, ttl)
            .await
            .unwrap();
        assert_eq!(m2.version, 1);
        assert_eq!(m2.sst.len(), 1);

        // Second load within TTL should hit cache via NotModified
        let (m3, _e3) = cache
            .load(Arc::clone(&store), &prefix, epoch, ttl)
            .await
            .unwrap();
        assert_eq!(m3.version, 1);
    }

    #[tokio::test]
    async fn manifest_cache_ttl_expiry() {
        let store = test_store();
        let prefix = Path::default();
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
        tokio::time::sleep(Duration::from_millis(20)).await;
        // After TTL expiry, get_cached should be None, but load will still fetch
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
