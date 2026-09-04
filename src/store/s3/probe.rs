//! Storage probe — validates conditional writes (`If-None-Match` / `If-Match`).
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]

use std::sync::Arc;

use object_store::path::Path;
use object_store::{ObjectStore, PutMode, PutPayload, PutResult, UpdateVersion};

use crate::store::{Result, StoreError};

fn probe_path(prefix: &Path) -> Path {
    let base = if prefix.as_ref().is_empty() {
        Path::from("probe")
    } else {
        prefix.child("probe")
    };
    base.child("canary")
}

/// Runs the storage probe against `store` at `prefix/probe/canary`.
///
/// Mirrors `celld diagnose` — validates `If-None-Match` / `If-Match`
/// conditional writes. Returns `Ok(())` only on
/// `ok (create, reject-create, reject-stale)`.
pub(crate) async fn probe_store(store: Arc<dyn ObjectStore>, prefix: &Path) -> Result<()> {
    let path = probe_path(prefix);

    let first: PutResult = store
        .put_opts(
            &path,
            PutPayload::from_static(b"probe"),
            PutMode::Create.into(),
        )
        .await
        .map_err(|e| StoreError::Storage(format!("probe create failed: {e}")))?;

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

    store
        .delete(&path)
        .await
        .map_err(|e| StoreError::Storage(format!("probe cleanup failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::s3::new_in_memory;

    #[tokio::test]
    async fn probe_ok_on_in_memory() {
        let store = new_in_memory();
        probe_store(Arc::clone(&store), &Path::default())
            .await
            .expect("InMemory must pass probe");
    }
}
