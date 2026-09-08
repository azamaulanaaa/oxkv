//! Storage probe — validates conditional writes (`If-None-Match` / `If-Match`).
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]

use std::sync::Arc;

use crate::store::storage::{ObjectPath, ObjectVersion, PutMode, Storage};
use crate::store::{Result, StoreError};

fn probe_path(prefix: &ObjectPath) -> ObjectPath {
    let base = if prefix.is_empty() {
        ObjectPath::from("probe")
    } else {
        prefix.child("probe")
    };
    base.child("canary")
}

/// Runs the storage probe against `store` at `prefix/probe/canary`.
///
/// Validates that the store correctly enforces `If-None-Match` and `If-Match`
/// conditional writes. Returns `Ok(())` only on
/// `ok (create, reject-create, reject-stale)`.
pub(crate) async fn probe_store(store: Arc<dyn Storage>, prefix: &ObjectPath) -> Result<()> {
    let path = probe_path(prefix);

    let first = store
        .put_opts(&path, b"probe".to_vec(), PutMode::Create)
        .await
        .map_err(|e| StoreError::Storage(format!("probe create failed: {e}")))?;

    let second = store
        .put_opts(&path, b"probe2".to_vec(), PutMode::Create)
        .await;
    match second {
        Err(StoreError::CasConflict(_)) => {}
        Ok(_) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(
                "probe store accepted duplicate create — conditional writes not enforced (needs S3/R2/GCS/Azure, not B2/Hetzner)".to_string(),
            ));
        }
        Err(e) => {
            let _ = store.delete(&path).await;
            return Err(StoreError::Storage(format!(
                "probe second create unexpected error (expected CAS conflict): {e}"
            )));
        }
    }

    let stale = ObjectVersion {
        e_tag: Some("\"stale-etag-should-not-match\"".to_string()),
        version: None,
    };
    let third = store
        .put_opts(&path, b"probe3".to_vec(), PutMode::Update(stale))
        .await;
    match third {
        Err(StoreError::CasConflict(_)) => {}
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
                "probe stale update unexpected error (expected CAS conflict): {e}"
            )));
        }
    }

    let valid = ObjectVersion {
        e_tag: first.e_tag.clone(),
        version: first.version.clone(),
    };
    store
        .put_opts(&path, b"probe-ok".to_vec(), PutMode::Update(valid))
        .await
        .map_err(|e| StoreError::Storage(format!("probe valid update failed: {e}")))?;

    store
        .delete(&path)
        .await
        .map_err(|e| StoreError::Storage(format!("probe cleanup failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStorage;

    fn new_in_memory() -> Arc<dyn Storage> {
        Arc::new(MemStorage::new())
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn probe_ok_on_in_memory() {
        let store = new_in_memory();
        probe_store(Arc::clone(&store), &ObjectPath::default())
            .await
            .expect("MemStorage must pass probe");
    }
}
