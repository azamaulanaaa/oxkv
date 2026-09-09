//! WAL record decoding and startup replay.
//!
//! The writer startup path and the reader open share both the framed-record
//! decoder and the loop applying every listed WAL file to a table.

use std::sync::Arc;

use super::MemTable;
use super::TOMBSTONE_VLEN;
use crate::store::storage::{ObjectPath, Storage};

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
/// Best-effort: missing files are skipped so a concurrent `gc_wal` cannot
/// fail the caller.
pub(crate) async fn replay_listed_wals(
    inner: &Arc<dyn Storage>,
    wal_ids: &[String],
    table: &MemTable,
) {
    let mut guard = table.write().await;
    for wal_id in wal_ids {
        let path = ObjectPath::from(wal_id.as_str());
        let Ok(out) = inner.get(&path).await else {
            continue;
        };
        for (key, value) in decode_wal_records(&out.bytes) {
            guard.insert(key, value);
        }
    }
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
