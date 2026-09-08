//! Blob overflow for large values — spills to `e{epoch}/blob/{hash}`.
#![allow(unreachable_pub, missing_docs)]
#![allow(clippy::pedantic, clippy::all)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::store::storage::{ObjectPath, PutMode, Storage};
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

/// Returns `\{prefix}/e{epoch}/blob/{hash}` for large-value overflow.
#[must_use]
pub(crate) fn blob_path(prefix: &ObjectPath, epoch: u64, hash: &str) -> ObjectPath {
    epoch_prefix(prefix, epoch).child("blob").child(hash)
}

/// Encodes a blob pointer as JSON bytes for inline SST value.
#[must_use]
pub(crate) fn encode_blob_pointer(blob: &ObjectPath, len: usize, crc: u32) -> Vec<u8> {
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
    // Fast-reject: blob pointers are JSON objects `{"blob":...}`; most values
    // are not JSON at all. Avoid serde parse for the common case.
    if value.len() < 10 || value.first() != Some(&b'{') {
        return None;
    }
    serde_json::from_slice(value).ok()
}

/// Puts `value` to `e{epoch}/blob/{hash}` via `If-None-Match` and returns the path.
pub(crate) async fn put_blob(
    store: Arc<dyn Storage>,
    prefix: &ObjectPath,
    epoch: u64,
    value: &[u8],
) -> Result<ObjectPath> {
    let hash = blob_hash(value);
    let path = blob_path(prefix, epoch, &hash);
    match store.put_opts(&path, value.to_vec(), PutMode::Create).await {
        Ok(_) => Ok(path),
        Err(StoreError::CasConflict(_)) => Ok(path),
        Err(err) => Err(StoreError::Storage(format!("put blob failed: {err}"))),
    }
}

/// Gets blob value at `blob_path`.
pub(crate) async fn get_blob(store: Arc<dyn Storage>, blob_path: &ObjectPath) -> Result<Vec<u8>> {
    let out = store
        .get(blob_path)
        .await
        .map_err(|e| StoreError::Storage(format!("get blob {blob_path} failed: {e}")))?;
    Ok(out.bytes)
}
