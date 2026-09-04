//! Blob overflow for large values — spills to `e{epoch}/blob/{hash}`.
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]

use std::sync::Arc;

use object_store::path::Path;
use object_store::{ObjectStore, PutMode, PutPayload};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::store::{Result, StoreError};

use super::ownership::epoch_prefix;

/// Overflow helper — `klen+vlen > block_size` spills to `blob/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BlobPointer {
    /// Blob object path as stored (e.g. `e000007/blob/<hash>`).
    pub blob: String,
    /// Original value length.
    pub len: usize,
    /// `CRC32` of original value.
    pub crc: u32,
}

/// Returns `true` if `key+value` exceeds `block_size` and must spill to blob.
#[must_use]
pub(crate) fn is_overflow(key: &str, value: &[u8], block_size: usize) -> bool {
    key.len() + value.len() + 8 > block_size
}

/// Deterministic hash for blob name — hex `SHA-256`.
#[must_use]
pub(crate) fn blob_hash(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    let digest = hasher.finalize();
    let mut buf = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as writer;
        let _ = writer::write_fmt(&mut buf, format_args!("{byte:02x}"));
    }
    buf
}

/// Returns `\{prefix}/e{epoch:06}/blob/{hash}` for large-value overflow.
#[must_use]
pub(crate) fn blob_path(prefix: &Path, epoch: u64, hash: &str) -> Path {
    epoch_prefix(prefix, epoch)
        .child("blob")
        .child(hash.to_string())
}

/// Encodes a blob pointer as JSON bytes for inline SST value.
#[must_use]
pub(crate) fn encode_blob_pointer(blob: &Path, len: usize, crc: u32) -> Vec<u8> {
    let ptr = BlobPointer {
        blob: blob.to_string(),
        len,
        crc,
    };
    serde_json::to_vec(&ptr).expect("blob pointer serialize")
}

/// Tries to decode `value` as `BlobPointer`; `None` if not a pointer.
#[must_use]
pub(crate) fn try_decode_blob_pointer(value: &[u8]) -> Option<BlobPointer> {
    serde_json::from_slice(value).ok()
}

/// Puts `value` to `e{epoch}/blob/{hash}` via `If-None-Match` and returns the path.
pub(crate) async fn put_blob(
    store: Arc<dyn ObjectStore>,
    prefix: &Path,
    epoch: u64,
    value: &[u8],
) -> Result<Path> {
    let hash = blob_hash(value);
    let path = blob_path(prefix, epoch, &hash);
    let payload = PutPayload::from(value.to_vec());
    match store.put_opts(&path, payload, PutMode::Create.into()).await {
        Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => Ok(path),
        Err(err) => Err(StoreError::Storage(format!("put blob failed: {err}"))),
    }
}

/// Gets blob value at `blob_path`.
pub(crate) async fn get_blob(store: Arc<dyn ObjectStore>, blob_path: &Path) -> Result<Vec<u8>> {
    let res = store
        .get(blob_path)
        .await
        .map_err(|e| StoreError::Storage(format!("get blob {blob_path} failed: {e}")))?;
    let bytes = res
        .bytes()
        .await
        .map_err(|e| StoreError::Storage(format!("read blob failed: {e}")))?;
    Ok(bytes.to_vec())
}
