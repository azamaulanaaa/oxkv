//! S3-backed LSM store.
#![cfg(not(target_arch = "wasm32"))]
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]

use std::sync::Arc;

use object_store::path::Path;
use object_store::{ObjectStore, PutMode, PutPayload};

use crate::store::{Result, StoreError};

mod ownership;
mod probe;

pub(crate) use ownership::{acquire_ownership, read_ownership, wal_path};
#[cfg(test)]
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
                None => {
                    crate::store::encode_record(&mut payload_buf, key, &[])
                        .map_err(|e| StoreError::Storage(format!("encode wal tombstone: {e}")))?;
                }
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

    pub(crate) fn new_in_memory() -> Arc<dyn ObjectStore> {
        Arc::new(InMemory::new())
    }

    #[tokio::test]
    async fn probe_ok_on_in_memory() {
        let store = new_in_memory();
        S3Store::probe(Arc::clone(&store), &Path::default())
            .await
            .expect("probe must pass");
    }

    #[test]
    fn ownership_path_no_prefix() {
        assert_eq!(ownership_path(&Path::default()).as_ref(), "ownership.json");
    }

    #[test]
    fn manifest_path_and_epoch_prefix_formatting() {
        assert_eq!(epoch_prefix(&Path::default(), 7).as_ref(), "e000007");
        assert_eq!(
            wal_path(&Path::from("oxkv"), 7, 42).as_ref(),
            "oxkv/e000007/wal/00000042.log"
        );
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
}
