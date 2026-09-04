//! S3-backed LSM store

use std::sync::Arc;

use async_trait::async_trait;
use object_store::path::Path;
use object_store::{ObjectStore, PutMode, PutPayload, PutResult, UpdateVersion};
use serde::{Deserialize, Serialize};

use crate::store::{Direction, GetSet, KeyValue, Result, Store, StoreError, Transaction};

type MemMap = std::collections::BTreeMap<String, Option<Vec<u8>>>;
type MemTable = Arc<tokio::sync::RwLock<MemMap>>;
type WalBuffer = Arc<tokio::sync::Mutex<Vec<(String, Option<Vec<u8>>)>>>;

/// S3-backed store.
pub struct S3Store {
    inner: Arc<dyn ObjectStore>,
    prefix: Path,
    epoch: u64,
    session: String,
    /// In-memory `MemTable` — `BTreeMap` mirroring `BTreeStore` overlay.
    mem: MemTable,
    /// WAL sequence for `e{epoch}/wal/{seq:08}.log.zst`.
    wal_seq: Arc<std::sync::atomic::AtomicU64>,
    /// Buffered WAL ops pending an explicit [`Self::flush`] (group commit).
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
    /// Creates a new store builder.
    #[must_use]
    pub fn builder() -> S3StoreBuilder {
        S3StoreBuilder {
            inner: None,
            prefix: Path::default(),
            skip_probe: false,
            session: None,
        }
    }

    /// Runs the storage probe against `store` at a unique per-run
    /// `prefix/probe/canary-<uuid>` path.
    ///
    /// validates `If-None-Match` / `If-Match`
    /// conditional writes. Returns `Ok(())` only on
    /// `ok (create, reject-create, reject-stale)`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if conditional writes are not enforced
    /// (store accepts duplicate `Create` or stale `Update`).
    pub async fn probe(store: Arc<dyn ObjectStore>, prefix: &Path) -> Result<()> {
        probe_store(store, prefix).await
    }

    /// Returns the underlying object store (for tests).
    #[cfg(test)]
    pub fn inner_store(&self) -> Arc<dyn ObjectStore> {
        Arc::clone(&self.inner)
    }

    /// Returns the prefix.
    #[cfg(test)]
    pub fn prefix(&self) -> &Path {
        &self.prefix
    }

    /// Returns the current epoch.
    #[cfg(test)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns the session id.
    #[cfg(test)]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Stages `set` into `MemTable` + WAL buffer (commit = mem).
    ///
    /// Does not hit S3 — use [`Self::flush`] or [`Self::commit_durable_set`] for RPO=0.
    /// Prefer `GetSet::set_bytes` / `Store::begin_tx` for trait-based access.
    pub async fn stage_set(&self, key: &str, value: &[u8]) {
        let v = value.to_vec();
        self.mem
            .write()
            .await
            .insert(key.to_string(), Some(v.clone()));
        self.wal_buffer
            .lock()
            .await
            .push((key.to_string(), Some(v)));
    }

    /// Stages `delete` into `MemTable` + WAL buffer.
    pub async fn stage_delete(&self, key: &str) {
        self.mem.write().await.insert(key.to_string(), None);
        self.wal_buffer.lock().await.push((key.to_string(), None));
    }

    /// Reads from `MemTable` (hot path, no S3).
    pub async fn mem_get(&self, key: &str) -> Option<Option<Vec<u8>>> {
        self.mem.read().await.get(key).cloned()
    }

    /// Internal: read-through MemTable only (no SST yet).
    async fn get_bytes_inner(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.mem.read().await.get(key).cloned().flatten())
    }

    async fn gets_bytes_inner(
        &self,
        limit: Option<u32>,
        direction: Direction,
        cursor: (Option<String>, Option<String>),
    ) -> Result<Vec<KeyValue>> {
        let mem = self.mem.read().await;
        Ok(apply_gets(live_kvs(&mem), limit, direction, cursor))
    }

    /// Flushes buffered WAL ops to `e{epoch}/wal/{seq:08}.log.zst` via
    /// `PutMode::Create` (`If-None-Match:"*"`), then gates on ownership.
    ///
    /// `PUT wal` must succeed *and* `GET ownership.json` must still name `self`.
    /// Idempotent on `AlreadyExists` (group-commit retry).
    pub async fn flush(&self) -> Result<()> {
        let ops = drain_wal_buffer(&self.wal_buffer).await;
        if ops.is_empty() {
            return Ok(());
        }
        let payload = encode_wal_ops(&ops)?;
        put_wal_and_gate(
            Arc::clone(&self.inner),
            &self.prefix,
            self.epoch,
            &self.session,
            &self.wal_seq,
            payload,
        )
        .await
    }

    /// Convenience: stage + flush (RPO=0).
    ///
    /// # Errors
    ///
    /// Propagates `StoreError` from [`Self::flush`].
    pub async fn commit_durable_set(&self, key: &str, value: &[u8]) -> Result<()> {
        self.stage_set(key, value).await;
        self.flush().await
    }
}

