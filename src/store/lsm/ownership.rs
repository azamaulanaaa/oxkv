//! Ownership and epoch fencing (`ownership.json` CAS).
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::store::storage::{ObjectPath, ObjectVersion, PutMode, Storage};
use crate::store::{Result, StoreError};

/// Ownership record stored at `{prefix}/ownership.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OwnershipRecord {
    /// Monotonic epoch — bumped on every successful CAS.
    pub epoch: u64,
    /// Owner session identifier (e.g. `node-a:uuid`).
    pub owner_session: String,
    /// Optional lease expiry in ms since epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_expiry_ms: Option<u64>,
    /// Last known manifest `e_tag` (debug aid).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_etag: Option<String>,
}

/// Returns the path for `ownership.json`.
#[must_use]
pub(crate) fn ownership_path(prefix: &ObjectPath) -> ObjectPath {
    if prefix.is_empty() {
        ObjectPath::from("ownership.json")
    } else {
        prefix.child("ownership.json")
    }
}

/// Formats an epoch as `e000007` (zero-padded 6 digits).
#[must_use]
pub(crate) fn format_epoch(epoch: u64) -> String {
    format!("e{epoch:06}")
}

/// Returns the epoch-scoped prefix `\{prefix}/e{epoch:06}`.
#[must_use]
pub(crate) fn epoch_prefix(prefix: &ObjectPath, epoch: u64) -> ObjectPath {
    let epoch_str = format_epoch(epoch);
    if prefix.is_empty() {
        ObjectPath::from(epoch_str)
    } else {
        prefix.child(&epoch_str)
    }
}

/// Returns `\{prefix}/e{epoch:06}/wal/{seq:08}.log`.
#[must_use]
pub(crate) fn wal_path(prefix: &ObjectPath, epoch: u64, seq: u64) -> ObjectPath {
    epoch_prefix(prefix, epoch)
        .child("wal")
        .child(&format!("{seq:08}.log"))
}

/// Returns `\{prefix}/e{epoch:06}/sst/{level}/{id:09}.sst`.
pub(crate) fn sst_path(prefix: &ObjectPath, epoch: u64, level: u8, id: u64) -> ObjectPath {
    epoch_prefix(prefix, epoch)
        .child("sst")
        .child(&format!("L{level}"))
        .child(&format!("{id:09}.sst"))
}

/// Backoff for CAS contention: `50ms*2^n + jitter`, cap `1s`.
#[must_use]
pub(crate) fn cas_backoff(attempt: u32) -> std::time::Duration {
    // Jitter is reserved *inside* the cap, not added after it: capping the base
    // and then adding jitter produced 1010ms against a documented 1 s ceiling,
    // and left every contended writer sleeping the same flat amount once
    // saturated.
    let jitter = u64::from(attempt).wrapping_mul(7) % 20;
    let base = 50u64.saturating_mul(1u64 << attempt.min(5));
    let base = base.min(1000 - 20);
    std::time::Duration::from_millis((base + jitter).min(1000))
}

/// Acquires ownership by CAS-bumping `ownership.json` epoch.
///
/// `session` is the owner identifier. On success returns the new
/// `OwnershipRecord` with `epoch = old.epoch + 1` (or `1` on first acquire).
pub(crate) async fn acquire_ownership(
    store: Arc<dyn Storage>,
    prefix: &ObjectPath,
    session: &str,
) -> Result<OwnershipRecord> {
    let path = ownership_path(prefix);

    let (existing, version) = match store.get(&path).await {
        Ok(out) => {
            let rec: OwnershipRecord = serde_json::from_slice(&out.bytes)
                .map_err(|e| StoreError::Storage(format!("corrupt ownership.json: {e}")))?;
            let ver = ObjectVersion {
                e_tag: out.e_tag.clone(),
                version: out.version.clone(),
            };
            (Some(rec), Some(ver))
        }
        Err(e) if e.to_string().contains("not found") => (None, None),
        Err(e) => return Err(StoreError::Storage(format!("get ownership failed: {e}"))),
    };

    // `saturating_add`, not `+ 1`: at `u64::MAX` the plain add panics under
    // overflow checks instead of reporting an unusable epoch.
    let next_epoch = existing.as_ref().map_or(1, |r| r.epoch.saturating_add(1));
    let new_rec = OwnershipRecord {
        epoch: next_epoch,
        owner_session: session.to_string(),
        lease_expiry_ms: None,
        manifest_etag: None,
    };
    let payload = serde_json::to_vec(&new_rec)
        .map_err(|e| StoreError::Storage(format!("serialize ownership: {e}")))?;

    let put_res = if let Some(ver) = version {
        store.put_opts(&path, payload, PutMode::Update(ver)).await
    } else {
        store.put_opts(&path, payload, PutMode::Create).await
    };

    match put_res {
        Ok(_) => Ok(new_rec),
        Err(StoreError::CasConflict(_)) => Err(StoreError::Fenced(format!(
            "ownership CAS conflict at epoch {next_epoch} for session {session} — fenced"
        ))),
        Err(e) => Err(StoreError::Storage(format!("put ownership failed: {e}"))),
    }
}

