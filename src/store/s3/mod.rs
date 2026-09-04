//! S3-backed LSM store.
#![cfg(not(target_arch = "wasm32"))]
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]

use std::sync::Arc;

use async_trait::async_trait;
use object_store::path::Path;
use object_store::{ObjectStore, PutMode, PutPayload};

use crate::store::{Direction, GetSet, KeyValue, Result, Store, StoreError, Transaction};

mod blob;
mod manifest;
mod ownership;
mod probe;
mod sst;

pub(crate) use blob::{
    encode_blob_pointer, get_blob, is_overflow, put_blob, try_decode_blob_pointer,
};
pub(crate) use manifest::{ManifestCache, SstMeta, cas_manifest};
pub(crate) use ownership::{acquire_ownership, cas_backoff, read_ownership, wal_path};
pub(crate) use probe::probe_store;
pub(crate) use sst::{DEFAULT_BLOCK_SIZE, SstFile, build_sst};

#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use blob::blob_path;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use manifest::manifest_path;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use ownership::{epoch_prefix, ownership_path};

type MemMap = std::collections::BTreeMap<String, Option<Vec<u8>>>;
type MemTable = Arc<tokio::sync::RwLock<MemMap>>;
type WalBuffer = Arc<tokio::sync::Mutex<Vec<(String, Option<Vec<u8>>)>>>;

/// S3-backed store.
pub struct S3Store {
    inner: Arc<dyn ObjectStore>,
    prefix: Path,
    epoch: u64,
    session: String,
    mem: MemTable,
    wal_seq: Arc<std::sync::atomic::AtomicU64>,
    wal_buffer: WalBuffer,
    sst_seq: Arc<std::sync::atomic::AtomicU64>,
    manifest_cache: Arc<tokio::sync::Mutex<ManifestCache>>,
    /// Pinned reader versions for WAL GC watermark.
    readers: Arc<tokio::sync::Mutex<std::collections::BTreeMap<u64, usize>>>,
}