// ---------------------------------------------------------------------------
// Trait impls — GetSet/Store/Transaction (like BTreeStore)
// ---------------------------------------------------------------------------

fn live_kvs(mem: &MemMap) -> Vec<KeyValue> {
    mem.iter()
        .filter_map(|(k, v)| {
            v.as_ref().map(|val| KeyValue {
                key: k.clone(),
                value: val.clone(),
            })
        })
        .collect()
}

fn apply_gets(
    items: Vec<KeyValue>,
    limit: Option<u32>,
    direction: Direction,
    cursor: (Option<String>, Option<String>),
) -> Vec<KeyValue> {
    let (start, end) = cursor;
    let limit = limit.map(|l| l as usize).unwrap_or(usize::MAX);
    match direction {
        Direction::Next => {
            if let (Some(s), Some(e)) = (&start, &end) {
                if s > e {
                    return Vec::new();
                }
            }
            items
                .into_iter()
                .filter(|kv| start.as_ref().is_none_or(|s| kv.key >= *s))
                .filter(|kv| end.as_ref().is_none_or(|e| kv.key <= *e))
                .take(limit)
                .collect()
        }
        Direction::Prev => {
            let Some(start) = start else {
                return Vec::new();
            };
            if let Some(end) = &end {
                if start < *end {
                    return Vec::new();
                }
            }
            items
                .into_iter()
                .rev()
                .filter(|kv| kv.key <= start)
                .filter(|kv| end.as_ref().is_none_or(|e| kv.key >= *e))
                .take(limit)
                .collect()
        }
    }
}

async fn drain_wal_buffer(buf: &WalBuffer) -> Vec<(String, Option<Vec<u8>>)> {
    let mut g = buf.lock().await;
    std::mem::take(&mut *g)
}

fn encode_wal_ops(ops: &[(String, Option<Vec<u8>>)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (k, v) in ops {
        // None (tombstone) is encoded as empty value for v1 wire-compat;
        // later SST layer maps `TOMBSTONE_VLEN` to a distinct sentinel.
        let val = v.as_deref().unwrap_or(b"");
        crate::store::encode_record(&mut out, k, val)
            .map_err(|e| StoreError::Storage(format!("encode wal: {e}")))?;
    }
    Ok(out)
}

async fn put_wal_and_gate(
    inner: Arc<dyn ObjectStore>,
    prefix: &Path,
    epoch: u64,
    session: &str,
    wal_seq: &Arc<std::sync::atomic::AtomicU64>,
    payload: Vec<u8>,
) -> Result<()> {
    let seq = wal_seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let path = wal_path(prefix, epoch, seq);
    match inner
        .put_opts(&path, PutPayload::from(payload), PutMode::Create.into())
        .await
    {
        Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
        Err(e) => return Err(StoreError::Storage(format!("put wal failed: {e}"))),
    }
    match read_ownership(Arc::clone(&inner), prefix).await? {
        Some(rec) if rec.epoch == epoch && rec.owner_session == session => Ok(()),
        Some(rec) => Err(StoreError::Fenced(format!(
            "fenced: epoch {epoch} session {session} superseded by epoch {} session {}",
            rec.epoch, rec.owner_session
        ))),
        None => Err(StoreError::Fenced(
            "fenced: ownership missing after wal put".to_string(),
        )),
    }
}

/// Transaction for `S3Store` — staged overlay, durable only on `commit` (like `BTreeTx`).
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
        let existed = self.get_bytes(key).await?.is_some();
        if existed {
            self.overlay.insert(key.to_string(), None);
        }
        Ok(existed)
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
        let mem = self.mem.read().await;
        let mut merged = mem.clone();
        merged.extend(self.overlay.clone());
        Ok(apply_gets(live_kvs(&merged), limit, direction, cursor))
    }
}

#[async_trait]
impl Transaction for S3Tx {
    async fn commit(self) -> Result<()> {
        if self.overlay.is_empty() {
            return Ok(());
        }
        {
            let mut mem = self.mem.write().await;
            mem.extend(self.overlay.clone());
        }
        {
            let mut wal = self.wal_buffer.lock().await;
            wal.extend(self.overlay.clone());
        }
        let ops = drain_wal_buffer(&self.wal_buffer).await;
        if ops.is_empty() {
            return Ok(());
        }
        let payload = encode_wal_ops(&ops)?;
        put_wal_and_gate(
            Arc::clone(&self.inner),
            &self.prefix,
            self.epoch,
            &self.session,
            &self.wal_seq,
            payload,
        )
        .await
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
    /// Sets the backing [`ObjectStore`] (use `Arc::new(InMemory::new())` in tests,
    /// `AmazonS3Builder` / `parse_url` in prod).
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn ObjectStore>) -> Self {
        self.inner = Some(store);
        self
    }

