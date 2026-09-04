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

pub(crate) use ownership::{acquire_ownership, read_ownership, wal_path};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use ownership::{epoch_prefix, ownership_path};
pub(crate) use probe::probe_store;

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

    /// Stages `set` into MemTable + WAL buffer.
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

    /// Stages `delete` into MemTable + WAL buffer.
    pub async fn stage_delete(&self, key: &str) {
        self.mem.write().await.insert(key.to_string(), None);
        self.wal_buffer.lock().await.push((key.to_string(), None));
    }

    /// Reads from MemTable (hot path, no S3).
    pub async fn mem_get(&self, key: &str) -> Option<Option<Vec<u8>>> {
        self.mem.read().await.get(key).cloned()
    }

    /// Flushes buffered WAL ops to `e{epoch}/wal/{seq:08}.log` via PutMode::Create, then gates on ownership.
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
                    .map_err(|e| StoreError::Storage(format!("encode wal tombstone: {e}")))?,
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
            Some(rec) if rec.epoch == self.epoch && rec.owner_session == self.session => Ok(()),
            Some(rec) => Err(StoreError::Fenced(format!(
                "fenced: epoch {} session {} superseded by epoch {} session {}",
                self.epoch, self.session, rec.epoch, rec.owner_session
            ))),
            None => Err(StoreError::Fenced(
                "fenced: ownership missing after wal put".to_string(),
            )),
        }
    }

    pub async fn commit_durable_set(&self, key: &str, value: &[u8]) -> Result<()> {
        self.stage_set(key, value).await;
        self.flush().await
    }

    async fn get_bytes_inner(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if let Some(v) = self.mem_get(key).await {
            return Ok(v);
        }
        Ok(None)
    }

    async fn gets_bytes_inner(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        let mem = self.mem.read().await;
        let mut items: Vec<KeyValue> = mem
            .iter()
            .filter_map(|(k, v)| {
                v.as_ref().map(|val| KeyValue {
                    key: k.clone(),
                    value: val.clone(),
                })
            })
            .collect();
        drop(mem);
        let (start, end) = cursor;
        let limit = limit.map(|l| l as usize).unwrap_or(usize::MAX);
        match direction {
            Direction::Next => {
                if let (Some(s), Some(e)) = (&start, &end) {
                    if s > e {
                        return Ok(Vec::new());
                    }
                }
                let res: Vec<KeyValue> = items
                    .into_iter()
                    .filter(|kv| start.as_ref().is_none_or(|s| kv.key >= *s))
                    .filter(|kv| end.as_ref().is_none_or(|e| kv.key <= *e))
                    .take(limit)
                    .collect();
                Ok(res)
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
                let res: Vec<KeyValue> = items
                    .into_iter()
                    .rev()
                    .filter(|kv| kv.key <= start)
                    .filter(|kv| end.as_ref().is_none_or(|e| kv.key >= *e))
                    .take(limit)
                    .collect();
                Ok(res)
            }
        }
    }
}

/// Transaction for S3Store — staged overlay.
pub struct S3Tx {
    inner: Arc<dyn ObjectStore>,
    prefix: Path,
    epoch: u64,
    session: String,
    mem: MemTable,
    wal_seq: Arc<std::sync::atomic::AtomicU64>,
    wal_buffer: WalBuffer,
    overlay: std::collections::BTreeMap<String, Option<Vec<u8>>>,
}

#[async_trait]
impl GetSet for S3Store {
    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get_bytes_inner(key).await
    }
    async fn has(&self, key: &str) -> Result<bool> {
        Ok(self.get_bytes_inner(key).await?.is_some())
    }
    async fn delete(&mut self, key: &str) -> Result<bool> {
        let prev = self.get_bytes_inner(key).await?;
        let existed = prev.is_some();
        self.stage_delete(key).await;
        self.flush().await?;
        Ok(existed)
    }
    async fn set_bytes(&mut self, key: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let prev = self.get_bytes_inner(key).await?;
        self.stage_set(key, value).await;
        self.flush().await?;
        Ok(prev)
    }
    async fn gets_bytes(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        self.gets_bytes_inner(limit, direction, cursor).await
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
            // update mem after durable
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    fn new_in_memory() -> Arc<dyn ObjectStore> {
        Arc::new(InMemory::new())
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

    #[tokio::test]
    async fn wal_flush_empty_is_noop() {
        let s = S3Store::builder()
            .with_store(new_in_memory())
            .with_prefix(Path::from("oxkv"))
            .with_session("node-a")
            .build()
            .await
            .unwrap();
        s.flush().await.expect("empty flush is noop");
    }

    #[tokio::test]
    async fn wal_commit_durable_ok() {
        let store = new_in_memory();
        let s = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv"))
            .with_session("sess-1")
            .build()
            .await
            .unwrap();
        s.stage_set("k1", b"v1").await;
        s.flush().await.expect("wal flush");
        // WAL file should exist
        let wal = wal_path(&Path::from("oxkv"), s.epoch(), 0);
        assert!(store.get(&wal).await.is_ok());
    }

    #[tokio::test]
    async fn store_get_set_via_mem() {
        let mut s = S3Store::builder()
            .with_store(Arc::new(InMemory::new()))
            .with_prefix(Path::from("t"))
            .with_session("s")
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        s.set_bytes("k", b"v").await.unwrap();
        assert_eq!(
            s.get_bytes("k").await.unwrap().as_deref(),
            Some(b"v".as_slice())
        );
    }

    #[tokio::test]
    async fn tx_commit() {
        let mut s = S3Store::builder()
            .with_store(Arc::new(InMemory::new()))
            .with_prefix(Path::from("t2"))
            .with_session("s")
            .skip_probe(true)
            .build()
            .await
            .unwrap();
        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes("a", b"1").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            s.get_bytes("a").await.unwrap().as_deref(),
            Some(b"1".as_slice())
        );
    }
}
