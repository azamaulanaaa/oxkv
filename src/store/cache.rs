//! Cache abstraction for SST files.
//!
//! Provides a minimal async trait so the LSM engine does not depend directly
//! on `moka`. The current production implementation delegates to `moka`, while
//! `WASM` and future targets can supply a single-threaded `LruCache` without
//! `Send`/`Sync` or `tokio`.

use std::hash::Hash;
use std::sync::Arc;

/// Async cache used by the LSM engine for SST files and other hot objects.
#[async_trait::async_trait]
pub trait Cache<K, V>: Clone + Send + Sync + 'static
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Returns a cached value if present.
    async fn get(&self, key: &K) -> Option<V>;

    /// Inserts `value` under `key`, evicting according to the implementation's policy.
    async fn insert(&self, key: K, value: V);

    /// Removes the entry for `key` if present.
    async fn remove(&self, key: &K);

    /// Returns `true` if the cache contains `key`.
    async fn contains(&self, key: &K) -> bool {
        self.get(key).await.is_some()
    }
}

#[cfg(all(feature = "s3", not(target_arch = "wasm32")))]
mod moka_impl {
    use super::*;
    use moka::future::Cache as MokaCache;

    #[async_trait::async_trait]
    impl<K, V> Cache<K, V> for MokaCache<K, V>
    where
        K: Eq + Hash + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        async fn get(&self, key: &K) -> Option<V> {
            MokaCache::get(self, key).await
        }

        async fn insert(&self, key: K, value: V) {
            MokaCache::insert(self, key, value).await;
        }

        async fn remove(&self, key: &K) {
            MokaCache::invalidate(self, key).await;
        }
    }
}

/// Simple weight-aware LRU cache suitable for `WASM` and single-threaded targets.
///
/// Backed by a `tokio::sync::RwLock` + `HashMap` for now so it satisfies
/// `Send+Sync` and can be `Clone`. A future `!Send` variant can be added for
/// `wasm32` behind `cfg(target_arch = "wasm32")` without changing the engine.
pub struct LruCache<K, V> {
    inner: Arc<tokio::sync::RwLock<LruInner<K, V>>>,
    capacity: usize,
    weigher: Arc<dyn Fn(&K, &V) -> u32 + Send + Sync>,
}

#[derive(Debug)]
struct LruInner<K, V> {
    map: std::collections::HashMap<K, V>,
    order: std::collections::VecDeque<K>,
    weight: usize,
}

impl<K, V> Clone for LruCache<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            capacity: self.capacity,
            weigher: Arc::clone(&self.weigher),
        }
    }
}

impl<K, V> LruCache<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Creates a new cache with `max_capacity` weight units.
    pub fn new(
        max_capacity: usize,
        weigher: impl Fn(&K, &V) -> u32 + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(tokio::sync::RwLock::new(LruInner {
                map: std::collections::HashMap::new(),
                order: std::collections::VecDeque::new(),
                weight: 0,
            })),
            capacity: max_capacity,
            weigher: Arc::new(weigher),
        }
    }
}

#[async_trait::async_trait]
impl<K, V> Cache<K, V> for LruCache<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    async fn get(&self, key: &K) -> Option<V> {
        let mut inner = self.inner.write().await;
        let val = inner.map.get(key).cloned();
        if val.is_some() {
            // Move to back (most-recent).
            if let Some(pos) = inner.order.iter().position(|k| k == key) {
                let k = inner.order.remove(pos).unwrap();
                inner.order.push_back(k);
            }
        }
        val
    }

    async fn insert(&self, key: K, value: V) {
        let weight = (self.weigher)(&key, &value) as usize;
        let mut inner = self.inner.write().await;

        if let Some(old) = inner.map.get(&key) {
            let old_w = (self.weigher)(&key, old) as usize;
            inner.weight = inner.weight.saturating_sub(old_w);
            if let Some(pos) = inner.order.iter().position(|k| k == &key) {
                inner.order.remove(pos);
            }
        }

        inner.map.insert(key.clone(), value);
        inner.order.push_back(key.clone());
        inner.weight += weight;

        while inner.weight > self.capacity && !inner.order.is_empty() {
            if let Some(oldest) = inner.order.pop_front() {
                if let Some(v) = inner.map.remove(&oldest) {
                    let w = (self.weigher)(&oldest, &v) as usize;
                    inner.weight = inner.weight.saturating_sub(w);
                }
            }
        }
    }

    async fn remove(&self, key: &K) {
        let mut inner = self.inner.write().await;
        if let Some(v) = inner.map.remove(key) {
            let w = (self.weigher)(key, &v) as usize;
            inner.weight = inner.weight.saturating_sub(w);
            if let Some(pos) = inner.order.iter().position(|k| k == key) {
                inner.order.remove(pos);
            }
        }
    }
}