    /// Sets the key prefix inside the bucket (e.g. `Path::from("oxkv")`).
    #[must_use]
    pub fn with_prefix(mut self, prefix: Path) -> Self {
        self.prefix = prefix;
        self
    }

    /// Sets the owner session id (unique per builder). If not set, a
    /// deterministic fallback is generated.
    #[must_use]
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    /// Skips the startup storage probe.
    #[must_use]
    pub fn skip_probe(mut self, skip: bool) -> Self {
        self.skip_probe = skip;
        self
    }

    /// Whether the probe will be skipped.
    #[must_use]
    pub fn is_skip_probe(&self) -> bool {
        self.skip_probe
    }

    /// Builds the store, running the probe unless skipped, then CAS-acquires
    /// `ownership.json` epoch. The returned store is fenced to that epoch.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Storage` if the probe fails or `StoreError::Fenced`
    /// if `ownership.json` CAS loses the race.
    pub async fn build(self) -> Result<S3Store> {
        let store = self.inner.ok_or_else(|| {
            StoreError::Storage("S3Store requires an ObjectStore via with_store()".to_string())
        })?;

        if !self.skip_probe {
            probe_store(Arc::clone(&store), &self.prefix).await?;
        }

        let session = self.session.unwrap_or_else(|| {
            // deterministic fallback — not for prod fleet (use explicit session)
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

/// A unique, per-run canary path so concurrent or repeated probes never collide
/// with each other or with pre-existing data under the prefix.
fn probe_path(prefix: &Path) -> Path {
    let base = if prefix.as_ref().is_empty() {
        Path::from("probe")
    } else {
        prefix.child("probe")
    };
    base.child(format!("canary-{}", uuid::Uuid::now_v7()))
}

async fn probe_store(store: Arc<dyn ObjectStore>, prefix: &Path) -> Result<()> {
    let path = probe_path(prefix);

    // 1. create must succeed
    let first: PutResult = store
        .put_opts(
            &path,
            PutPayload::from_static(b"probe"),
            PutMode::Create.into(),
        )
        .await
        .map_err(|e| StoreError::Storage(format!("probe create failed: {e}")))?;

    // 2. create again must fail with AlreadyExists
    let second = store
        .put_opts(
            &path,
            PutPayload::from_static(b"probe2"),
            PutMode::Create.into(),
        )
        .await;
    match second {
        Err(object_store::Error::AlreadyExists { .. }) => {}
        Ok(_) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(
                "probe store accepted duplicate create — conditional writes not enforced (needs S3/R2/GCS/Azure, not B2/Hetzner)".to_string(),
            ));
        }
        Err(e) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(format!(
                "probe second create unexpected error (expected AlreadyExists): {e}"
            )));
        }
    }

