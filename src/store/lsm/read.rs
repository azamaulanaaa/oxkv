//! Single lookup engine for point and scan reads.
//!
//! The writer, the transaction, and the reader differ only in the newest
//! layers they snapshot (replay overlay, transaction overlay, `MemTable`);
//! everything below — manifest load, SST fetch, blob deref, newest-wins
//! merge — runs here once. Stale-manifest races (a compaction deleting a
//! listed file between the manifest load and the file `GET`) restart the
//! lookup against a fresh manifest before surfacing the error.

use std::sync::Arc;

use super::{ManifestCache, MemMap};
use super::{get_blob, load_manifest, try_decode_blob_pointer};
use super::{
    merge::{MergeSource, merged_gets_bytes, pull_merge_next},
    sst::SstFile,
};
use crate::store::cache::Cache;
use crate::store::storage::{ObjectPath, Storage};
use crate::store::{Direction, KeyValue, Result, StoreError};

/// Shared inputs for one lookup: storage plus the SST cache.
pub(crate) struct ReadCtx<'a, C> {
    pub inner: &'a Arc<dyn Storage>,
    pub prefix: &'a ObjectPath,
    pub epoch: u64,
    pub manifest_cache: &'a Arc<async_lock::Mutex<ManifestCache>>,
    pub sst_cache: &'a C,
}

#[must_use]
pub(crate) fn is_not_found(err: &StoreError) -> bool {
    matches!(err, StoreError::Storage(msg) if msg.contains("not found"))
}