impl std::fmt::Debug for S3Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Store")
            .field("prefix", &self.prefix)
            .field("epoch", &self.epoch)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl S3Store {
    #[must_use]
    pub fn builder() -> S3StoreBuilder {
        S3StoreBuilder {
            inner: None,
            prefix: Path::default(),
            skip_probe: false,
            session: None,
        }
    }
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

    pub async fn stage_set(&self, key: &str, value: &[u8]) {
        self.mem
            .write()
            .await
            .insert(key.to_string(), Some(value.to_vec()));
        self.wal_buffer
            .lock()
            .await
            .push((key.to_string(), Some(value.to_vec())));
    }
    pub async fn stage_delete(&self, key: &str) {
        self.mem.write().await.insert(key.to_string(), None);
        self.wal_buffer.lock().await.push((key.to_string(), None));
    }
    pub async fn mem_get(&self, key: &str) -> Option<Option<Vec<u8>>> {
        self.mem.read().await.get(key).cloned()
    }

    pub async fn flush(&self) -> Result<()> {
        let ops: Vec<(String, Option<Vec<u8>>)> = {
            let mut buf = self.wal_buffer.lock().await;
            if buf.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut *buf)
        };
        let mut payload_buf = Vec::new();
        for (key, value) in &ops {
            match value {
                Some(val) => crate::store::encode_record(&mut payload_buf, key, val)
                    .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?,
                None => crate::store::encode_record(&mut payload_buf, key, &[])
                    .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?,
            }
        }
        let seq = self
            .wal_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = wal_path(&self.prefix, self.epoch, seq);
        let put_res = self
            .inner
            .put_opts(&path, PutPayload::from(payload_buf), PutMode::Create.into())
            .await;
        match put_res {
            Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
            Err(e) => return Err(StoreError::Storage(format!("put wal failed: {e}"))),
        }
        let cur = read_ownership(Arc::clone(&self.inner), &self.prefix).await?;
        match cur {
            Some(rec) if rec.epoch == self.epoch && rec.owner_session == self.session => {}
            Some(rec) => {
                return Err(StoreError::Fenced(format!(
                    "fenced: epoch {} session {} superseded by epoch {} session {}",
                    self.epoch, self.session, rec.epoch, rec.owner_session
                )));
            }
            None => {
                return Err(StoreError::Fenced(
                    "fenced: ownership missing after wal put".to_string(),
                ));
            }
        }
        // update manifest wal list
        for _ in 0..4 {
            let mut cache = self.manifest_cache.lock().await;
            let (mut manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            let wal_id = path.to_string();
            if manifest.wal.contains(&wal_id) {
                return Ok(());
            }
            manifest.wal.push(wal_id.clone());
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    cache.update(manifest, new_etag);
                    return Ok(());
                }
                Err(e) if e.to_string().contains("CAS conflict") => {
                    cache.clear();
                    tokio::time::sleep(cas_backoff(0)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub async fn flush_mem_to_sst(&self) -> Result<Option<SstMeta>> {
        self.flush_mem_to_sst_inner(false).await
    }
    pub async fn flush_mem_to_sst_force(&self) -> Result<Option<SstMeta>> {
        self.flush_mem_to_sst_inner(true).await
    }

    #[allow(clippy::too_many_lines)]
    async fn flush_mem_to_sst_inner(&self, force: bool) -> Result<Option<SstMeta>> {
        let snapshot: std::collections::BTreeMap<String, Option<Vec<u8>>> = {
            let mem = self.mem.read().await;
            if mem.is_empty() {
                return Ok(None);
            }
            let est: usize = mem
                .iter()
                .map(|(k, v)| k.len() + v.as_ref().map_or(0, |b| b.len()) + 8)
                .sum();
            if !force && est < 32 * 1024 * 1024 {
                return Ok(None);
            }
            mem.clone()
        };
        let mut sst_entries: std::collections::BTreeMap<String, Option<Vec<u8>>> =
            std::collections::BTreeMap::new();
        for (key, value) in &snapshot {
            match value {
                Some(val) if is_overflow(key, val, DEFAULT_BLOCK_SIZE) => {
                    let blob_path =
                        put_blob(Arc::clone(&self.inner), &self.prefix, self.epoch, val).await?;
                    let crc = crc32fast::hash(val);
                    let ptr = encode_blob_pointer(&blob_path, val.len(), crc);
                    sst_entries.insert(key.clone(), Some(ptr));
                }
                other => {
                    sst_entries.insert(key.clone(), other.clone());
                }
            }
        }
        let sst_bytes = build_sst(&sst_entries, DEFAULT_BLOCK_SIZE)?;
        if sst_bytes.is_empty() {
            return Ok(None);
        }
        let seq = self
            .sst_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let sst_id = format!("e{:06}/sst/L0/{:09}.sst", self.epoch, seq);
        let sst_path = Path::from(sst_id.clone());
        let put_res = self
            .inner
            .put_opts(
                &sst_path,
                PutPayload::from(sst_bytes.clone()),
                PutMode::Create.into(),
            )
            .await;
        match put_res {
            Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
            Err(e) => return Err(StoreError::Storage(format!("put sst failed: {e}"))),
        }
        let cur_owner = read_ownership(Arc::clone(&self.inner), &self.prefix).await?;
        match cur_owner {
            Some(rec) if rec.epoch == self.epoch && rec.owner_session == self.session => {}
            Some(rec) => {
                return Err(StoreError::Fenced(format!(
                    "fenced: epoch {} session {} superseded by epoch {} session {}",
                    self.epoch, self.session, rec.epoch, rec.owner_session
                )));
            }
            None => {
                return Err(StoreError::Fenced(
                    "fenced: ownership missing before manifest CAS".to_string(),
                ));
            }
        }
        let mut cache = self.manifest_cache.lock().await;
        let (mut manifest, etag) = cache
            .load(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                std::time::Duration::from_secs(1),
            )
            .await?;
        if manifest.sst.iter().any(|m| m.id == sst_id) {
            let existing = manifest.sst.iter().find(|m| m.id == sst_id).cloned();
            {
                let mut mem = self.mem.write().await;
                for key in snapshot.keys() {
                    mem.remove(key);
                }
            }
            return Ok(existing);
        }
        let sst_meta = SstMeta {
            id: sst_id.clone(),
            level: 0,
            min_key: sst_entries.keys().next().cloned().unwrap_or_default(),
            max_key: sst_entries.keys().next_back().cloned().unwrap_or_default(),
            size: sst_bytes.len() as u64,
        };
        manifest.sst.push(sst_meta.clone());
        manifest.version = manifest.version.wrapping_add(1);
        let etag_opt = if etag.is_empty() { None } else { Some(etag) };
        match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
            Ok(new_etag) => {
                cache.update(manifest, new_etag);
                {
                    let mut mem = self.mem.write().await;
                    for key in snapshot.keys() {
                        mem.remove(key);
                    }
                }
                Ok(Some(sst_meta))
            }
            Err(e) if e.to_string().contains("CAS conflict") => {
                cache.clear();
                Err(StoreError::Storage(format!("manifest CAS conflict: {e}")))
            }
            Err(e) => Err(e),
        }
    }

    async fn resolve_value(&self, raw: Vec<u8>) -> Result<Vec<u8>> {
        if let Some(ptr) = try_decode_blob_pointer(&raw) {
            let blob_path = Path::from(ptr.blob.clone());
            let bytes = get_blob(Arc::clone(&self.inner), &blob_path).await?;
            if bytes.len() != ptr.len {
                return Err(StoreError::Storage(format!("blob len mismatch")));
            }
            let crc = crc32fast::hash(&bytes);
            if crc != ptr.crc {
                return Err(StoreError::Storage(format!("blob crc mismatch")));
            }
            Ok(bytes)
        } else {
            Ok(raw)
        }
    }

    async fn fetch_sst(&self, id: &str) -> Result<SstFile> {
        let path = Path::from(id.to_string());
        let res = self
            .inner
            .get(&path)
            .await
            .map_err(|e| StoreError::Storage(format!("get sst {id} failed: {e}")))?;
        let bytes = res
            .bytes()
            .await
            .map_err(|e| StoreError::Storage(format!("read sst {id} failed: {e}")))?;
        let sst = SstFile::parse(bytes.to_vec())?;
        Ok(sst)
    }

    pub async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        {
            let mem = self.mem.read().await;
            if let Some(val) = mem.get(key) {
                match val {
                    Some(v) => return Ok(Some(self.resolve_value(v.clone()).await?)),
                    None => return Ok(None),
                }
            }
        }
        let (manifest, _etag) = {
            let mut cache = self.manifest_cache.lock().await;
            cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?
        };
        for meta in manifest.sst.iter().rev() {
            if key < meta.min_key.as_str() || key > meta.max_key.as_str() {
                continue;
            }
            let sst = self.fetch_sst(&meta.id).await?;
            match sst.get_option(key)? {
                Some(Some(raw)) => return Ok(Some(self.resolve_value(raw).await?)),
                Some(None) => return Ok(None),
                None => continue,
            }
        }
        Ok(None)
    }

    pub async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get_bytes(key).await?.is_some())
    }

    pub async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        let mut sources: Vec<Vec<(String, Option<Vec<u8>>)>> = Vec::new();
        {
            let mem = self.mem.read().await;
            let mem_vec: Vec<(String, Option<Vec<u8>>)> =
                mem.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            sources.push(mem_vec);
        }
        let (manifest, _etag) = {
            let mut cache = self.manifest_cache.lock().await;
            cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?
        };
        for meta in manifest.sst.iter().rev() {
            let overlaps = {
                let start = cursor.0.as_deref();
                let end = cursor.1.as_deref();
                let min = meta.min_key.as_str();
                let max = meta.max_key.as_str();
                let after_start = start.is_none_or(|s| max >= s);
                let before_end = end.is_none_or(|e| min <= e);
                after_start && before_end
            };
            if !overlaps && cursor.0.is_some() {
                continue;
            }
            let sst = self.fetch_sst(&meta.id).await?;
            let scan = sst.scan_with_tombstones(None, None, None)?;
            let mut resolved: Vec<(String, Option<Vec<u8>>)> = Vec::with_capacity(scan.len());
            for (key, value) in scan {
                match value {
                    Some(raw) => {
                        let val = self.resolve_value(raw).await?;
                        resolved.push((key, Some(val)));
                    }
                    None => resolved.push((key, None)),
                }
            }
            sources.push(resolved);
        }
        Ok(merged_gets_bytes(sources, limit, direction, cursor))
    }

    pub async fn register_reader(&self, version: u64) {
        let mut readers = self.readers.lock().await;
        *readers.entry(version).or_insert(0) += 1;
    }
    pub async fn unregister_reader(&self, version: u64) {
        let mut readers = self.readers.lock().await;
        if let Some(count) = readers.get_mut(&version) {
            *count -= 1;
            if *count == 0 {
                readers.remove(&version);
            }
        }
    }
    pub async fn min_reader_version(&self) -> Option<u64> {
        let readers = self.readers.lock().await;
        readers.keys().next().copied()
    }
    pub async fn manifest_version(&self) -> Result<u64> {
        let mut cache = self.manifest_cache.lock().await;
        let (manifest, _) = cache
            .load(
                Arc::clone(&self.inner),
                &self.prefix,
                self.epoch,
                std::time::Duration::from_secs(1),
            )
            .await?;
        Ok(manifest.version)
    }
    pub async fn gc_wal(&self) -> Result<usize> {
        let min_version = self.min_reader_version().await;
        for _ in 0..4 {
            let mut cache = self.manifest_cache.lock().await;
            let (mut manifest, etag) = cache
                .load(
                    Arc::clone(&self.inner),
                    &self.prefix,
                    self.epoch,
                    std::time::Duration::from_secs(1),
                )
                .await?;
            if manifest.wal.is_empty() || manifest.sst.is_empty() {
                return Ok(0);
            }
            if let Some(min) = min_version {
                if min < manifest.version {
                    return Ok(0);
                }
            }
            let to_delete = manifest.wal.clone();
            manifest.wal.clear();
            manifest.version = manifest.version.wrapping_add(1);
            let etag_opt = if etag.is_empty() { None } else { Some(etag) };
            match cas_manifest(Arc::clone(&self.inner), &self.prefix, &manifest, etag_opt).await {
                Ok(new_etag) => {
                    cache.update(manifest, new_etag);
                    drop(cache);
                    let mut deleted = 0usize;
                    for wal in &to_delete {
                        let path = Path::from(wal.clone());
                        match self.inner.delete(&path).await {
                            Ok(()) | Err(object_store::Error::NotFound { .. }) => deleted += 1,
                            Err(e) => {
                                return Err(StoreError::Storage(format!(
                                    "delete wal {wal} failed: {e}"
                                )));
                            }
                        }
                    }
                    return Ok(deleted);
                }
                Err(e) if e.to_string().contains("CAS conflict") => {
                    cache.clear();
                    drop(cache);
                    tokio::time::sleep(cas_backoff(0)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(0)
    }
}

/// Transaction for S3Store.
pub struct S3Tx {
    inner: Arc<dyn ObjectStore>,
    prefix: Path,
    epoch: u64,
    session: String,
    mem: MemTable,
    wal_seq: Arc<std::sync::atomic::AtomicU64>,
    wal_buffer: WalBuffer,
    sst_seq: Arc<std::sync::atomic::AtomicU64>,
    manifest_cache: Arc<tokio::sync::Mutex<ManifestCache>>,
    overlay: std::collections::BTreeMap<String, Option<Vec<u8>>>,
}

#[async_trait]
impl GetSet for S3Store {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get_bytes(key).await
    }
    async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get_bytes(key).await?.is_some())
    }
    async fn delete(&mut self, key: &str) -> Result<bool> {
        let prev = self.get_bytes(key).await?;
        let existed = prev.is_some();
        self.mem.write().await.insert(key.to_string(), None);
        self.wal_buffer.lock().await.push((key.to_string(), None));
        self.flush().await?;
        self.flush_mem_to_sst().await?;
        Ok(existed)
    }
    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = self.get_bytes(key).await?;
        self.mem
            .write()
            .await
            .insert(key.to_string(), Some(value.to_vec()));
        self.wal_buffer
            .lock()
            .await
            .push((key.to_string(), Some(value.to_vec())));
        self.flush().await?;
        self.flush_mem_to_sst().await?;
        Ok(prev)
    }
    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.gets_bytes(limit, direction, cursor).await
    }
}