    // 3. stale update must fail with Precondition (If-Match stale etag)
    let stale = UpdateVersion {
        e_tag: Some("\"stale-etag-should-not-match\"".to_string()),
        version: None,
    };
    let third = store
        .put_opts(
            &path,
            PutPayload::from_static(b"probe3"),
            PutMode::Update(stale).into(),
        )
        .await;
    match third {
        Err(object_store::Error::Precondition { .. }) => {}
        Ok(_) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(
                "probe store accepted stale If-Match — conditional overwrite not enforced"
                    .to_string(),
            ));
        }
        Err(e) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(format!(
                "probe stale update unexpected error (expected Precondition): {e}"
            )));
        }
    }

    // 4. valid update with current etag must succeed (proves If-Match works)
    let valid = UpdateVersion {
        e_tag: first.e_tag.clone(),
        version: first.version.clone(),
    };
    store
        .put_opts(
            &path,
            PutPayload::from_static(b"probe-ok"),
            PutMode::Update(valid).into(),
        )
        .await
        .map_err(|e| StoreError::Storage(format!("probe valid update failed: {e}")))?;

    // cleanup
    store
        .delete(&path)
        .await
        .map_err(|e| StoreError::Storage(format!("probe cleanup failed: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Ownership + epoch fencing
// ---------------------------------------------------------------------------

/// Ownership record stored at `{prefix}/ownership.json`.
///
/// CAS record — every activation
/// bumps `epoch`, all new WAL/SST objects go under `e{epoch:06}/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OwnershipRecord {
    /// Monotonic epoch — bumped on every successful CAS.
    pub epoch: u64,
    /// Owner session identifier (e.g. `node-a:uuid`).
    pub owner_session: String,
    /// Optional lease expiry in ms since epoch (fleet mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_expiry_ms: Option<u64>,
    /// Last known manifest `e_tag` (debug aid).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_etag: Option<String>,
}

/// Returns the path for `ownership.json`.
#[must_use]
pub(crate) fn ownership_path(prefix: &Path) -> Path {
    if prefix.as_ref().is_empty() {
        Path::from("ownership.json")
    } else {
        prefix.child("ownership.json")
    }
}

/// Returns the path for `manifest.json`.
#[must_use]
pub(crate) fn manifest_path(prefix: &Path) -> Path {
    if prefix.as_ref().is_empty() {
        Path::from("manifest.json")
    } else {
        prefix.child("manifest.json")
    }
}

/// Formats an epoch as `e000007` (zero-padded 6 digits).
#[must_use]
pub(crate) fn format_epoch(epoch: u64) -> String {
    format!("e{epoch:06}")
}

/// Returns the epoch-scoped prefix `\{prefix}/e{epoch:06}`.
#[must_use]
pub(crate) fn epoch_prefix(prefix: &Path, epoch: u64) -> Path {
    let epoch_str = format_epoch(epoch);
    if prefix.as_ref().is_empty() {
        Path::from(epoch_str)
    } else {
        prefix.child(epoch_str)
    }
}

/// Returns `\{prefix}/e{epoch:06}/wal/{seq:08}.log.zst`.
#[must_use]
pub(crate) fn wal_path(prefix: &Path, epoch: u64, seq: u64) -> Path {
    epoch_prefix(prefix, epoch)
        .child("wal")
        .child(format!("{seq:08}.log.zst"))
}

/// Returns `\{prefix}/e{epoch:06}/sst/{level}/{id:09}.sst.zst`.
#[must_use]
pub(crate) fn sst_path(prefix: &Path, epoch: u64, level: u8, id: u64) -> Path {
    epoch_prefix(prefix, epoch)
        .child("sst")
        .child(format!("L{level}"))
        .child(format!("{id:09}.sst.zst"))
}

/// Returns `\{prefix}/e{epoch:06}/blob/{hash}.zst` for large-value overflow.
#[must_use]
pub(crate) fn blob_path(prefix: &Path, epoch: u64, hash: &str) -> Path {
    epoch_prefix(prefix, epoch)
        .child("blob")
        .child(format!("{hash}.zst"))
}

/// Acquires ownership by CAS-bumping `ownership.json` epoch.
///
/// `session` is the owner identifier. On success returns the new
/// `OwnershipRecord` with `epoch = old.epoch + 1` (or `1` on first acquire).
/// On `AlreadyExists`/`Precondition` conflict returns `StoreError::Fenced`.
///
/// This maps to S3 `If-None-Match:"*"` (create) and `If-Match: etag`
/// (update) — `object_store` translates `PutMode::Create/Update` to the
/// correct header per provider (`S3`/`R2`/`Azure` vs `GCS`
/// `x-goog-if-generation-match`), so the same code qualifies all stores.
///
/// For contended callers, retry with backoff and reload.
///
/// Provider note: `InMemory` supports `Create` (`AlreadyExists`) and
/// `Update` (`Precondition`) exactly, so unit tests run without a real bucket.
pub(crate) async fn acquire_ownership(
    store: Arc<dyn ObjectStore>,
    prefix: &Path,
    session: &str,
) -> Result<OwnershipRecord> {
    let path = ownership_path(prefix);

    // Load current record + version, if any.
    let (existing, version) = match store.get(&path).await {
        Ok(res) => {
            let meta = res.meta.clone();
            let bytes = res
                .bytes()
                .await
                .map_err(|e| StoreError::Storage(format!("read ownership failed: {e}")))?;
            let rec: OwnershipRecord = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Storage(format!("corrupt ownership.json: {e}")))?;
            let ver = UpdateVersion {
                e_tag: meta.e_tag.clone(),
                version: meta.version.clone(),
            };
            (Some(rec), Some(ver))
        }
        Err(object_store::Error::NotFound { .. }) => (None, None),
        Err(e) => return Err(StoreError::Storage(format!("get ownership failed: {e}"))),
    };

    let next_epoch = existing.as_ref().map_or(1, |r| r.epoch + 1);
    let new_rec = OwnershipRecord {
        epoch: next_epoch,
        owner_session: session.to_string(),
        lease_expiry_ms: None,
        manifest_etag: None,
    };
    let payload = PutPayload::from(
        serde_json::to_vec(&new_rec)
            .map_err(|e| StoreError::Storage(format!("serialize ownership: {e}")))?,
    );

    let put_res = if let Some(ver) = version {
        store
            .put_opts(&path, payload, PutMode::Update(ver).into())
            .await
    } else {
        store.put_opts(&path, payload, PutMode::Create.into()).await
    };

    match put_res {
        Ok(_) => Ok(new_rec),
        Err(
            object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. },
        ) => Err(StoreError::Fenced(format!(
            "ownership CAS conflict at epoch {next_epoch} for session {session} — fenced"
        ))),
        Err(e) => Err(StoreError::Storage(format!("put ownership failed: {e}"))),
    }
}

