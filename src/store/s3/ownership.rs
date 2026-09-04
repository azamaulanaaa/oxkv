//! Ownership and epoch fencing (`ownership.json` CAS).
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]

use std::sync::Arc;

use object_store::path::Path;
use object_store::{ObjectStore, PutMode, PutPayload, UpdateVersion};
use serde::{Deserialize, Serialize};

use crate::store::{Result, StoreError};

/// Ownership record stored at `{prefix}/ownership.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OwnershipRecord {
    /// Monotonic epoch — bumped on every successful CAS.
    pub epoch: u64,
    /// Owner session identifier (e.g. `node-a:uuid`).
    pub owner_session: String,
    /// Optional lease expiry in ms since epoch (fleet mode, §9).
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

/// Formats an epoch as `e000007` (zero-padded 6 digits, §3).
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

/// Backoff for CAS contention: `50ms*2^n + jitter`, cap `1s` (§9 G9).
#[must_use]
pub(crate) fn cas_backoff(attempt: u32) -> std::time::Duration {
    let base = 50u64.saturating_mul(1u64 << attempt.min(5));
    let base = base.min(1000);
    let jitter = u64::from(attempt).wrapping_mul(7) % 20;
    std::time::Duration::from_millis(base + jitter)
}

/// Acquires ownership by CAS-bumping `ownership.json` epoch.
///
/// `session` is the owner identifier. On success returns the new
/// `OwnershipRecord` with `epoch = old.epoch + 1` (or `1` on first acquire).
pub(crate) async fn acquire_ownership(
    store: Arc<dyn ObjectStore>,
    prefix: &Path,
    session: &str,
) -> Result<OwnershipRecord> {
    let path = ownership_path(prefix);

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