#[async_trait]
impl GetSet for S3Tx {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if let Some(v) = self.overlay.get(key) {
            return Ok(v.clone());
        }
        let mem = self.mem.read().await;
        match mem.get(key) {
            Some(Some(v)) => Ok(Some(v.clone())),
            Some(None) => Ok(None),
            None => Ok(None),
        }
    }
    async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get_bytes(key).await?.is_some())
    }
    async fn delete(&mut self, key: &str) -> Result<bool> {
        let prev = self.get_bytes(key).await?;
        self.overlay.insert(key.to_string(), None);
        Ok(prev.is_some())
    }
    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = self.get_bytes(key).await?;
        self.overlay.insert(key.to_string(), Some(value.to_vec()));
        Ok(prev)
    }
    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        let mut merged = self.overlay.clone();
        for (k, v) in self.mem.read().await.iter() {
            merged.entry(k.clone()).or_insert(v.clone());
        }
        let items: Vec<KeyValue> = merged
            .into_iter()
            .filter_map(|(k, v)| v.map(|val| KeyValue { key: k, value: val }))
            .collect();
        let (start, end) = cursor;
        let limit = limit.map(|l| l as usize).unwrap_or(usize::MAX);
        match direction {
            Direction::Next => {
                if let (Some(s), Some(e)) = (&start, &end) {
                    if s > e {
                        return Ok(Vec::new());
                    }
                }
                Ok(items
                    .into_iter()
                    .filter(|kv| start.as_ref().is_none_or(|s| kv.key >= *s))
                    .filter(|kv| end.as_ref().is_none_or(|e| kv.key <= *e))
                    .take(limit)
                    .collect())
            }
            Direction::Prev => {
                let Some(start) = start else {
                    return Ok(Vec::new());
                };
                if let Some(end) = &end {
                    if start < *end {
                        return Ok(Vec::new());
                    }
                }
                Ok(items
                    .into_iter()
                    .rev()
                    .filter(|kv| kv.key <= start)
                    .filter(|kv| end.as_ref().is_none_or(|e| kv.key >= *e))
                    .take(limit)
                    .collect())
            }
        }
    }
}