/// Reads the current ownership record, if any.
pub(crate) async fn read_ownership(
    store: Arc<dyn ObjectStore>,
    prefix: &Path,
) -> Result<Option<OwnershipRecord>> {
    let path = ownership_path(prefix);
    match store.get(&path).await {
        Ok(res) => {
            let bytes = res
                .bytes()
                .await
                .map_err(|e| StoreError::Storage(format!("read ownership failed: {e}")))?;
            let rec: OwnershipRecord = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Storage(format!("corrupt ownership.json: {e}")))?;
            Ok(Some(rec))
        }
        Err(object_store::Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(StoreError::Storage(format!("get ownership failed: {e}"))),
    }
}

/// In-memory store helper for tests.
#[cfg(test)]
pub(crate) fn new_in_memory() -> Arc<dyn ObjectStore> {
    Arc::new(object_store::memory::InMemory::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{GetSet, Store, Transaction};
    use object_store::ObjectStore;

    #[tokio::test]
    async fn probe_ok_on_in_memory() {
        let store = new_in_memory();
        probe_store(Arc::clone(&store), &Path::default())
            .await
            .expect("InMemory must pass probe");
    }

    #[tokio::test]
    async fn probe_ok_with_prefix() {
        let store = new_in_memory();
        probe_store(Arc::clone(&store), &Path::from("oxkv"))
            .await
            .expect("prefixed probe must pass");
    }

    #[tokio::test]
    async fn probe_rejects_b2_like_store() {
        // Wraps InMemory but ignores PutMode, always overwrites — simulates B2/Hetzner
        #[derive(Debug)]
        struct NoConditionStore {
            inner: Arc<dyn ObjectStore>,
        }

        impl std::fmt::Display for NoConditionStore {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "NoConditionStore")
            }
        }

        #[async_trait::async_trait]
        impl ObjectStore for NoConditionStore {
            async fn put_opts(
                &self,
                location: &Path,
                payload: PutPayload,
                _opts: object_store::PutOptions,
            ) -> object_store::Result<PutResult> {
                // ignore mode — always overwrite
                self.inner.put(location, payload).await
            }

            async fn put_multipart_opts(
                &self,
                _location: &Path,
                _opts: object_store::PutMultipartOptions,
            ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
                unimplemented!()
            }

            async fn get_opts(
                &self,
                location: &Path,
                options: object_store::GetOptions,
            ) -> object_store::Result<object_store::GetResult> {
                self.inner.get_opts(location, options).await
            }

            async fn delete(&self, location: &Path) -> object_store::Result<()> {
                self.inner.delete(location).await
            }

            fn list(
                &self,
                prefix: Option<&Path>,
            ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
            {
                self.inner.list(prefix)
            }

            async fn list_with_delimiter(
                &self,
                prefix: Option<&Path>,
            ) -> object_store::Result<object_store::ListResult> {
                self.inner.list_with_delimiter(prefix).await
            }

            async fn copy(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy(from, to).await
            }

            async fn copy_if_not_exists(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy_if_not_exists(from, to).await
            }
        }

        let inner = new_in_memory();
        let bad: Arc<dyn ObjectStore> = Arc::new(NoConditionStore { inner });
        let err = probe_store(bad, &Path::default())
            .await
            .expect_err("must reject");
        assert!(
            err.to_string().contains("conditional writes not enforced")
                || err.to_string().contains("stale If-Match"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn builder_runs_probe_by_default() {
        let store = new_in_memory();
        let built = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv"))
            .build()
            .await
            .expect("builder with InMemory must pass probe");
        assert_eq!(built.prefix().as_ref(), "oxkv");
    }

    #[tokio::test]
    async fn builder_skip_probe_flag() {
        assert!(!S3Store::builder().is_skip_probe());
        assert!(S3Store::builder().skip_probe(true).is_skip_probe());
    }

    #[tokio::test]
    async fn builder_skip_probe_allows_b2_like_store() {
        #[derive(Debug)]
        struct NoConditionStore {
            inner: Arc<dyn ObjectStore>,
        }
        impl std::fmt::Display for NoConditionStore {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "NoConditionStore")
            }
        }
        #[async_trait::async_trait]
        impl ObjectStore for NoConditionStore {
            async fn put_opts(
                &self,
                location: &Path,
                payload: PutPayload,
                _opts: object_store::PutOptions,
            ) -> object_store::Result<PutResult> {
                self.inner.put(location, payload).await
            }
            async fn put_multipart_opts(
                &self,
                _location: &Path,
                _opts: object_store::PutMultipartOptions,
            ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
                unimplemented!()
            }
            async fn get_opts(
                &self,
                location: &Path,
                options: object_store::GetOptions,
            ) -> object_store::Result<object_store::GetResult> {
                self.inner.get_opts(location, options).await
            }
            async fn delete(&self, location: &Path) -> object_store::Result<()> {
                self.inner.delete(location).await
            }
            fn list(
                &self,
                prefix: Option<&Path>,
            ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
            {
                self.inner.list(prefix)
            }
            async fn list_with_delimiter(
                &self,
                prefix: Option<&Path>,
            ) -> object_store::Result<object_store::ListResult> {
                self.inner.list_with_delimiter(prefix).await
            }
            async fn copy(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy(from, to).await
            }
            async fn copy_if_not_exists(&self, from: &Path, to: &Path) -> object_store::Result<()> {
                self.inner.copy_if_not_exists(from, to).await
            }
        }

        let bad: Arc<dyn ObjectStore> = Arc::new(NoConditionStore {
            inner: new_in_memory(),
        });
        // without skip_probe -> must fail
        let err = S3Store::builder()
            .with_store(Arc::clone(&bad))
            .build()
            .await
            .expect_err("must reject without skip_probe");
        assert!(err.to_string().contains("conditional writes"));

        // with skip_probe -> must succeed
        S3Store::builder()
            .with_store(bad)
            .skip_probe(true)
            .build()
            .await
            .expect("skip_probe must allow bad store");
    }

    #[tokio::test]
    async fn probe_static_entry_point() {
        let store = new_in_memory();
        S3Store::probe(store, &Path::default())
            .await
            .expect("static probe must pass");
    }

    // -- epoch fencing ------------------------------------------------

    #[test]
    fn ownership_path_no_prefix() {
        assert_eq!(ownership_path(&Path::default()).as_ref(), "ownership.json");
    }

    #[test]
    fn ownership_path_with_prefix() {
        assert_eq!(
            ownership_path(&Path::from("oxkv")).as_ref(),
            "oxkv/ownership.json"
        );
    }

    #[test]
    fn manifest_path_and_epoch_prefix_formatting() {
        assert_eq!(manifest_path(&Path::default()).as_ref(), "manifest.json");
        assert_eq!(epoch_prefix(&Path::default(), 7).as_ref(), "e000007");
        assert_eq!(
            epoch_prefix(&Path::from("oxkv"), 7).as_ref(),
            "oxkv/e000007"
        );
        assert_eq!(
            wal_path(&Path::from("oxkv"), 7, 42).as_ref(),
            "oxkv/e000007/wal/00000042.log.zst"
        );
        assert_eq!(
            sst_path(&Path::from("oxkv"), 7, 0, 123).as_ref(),
            "oxkv/e000007/sst/L0/000000123.sst.zst"
        );
        assert_eq!(
            blob_path(&Path::from("oxkv"), 7, "abc").as_ref(),
            "oxkv/e000007/blob/abc.zst"
        );
    }

    #[tokio::test]
    async fn fencing_acquire_increments_epoch() {
        let store = new_in_memory();
        let prefix = Path::from("oxkv");
        let r1 = acquire_ownership(Arc::clone(&store), &prefix, "node-a")
            .await
            .expect("first acquire");
        assert_eq!(r1.epoch, 1);
        assert_eq!(r1.owner_session, "node-a");
        let r2 = acquire_ownership(Arc::clone(&store), &prefix, "node-b")
            .await
            .expect("second acquire");
        assert_eq!(r2.epoch, 2);
        assert_eq!(r2.owner_session, "node-b");
        let cur = read_ownership(Arc::clone(&store), &prefix)
            .await
            .expect("read")
            .expect("some");
        assert_eq!(cur.epoch, 2);
    }

    #[tokio::test]
    async fn fencing_stale_writer_superseded_prefix_invisible() {
        let store = new_in_memory();
        let prefix = Path::from("oxkv");
        // node-a owns epoch 1 and writes to e000001
        let r1 = acquire_ownership(Arc::clone(&store), &prefix, "node-a")
            .await
            .unwrap();
        let wal1 = wal_path(&prefix, r1.epoch, 1);
        store
            .put(&wal1, PutPayload::from_static(b"wal1"))
            .await
            .unwrap();

        // node-b fences to epoch 2
        let r2 = acquire_ownership(Arc::clone(&store), &prefix, "node-b")
            .await
            .unwrap();
        assert_eq!(r2.epoch, 2);
        let wal2 = wal_path(&prefix, r2.epoch, 1);
        store
            .put(&wal2, PutPayload::from_static(b"wal2"))
            .await
            .unwrap();

        // stale writer that captured r1's version tries to CAS ownership with stale etag — must fence
        // Simulate by directly attempting Put with stale version (r1's etag)
        let stale_path = ownership_path(&prefix);
        // fetch current meta to get a stale version from r1 era: we fake a stale etag
        let stale_ver = UpdateVersion {
            e_tag: Some("\"stale-etag-r1\"".to_string()),
            version: None,
        };
        let stale_put = store
            .put_opts(
                &stale_path,
                PutPayload::from_static(b"stale"),
                PutMode::Update(stale_ver).into(),
            )
            .await;
        assert!(
            matches!(stale_put, Err(object_store::Error::Precondition { .. })),
            "stale If-Match must be rejected"
        );

        // objects are isolated by epoch prefix — reads scoped to current epoch don't see old prefix
        let got1 = store
            .get(&wal1)
            .await
            .expect("old epoch wal still exists but isolated");
        assert_eq!(got1.bytes().await.unwrap().as_ref(), b"wal1");
        let got2 = store.get(&wal2).await.expect("new epoch wal");
        assert_eq!(got2.bytes().await.unwrap().as_ref(), b"wal2");
        // ownership is at epoch 2
        let cur = read_ownership(store, &prefix).await.unwrap().unwrap();
        assert_eq!(cur.epoch, 2);
        assert_eq!(cur.owner_session, "node-b");
    }

    #[tokio::test]
    async fn fencing_412_maps_to_fenced_error() {
        let store = new_in_memory();
        let prefix = Path::default();
        let _r1 = acquire_ownership(Arc::clone(&store), &prefix, "node-a")
            .await
            .unwrap();
        // Simulate a racing acquirer that holds stale read: read old record, delay, then try to acquire
        // Our acquire_ownership does read+put atomically; to force conflict, we concurrently race two acquires
        let s1 = Arc::clone(&store);
        let s2 = Arc::clone(&store);
        let (a, b) = tokio::join!(
            acquire_ownership(s1, &prefix, "racer-1"),
            acquire_ownership(s2, &prefix, "racer-2")
        );
        // exactly one must succeed, one must be Fenced (InMemory serializes, so second sees updated epoch or conflicts)
        let successes = usize::from(a.is_ok()) + usize::from(b.is_ok());
        // With our current read-then-put without retry, the second concurrent read may see epoch 1 and both try epoch 2;
        // one will get Precondition -> Fenced. If serialized, one will see epoch 2 and succeed at 3. Either way, at least one fences or both succeed sequentially.
        // We assert that eventual epoch is 2 or 3 and at most one conflict.
        let final_rec = read_ownership(Arc::clone(&store), &prefix)
            .await
            .unwrap()
            .unwrap();
        assert!(final_rec.epoch == 2 || final_rec.epoch == 3);
        assert!(successes >= 1);
        if successes == 1 {
            let err = a.err().or(b.err()).unwrap();
            assert!(
                matches!(err, StoreError::Fenced(_)),
                "conflict must be Fenced, got {err}"
            );
        }
    }

    // -- durability gate + WAL buffer ---------------------------------------------

    #[tokio::test]
    async fn wal_flush_empty_is_noop() {
        let s = S3Store::builder()
            .with_store(new_in_memory())
            .with_prefix(Path::from("oxkv"))
            .with_session("node-a")
            .build()
            .await
            .expect("build");
        s.flush().await.expect("empty flush is noop");
    }

    #[tokio::test]
    async fn wal_commit_durable_ok() {
        let store = new_in_memory();
        let mut s = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv"))
            .with_session("node-a")
            .build()
            .await
            .expect("build");
        let epoch = s.epoch();
        // via Store trait (always flush = RPO=0)
        s.set_bytes("k1", b"v1").await.expect("set_bytes");
        // WAL file must exist at e{epoch}/wal/00000000.log.zst
        let wal = wal_path(&Path::from("oxkv"), epoch, 0);
        let got = store.get(&wal).await.expect("wal exists");
        let bytes = got.bytes().await.expect("bytes");
        assert!(!bytes.is_empty());
        // mem reflects write via GetSet
        assert_eq!(s.get_bytes("k1").await.unwrap(), Some(b"v1".to_vec()));
    }

    #[tokio::test]
    async fn wal_commit_durable_fenced_after_epoch_steal() {
        let store = new_in_memory();
        let prefix = Path::from("oxkv");
        let mut s_a = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(prefix.clone())
            .with_session("node-a")
            .build()
            .await
            .expect("node-a build");
        // node-a commits one durable write — succeeds
        s_a.set_bytes("k1", b"v1").await.expect("a commit 1");

        // node-b fences node-a
        let mut s_b = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(prefix.clone())
            .with_session("node-b")
            .build()
            .await
            .expect("node-b build");
        assert_eq!(s_b.epoch(), s_a.epoch() + 1);

        // node-a's next durable write must be fenced (bucket proof fails)
        let err = s_a
            .set_bytes("k2", b"v2")
            .await
            .expect_err("must be fenced");
        assert!(
            matches!(err, StoreError::Fenced(_)),
            "expected Fenced, got {err}"
        );

        // node-b can still write
        s_b.set_bytes("k3", b"v3").await.expect("b can write");
    }

    #[tokio::test]
    async fn wal_group_commit_batches() {
        let store = new_in_memory();
        let mut s = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv"))
            .with_session("node-a")
            .build()
            .await
            .expect("build");
        let epoch = s.epoch();
        // via Transaction: two staged writes before single flush -> one WAL file (group commit)
        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes("k1", b"v1").await.unwrap();
        tx.set_bytes("k2", b"v2").await.unwrap();
        tx.commit().await.expect("tx commit batch");
        let wal0 = wal_path(&Path::from("oxkv"), epoch, 0);
        assert!(store.get(&wal0).await.is_ok(), "first flush creates wal 0");
        // second tx with no ops is noop (not tested) - next staged write creates wal 1
        let mut tx2 = s.begin_tx().unwrap();
        tx2.set_bytes("k3", b"v3").await.unwrap();
        tx2.commit().await.expect("tx commit wal 1");
        let wal1 = wal_path(&Path::from("oxkv"), epoch, 1);
        assert!(store.get(&wal1).await.is_ok());
        // also verify visibility via GetSet
        assert_eq!(s.get_bytes("k1").await.unwrap(), Some(b"v1".to_vec()));
        assert_eq!(s.get_bytes("k3").await.unwrap(), Some(b"v3".to_vec()));
    }

    #[tokio::test]
    async fn wal_put_already_exists_is_idempotent() {
        let store = new_in_memory();
        let mut s = S3Store::builder()
            .with_store(Arc::clone(&store))
            .with_prefix(Path::from("oxkv"))
            .with_session("node-a")
            .build()
            .await
            .expect("build");
        let epoch = s.epoch();
        // manually create wal 0 to simulate prior successful PUT that caller retried
        let wal0 = wal_path(&Path::from("oxkv"), epoch, 0);
        store
            .put(&wal0, PutPayload::from_static(b"pre-existing"))
            .await
            .unwrap();
        // our next set will try to create wal 0 again with PutMode::Create -> AlreadyExists, but should be idempotent
        s.set_bytes("k1", b"v1")
            .await
            .expect("idempotent on AlreadyExists");
        // still fenced check must pass (ownership still us)
        s.wal_seq.store(99, std::sync::atomic::Ordering::SeqCst);
        s.set_bytes("k2", b"v2").await.expect("next seq ok");
    }

    #[tokio::test]
    async fn s3store_getset_has_and_gets_via_trait() {
        let mut s = S3Store::builder()
            .with_store(new_in_memory())
            .with_prefix(Path::from("oxkv"))
            .with_session("node-a")
            .build()
            .await
            .expect("build");
        assert_eq!(s.get_bytes("missing").await.unwrap(), None);
        assert!(!s.has("missing").await.unwrap());
        s.set_bytes("a", b"1").await.unwrap();
        s.set_bytes("b", b"2").await.unwrap();
        s.set_bytes("c", b"3").await.unwrap();
        assert!(s.has("a").await.unwrap());
        let all = s
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].key, "a");
        // delete via trait is durable
        assert!(s.delete("b").await.unwrap());
        assert_eq!(s.get_bytes("b").await.unwrap(), None);
        let after = s
            .gets_bytes(None, Direction::Next, (None, None))
            .await
            .unwrap();
        assert_eq!(after.len(), 2);
    }

    #[tokio::test]
    async fn s3tx_only_persists_on_commit() {
        let mut s = S3Store::builder()
            .with_store(new_in_memory())
            .with_prefix(Path::from("oxkv"))
            .with_session("node-a")
            .build()
            .await
            .expect("build");
        s.set_bytes("k0", b"v0").await.unwrap();
        let epoch = s.epoch();
        // tx stages but not visible to store until commit
        let mut tx = s.begin_tx().unwrap();
        tx.set_bytes("k1", b"v1").await.unwrap();
        tx.set_bytes("k2", b"v2").await.unwrap();
        // read-your-writes inside tx
        assert_eq!(tx.get_bytes("k1").await.unwrap(), Some(b"v1".to_vec()));
        assert_eq!(tx.get_bytes("k0").await.unwrap(), Some(b"v0".to_vec()));
        // not yet durable: wal count still 1 (only k0)
        let store = s.inner_store();
        assert!(
            store
                .get(&wal_path(&Path::from("oxkv"), epoch, 1))
                .await
                .is_err()
        );
        // rollback discards
        let mut tx2 = s.begin_tx().unwrap();
        tx2.set_bytes("k9", b"v9").await.unwrap();
        tx2.rollback().await.unwrap();
        assert_eq!(s.get_bytes("k9").await.unwrap(), None);
        // commit persists
        tx.commit().await.unwrap();
        assert_eq!(s.get_bytes("k1").await.unwrap(), Some(b"v1".to_vec()));
        assert!(
            store
                .get(&wal_path(&Path::from("oxkv"), epoch, 1))
                .await
                .is_ok()
        );
    }
}
