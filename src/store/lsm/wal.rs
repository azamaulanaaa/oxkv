//! WAL record decoding and startup replay.
//!
//! The writer startup path and the reader open share both the framed-record
//! decoder and the loop applying every listed WAL file to a table.

use std::sync::Arc;
use std::time::Duration;

use super::MemTable;
use super::TOMBSTONE_VLEN;
use super::manifest::{ManifestCache, load_manifest};
use super::read::is_not_found;
use crate::store::storage::{ObjectPath, Storage};
use crate::store::{Result, StoreError};

/// Decodes length-prefixed WAL records (`[u32 key len][key][u32 value len][value]`).
///
/// A value length of [`TOMBSTONE_VLEN`] marks a tombstone (`None`). Decoding
/// stops at the first truncated tail, tolerating partially written files.
#[must_use]
pub(crate) fn decode_wal_records(data: &[u8]) -> Vec<(String, Option<Vec<u8>>)> {
    let mut records = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= data.len() {
        let klen =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        if pos + 4 + klen + 4 > data.len() {
            break;
        }
        let key = match std::str::from_utf8(&data[pos + 4..pos + 4 + klen]) {
            Ok(key) => key.to_string(),
            Err(_) => break,
        };
        let value_start = pos + 4 + klen;
        let vlen = u32::from_le_bytes([
            data[value_start],
            data[value_start + 1],
            data[value_start + 2],
            data[value_start + 3],
        ]) as usize;
        if vlen == TOMBSTONE_VLEN as usize {
            records.push((key, None));
            pos = value_start + 4;
        } else {
            if value_start + 4 + vlen > data.len() {
                break;
            }
            records.push((
                key,
                Some(data[value_start + 4..value_start + 4 + vlen].to_vec()),
            ));
            pos = value_start + 4 + vlen;
        }
    }
    records
}

/// Applies every listed WAL file to `table`, newest file winning per key.
///
/// A listed WAL that is *gone* means the caller's listing is stale — a
/// concurrent `gc_wal` already folded those records into an SST — so the
/// listing is refreshed and replay retried against it. Any other read error
/// is surfaced: silently skipping a transient failure would leave the
/// `MemTable` missing committed records, which the next flush then writes
/// into an SST as if it were complete.
///
/// # Errors
///
/// Returns `StoreError` when a listed WAL is still absent after one refresh,
/// or when its read fails for any reason other than absence.
pub(crate) async fn replay_listed_wals(
    inner: &Arc<dyn Storage>,
    prefix: &ObjectPath,
    epoch: u64,
    manifest_cache: &Arc<async_lock::Mutex<ManifestCache>>,
    wal_ids: &[String],
    table: &MemTable,
) -> Result<()> {
    let mut ids = wal_ids.to_vec();
    for attempt in 0..2 {
        let mut missing: Vec<String> = Vec::new();
        {
            let mut guard = table.write().await;
            for wal_id in &ids {
                let path = ObjectPath::from(wal_id.as_str());
                match inner.get(&path).await {
                    Ok(out) => {
                        for (key, value) in decode_wal_records(&out.bytes) {
                            guard.insert(key, value);
                        }
                    }
                    Err(e) if is_not_found(&e) => missing.push(wal_id.clone()),
                    Err(e) => {
                        return Err(StoreError::Storage(format!(
                            "read wal {wal_id} failed: {e}"
                        )));
                    }
                }
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        if attempt == 1 {
            return Err(StoreError::Storage(format!(
                "wal listed in manifest but absent from storage: {}",
                missing.join(", ")
            )));
        }
        // Refresh: `gc_wal` cleared these ids, so their records live in an SST.
        manifest_cache.lock().await.clear();
        let (manifest, _etag) = load_manifest(
            Arc::clone(inner),
            prefix,
            epoch,
            manifest_cache,
            Duration::from_secs(0),
        )
        .await?;
        ids.clone_from(&manifest.wal);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::encode_record;

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn decode_roundtrip_with_tombstone() {
        let mut buf = Vec::new();
        encode_record(&mut buf, "a", b"1").expect("encode");
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(b"b");
        buf.extend_from_slice(&TOMBSTONE_VLEN.to_le_bytes());
        assert_eq!(
            decode_wal_records(&buf),
            vec![
                ("a".to_string(), Some(b"1".to_vec())),
                ("b".to_string(), None),
            ]
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    fn decode_stops_at_truncated_tail() {
        let mut buf = Vec::new();
        encode_record(&mut buf, "a", b"1").expect("encode");
        buf.extend_from_slice(&[9, 0]);
        assert_eq!(
            decode_wal_records(&buf),
            vec![("a".to_string(), Some(b"1".to_vec()))]
        );
    }
}