#[async_trait]
impl Transaction for S3Tx {
    async fn commit(self) -> Result<()> {
        let mut payload_buf = Vec::new();
        for (k, v) in &self.overlay {
            match v {
                Some(val) => crate::store::encode_record(&mut payload_buf, k, val)
                    .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?,
                None => crate::store::encode_record(&mut payload_buf, k, &[])
                    .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?,
            }
        }
        if !payload_buf.is_empty() {
            let seq = self
                .wal_seq
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let path = wal_path(&self.prefix, self.epoch, seq);
            self.inner
                .put_opts(&path, PutPayload::from(payload_buf), PutMode::Create.into())
                .await
                .map_err(|e| StoreError::Storage(format!("put wal failed: {e}")))?;
            let mut mem = self.mem.write().await;
            for (k, v) in self.overlay {
                mem.insert(k, v);
            }
        }
        Ok(())
    }
    async fn rollback(self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Store for S3Store {
    type Transaction = S3Tx;
    fn begin_tx(&mut self) -> Result<Self::Transaction> {
        Ok(S3Tx {
            inner: Arc::clone(&self.inner),
            prefix: self.prefix.clone(),
            epoch: self.epoch,
            session: self.session.clone(),
            mem: Arc::clone(&self.mem),
            wal_seq: Arc::clone(&self.wal_seq),
            wal_buffer: Arc::clone(&self.wal_buffer),
            sst_seq: Arc::clone(&self.sst_seq),
            manifest_cache: Arc::clone(&self.manifest_cache),
            overlay: std::collections::BTreeMap::new(),
        })
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
            mem: Arc::new(tokio::sync::RwLock::new(std::collections::BTreeMap::new())),
            wal_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            wal_buffer: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            sst_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            manifest_cache: Arc::new(tokio::sync::Mutex::new(ManifestCache::new())),
            readers: Arc::new(tokio::sync::Mutex::new(std::collections::BTreeMap::new())),
        })
    }
}