/// Filters `map` to `[lower, upper]` bounds derived from `direction`/`cursor`.
#[must_use]
pub(crate) fn filter_rows(
    map: &MemMap,
    direction: Direction,
    cursor: &(Option<String>, Option<String>),
) -> Vec<(String, Option<Vec<u8>>)> {
    let (lower, upper) = match direction {
        Direction::Next => (cursor.0.as_deref(), cursor.1.as_deref()),
        Direction::Prev => (cursor.1.as_deref(), cursor.0.as_deref()),
    };
    if lower.is_none() && upper.is_none() {
        return map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    map.iter()
        .filter(|(k, _)| {
            if let Some(lo) = lower
                && k.as_str() < lo
            {
                return false;
            }
            if let Some(hi) = upper
                && k.as_str() > hi
            {
                return false;
            }
            true
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Dereferences a blob pointer with length and CRC verification.
pub(crate) async fn resolve_value<C>(ctx: &ReadCtx<'_, C>, raw: Vec<u8>) -> Result<Vec<u8>> {
    if let Some(ptr) = try_decode_blob_pointer(&raw) {
        let blob_path = ObjectPath::from(ptr.blob.as_str());
        let bytes = get_blob(Arc::clone(ctx.inner), &blob_path).await?;
        if bytes.len() != ptr.len {
            return Err(StoreError::Storage(format!(
                "blob len mismatch for {}: expected {}, got {}",
                blob_path,
                ptr.len,
                bytes.len()
            )));
        }
        let crc = crc32fast::hash(&bytes);
        if crc != ptr.crc {
            return Err(StoreError::Storage(format!(
                "blob crc mismatch for {}: expected {}, got {}",
                blob_path, ptr.crc, crc
            )));
        }
        Ok(bytes)
    } else {
        Ok(raw)
    }
}

async fn read_sst<C>(ctx: &ReadCtx<'_, C>, id: &str) -> Result<Arc<SstFile>> {
    let path = ObjectPath::from(id);
    let out = ctx
        .inner
        .get(&path)
        .await
        .map_err(|e| StoreError::Storage(format!("get sst {id} failed: {e}")))?;
    let sst = Arc::new(SstFile::parse(out.bytes)?);
    sst.verify_file_crc()?;
    Ok(sst)
}

async fn fetch_sst<C>(ctx: &ReadCtx<'_, C>, id: &str) -> Result<Arc<SstFile>>
where
    C: Cache<String, Arc<SstFile>>,
{
    if let Some(cached) = ctx.sst_cache.get(&id.to_string()).await {
        return Ok(cached);
    }
    let sst = read_sst(ctx, id).await?;
    ctx.sst_cache.insert(id.to_string(), Arc::clone(&sst)).await;
    Ok(sst)
}

/// Reads `key` via an optional staged hit, then SSTs newest-first.
///
/// `staged` is the caller-snapshotted newest layers (`Some` hit — value or
/// tombstone — or `None` on a miss); callers snapshot under brief locks so no
/// guard is held across the SST I/O below. Single attempt: callers restart
/// with a fresh snapshot when [`is_not_found`] matches the error.
///
/// # Errors
///
/// Returns `StoreError` on I/O or CRC failure.
#[allow(clippy::option_option)]
pub(crate) async fn point_lookup<C>(
    ctx: &ReadCtx<'_, C>,
    staged: Option<Option<Vec<u8>>>,
    key: &str,
) -> Result<Option<Vec<u8>>>
where
    C: Cache<String, Arc<SstFile>>,
{
    if let Some(hit) = staged {
        return match hit {
            Some(raw) => Ok(Some(resolve_value(ctx, raw).await?)),
            None => Ok(None),
        };
    }
    let (manifest, _etag) = load_manifest(
        Arc::clone(ctx.inner),
        ctx.prefix,
        ctx.epoch,
        ctx.manifest_cache,
        std::time::Duration::from_secs(1),
    )
    .await?;
    for meta in manifest.sst.iter().rev() {
        if key < meta.min_key.as_str() || key > meta.max_key.as_str() {
            continue;
        }
        let sst = fetch_sst(ctx, &meta.id).await?;
        match sst.get_option(key)? {
            Some(Some(raw)) => return Ok(Some(resolve_value(ctx, raw).await?)),
            Some(None) => return Ok(None),
            None => {}
        }
    }
    Ok(None)
}

/// Range scan merging caller-snapshotted newest-first `layers` with SSTs.
///
/// Layers arrive as owned rows so callers never hold table guards across the
/// SST I/O below. Single attempt: callers restart with a fresh snapshot when
/// [`is_not_found`] matches the error.
///
/// # Errors
///
/// Returns `StoreError` on I/O or CRC failure.
pub(crate) async fn range_lookup<C>(
    ctx: &ReadCtx<'_, C>,
    layers: Vec<Vec<(String, Option<Vec<u8>>)>>,
    limit: Option<u32>,
    direction: Direction,
    cursor: (Option<String>, Option<String>),
) -> Result<Vec<KeyValue>>
where
    C: Cache<String, Arc<SstFile>>,
{
    let (manifest, _etag) = load_manifest(
        Arc::clone(ctx.inner),
        ctx.prefix,
        ctx.epoch,
        ctx.manifest_cache,
        std::time::Duration::from_secs(1),
    )
    .await?;
    scan_manifest(ctx, manifest, layers, limit, direction, cursor).await
}

async fn scan_manifest<C>(
    ctx: &ReadCtx<'_, C>,
    manifest: Arc<super::Manifest>,
    layers: Vec<Vec<(String, Option<Vec<u8>>)>>,
    limit: Option<u32>,
    direction: Direction,
    cursor: (Option<String>, Option<String>),
) -> Result<Vec<KeyValue>>
where
    C: Cache<String, Arc<SstFile>>,
{
    let (scan_start, scan_end) = match direction {
        Direction::Next => (cursor.0.as_deref(), cursor.1.as_deref()),
        Direction::Prev => (cursor.1.as_deref(), cursor.0.as_deref()),
    };
    if direction == Direction::Prev && cursor.0.is_none() {
        return Ok(Vec::new());
    }
    if direction == Direction::Next {
        let mut files: Vec<Arc<SstFile>> = Vec::new();
        for meta in manifest.sst.iter().rev() {
            let overlaps = {
                let min = meta.min_key.as_str();
                let max = meta.max_key.as_str();
                let after_lower = scan_start.is_none_or(|s| max >= s);
                let before_upper = scan_end.is_none_or(|e| min <= e);
                after_lower && before_upper
            };
            if !overlaps {
                continue;
            }
            files.push(fetch_sst(ctx, &meta.id).await?);
        }
        let mut pull: Vec<MergeSource<'_>> = Vec::with_capacity(layers.len() + files.len());
        for layer in layers {
            pull.push(MergeSource::Mem(layer.into_iter()));
        }
        for file in &files {
            pull.push(MergeSource::File(file.scan_iter(scan_start, scan_end)));
        }
        let merged = pull_merge_next(&mut pull, limit.map(|l| l as usize))?;
        let mut out = Vec::with_capacity(merged.len());
        for (key, raw) in merged {
            out.push(KeyValue {
                key,
                value: resolve_value(ctx, raw).await?,
            });
        }
        return Ok(out);
    }
    let mut sources = layers;
    for meta in manifest.sst.iter().rev() {
        let overlaps = {
            let min = meta.min_key.as_str();
            let max = meta.max_key.as_str();
            let after_lower = scan_start.is_none_or(|s| max >= s);
            let before_upper = scan_end.is_none_or(|e| min <= e);
            after_lower && before_upper
        };
        if !overlaps {
            continue;
        }
        let sst = read_sst(ctx, &meta.id).await?;
        let scan = sst.scan_with_tombstones(scan_start, scan_end, None)?;
        let mut resolved: Vec<(String, Option<Vec<u8>>)> = Vec::with_capacity(scan.len());
        for (key, value) in scan {
            match value {
                Some(raw) => {
                    let val = resolve_value(ctx, raw).await?;
                    resolved.push((key, Some(val)));
                }
                None => resolved.push((key, None)),
            }
        }
        sources.push(resolved);
    }
    Ok(merged_gets_bytes(sources, limit, direction, cursor))
}