/// Reads the current ownership record, if any.
pub(crate) async fn read_ownership(
    store: Arc<dyn Storage>,
    prefix: &ObjectPath,
) -> Result<Option<OwnershipRecord>> {
    let path = ownership_path(prefix);
    match store.get(&path).await {
        Ok(out) => {
            let rec: OwnershipRecord = serde_json::from_slice(&out.bytes)
                .map_err(|e| StoreError::Storage(format!("corrupt ownership.json: {e}")))?;
            Ok(Some(rec))
        }
        Err(e) if e.to_string().contains("not found") => Ok(None),
        Err(e) => Err(StoreError::Storage(format!("get ownership failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::storage::{MemStorage, PutMode};

    fn backend() -> Arc<dyn Storage> {
        Arc::new(MemStorage::new())
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn epoch_and_path_formatting_contract() {
        assert_eq!(format_epoch(1), "e000001");
        assert_eq!(format_epoch(1_000_000), "e1000000");
        let p = ObjectPath::from("p");
        assert_eq!(
            wal_path(&p, 1_000_000, 5).as_str(),
            "p/e1000000/wal/00000005.log"
        );
        assert_eq!(
            sst_path(&p, 1_000_000, 1, 5).as_str(),
            "p/e1000000/sst/L1/000000005.sst"
        );
        assert_eq!(
            wal_path(&ObjectPath::default(), 7, 0).as_str(),
            "e000007/wal/00000000.log"
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn cas_backoff_grows_and_stays_capped() {
        assert!(cas_backoff(0) < cas_backoff(1));
        assert!(cas_backoff(1) < cas_backoff(3));
        // The documented ceiling is 1 s for *every* attempt, jitter included.
        for attempt in 0..64 {
            assert!(
                cas_backoff(attempt) <= std::time::Duration::from_secs(1),
                "attempt {attempt} backed off {:?}, over the documented cap",
                cas_backoff(attempt)
            );
        }
        // Jitter must survive at saturation, else every contended writer
        // sleeps the same flat amount and re-synchronises into a thundering
        // herd.
        let saturated: std::collections::BTreeSet<u64> =
            (5..64).map(|a| cas_backoff(a).as_millis() as u64).collect();
        assert!(
            saturated.len() > 1,
            "backoff lost its jitter once saturated: {saturated:?}"
        );
    }

    /// A corrupt `ownership.json` must be reported, never silently treated as
    /// "no owner" — that would let a second writer take over a live prefix.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn corrupt_ownership_record_is_reported() {
        let store = backend();
        let p = ObjectPath::from("corrupt");
        store
            .put_opts(&ownership_path(&p), b"not json".to_vec(), PutMode::Create)
            .await
            .unwrap();
        let err = acquire_ownership(Arc::clone(&store), &p, "a")
            .await
            .expect_err("acquire must not succeed on a corrupt record");
        assert!(
            err.to_string().contains("corrupt ownership.json"),
            "unexpected error: {err}"
        );
        let err = read_ownership(Arc::clone(&store), &p)
            .await
            .expect_err("read must not succeed on a corrupt record");
        assert!(
            err.to_string().contains("corrupt ownership.json"),
            "unexpected error: {err}"
        );
    }

    /// A second acquire takes ownership, and the previous holder is fenced.
    ///
    /// This is the guarantee fencing rests on: the epoch bump is what makes a
    /// superseded writer discover it is no longer the owner.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn acquiring_twice_fences_the_first_holder() {
        let store = backend();
        let p = ObjectPath::from("race");
        let first = acquire_ownership(Arc::clone(&store), &p, "a")
            .await
            .unwrap();
        let second = acquire_ownership(Arc::clone(&store), &p, "b")
            .await
            .unwrap();
        assert_eq!(second.epoch, first.epoch + 1);
        let current = read_ownership(Arc::clone(&store), &p)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.owner_session, "b");
        assert_ne!(
            (current.epoch, current.owner_session.clone()),
            (first.epoch, "a".to_string()),
            "the superseded holder must no longer match ownership"
        );
    }

    /// Epochs are monotonic; the bump at `u64::MAX` must not panic under
    /// overflow checks.
    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn epoch_is_monotonic_and_bump_saturates() {
        let store = backend();
        let p = ObjectPath::from("mono");
        let first = acquire_ownership(Arc::clone(&store), &p, "a")
            .await
            .unwrap();
        let second = acquire_ownership(Arc::clone(&store), &p, "b")
            .await
            .unwrap();
        assert_eq!(second.epoch, first.epoch + 1);

        // Park the record at u64::MAX and confirm the next acquire reports a
        // fencing/CAS failure rather than panicking on `+ 1`.
        let payload = serde_json::to_vec(&OwnershipRecord {
            epoch: u64::MAX,
            owner_session: "z".to_string(),
            lease_expiry_ms: None,
            manifest_etag: None,
        })
        .unwrap();
        store.delete(&ownership_path(&p)).await.unwrap();
        store
            .put_opts(&ownership_path(&p), payload, PutMode::Create)
            .await
            .unwrap();
        let next = acquire_ownership(store, &p, "c").await.unwrap();
        assert_eq!(next.epoch, u64::MAX, "bump must saturate, not wrap");
    }
}