#[must_use]
pub(crate) fn merge_sources(sources: Vec<Vec<(String, Option<Vec<u8>>)>>) -> Vec<KeyValue> {
    let mut map = std::collections::BTreeMap::new();
    for src in sources {
        for (key, value) in src {
            map.entry(key).or_insert(value);
        }
    }
    map.into_iter()
        .filter_map(|(key, value)| value.map(|v| KeyValue { key, value: v }))
        .collect()
}

#[must_use]
pub(crate) fn merged_gets_bytes(
    sources: Vec<Vec<(String, Option<Vec<u8>>)>>,
    limit: Option<u32>,
    direction: Direction,
    cursor: (Option<String>, Option<String>),
) -> Vec<KeyValue> {
    let merged = merge_sources(sources);
    let (start, end) = cursor;
    let mut filtered: Vec<KeyValue> = match direction {
        Direction::Next => merged
            .into_iter()
            .filter(|kv| {
                if let Some(ref s) = start {
                    if kv.key < *s {
                        return false;
                    }
                }
                if let Some(ref e) = end {
                    if kv.key > *e {
                        return false;
                    }
                }
                true
            })
            .collect(),
        Direction::Prev => {
            if start.is_none() {
                return Vec::new();
            }
            let mut v = merged
                .into_iter()
                .filter(|kv| {
                    if let Some(ref s) = start {
                        if kv.key > *s {
                            return false;
                        }
                    }
                    if let Some(ref e) = end {
                        if kv.key < *e {
                            return false;
                        }
                    }
                    true
                })
                .collect::<Vec<_>>();
            v.reverse();
            v
        }
    };
    if let Some(lim) = limit {
        let lim = lim as usize;
        if filtered.len() > lim {
            filtered.truncate(lim);
        }
    }
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;
    pub(crate) fn new_in_memory() -> Arc<dyn ObjectStore> {
        Arc::new(InMemory::new())
    }
    #[tokio::test]
    async fn probe_ok() {
        let s = new_in_memory();
        S3Store::probe(Arc::clone(&s), &Path::default())
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn wal_and_sst() {
        let store = new_in_memory();
        let s = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("t"))
            .with_session("s")
            .build()
            .await
            .unwrap();
        s.stage_set("k1", b"v1").await;
        s.flush().await.unwrap();
        let large = vec![b'x'; DEFAULT_BLOCK_SIZE];
        s.stage_set("large", &large).await;
        s.flush_mem_to_sst_force().await.unwrap().expect("sst");
        assert_eq!(s.get_bytes("large").await.unwrap().unwrap(), large);
    }
}
