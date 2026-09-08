//! Cache abstraction for SST files.
//!
//! Provides a minimal async trait so the LSM engine does not depend directly
//! on any single cache. The production implementation is the built-in
//! `LruCache` (an `S3-FIFO` eviction policy kept under its historic name, also
//! aliased as [`S3FifoCache`]), while `moka` remains an optional alternative
//! and `WASM` and future targets can supply their own `Cache` without `tokio`.

use std::collections::{HashMap, VecDeque};
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

#[cfg(all(feature = "moka", not(target_arch = "wasm32")))]
mod moka_impl {
    use super::{Cache, Hash};
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

/// Weight function for [`LruCache`]: maps an entry to its weight units.
type Weigher<K, V> = Arc<dyn Fn(&K, &V) -> u32 + Send + Sync>;

/// Saturating per-entry frequency counter cap (`S3-FIFO` tracks 0..=3).
const MAX_FREQ: u8 = 3;

/// Backstop on remembered ghost keys, bounding memory when the weigher
/// reports zero weights (weight-based trimming alone cannot shrink then).
const GHOST_MAX_ENTRIES: usize = 1024;

/// Queue an entry currently lives in.
///
/// Fresh entries enter [`Queue::Small`]; entries re-admitted after a ghost hit
/// enter [`Queue::Medium`]; entries hit while queued are demoted to
/// [`Queue::Large`], which evicts with a second-chance decrement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Queue {
    Small,
    Medium,
    Large,
}

#[derive(Debug)]
struct Entry<V> {
    value: V,
    weight: usize,
    freq: u8,
    queue: Queue,
    seq: u64,
}

#[derive(Debug)]
struct S3Inner<K, V> {
    map: HashMap<K, Entry<V>>,
    small: VecDeque<(u64, K)>,
    medium: VecDeque<(u64, K)>,
    large: VecDeque<(u64, K)>,
    /// Recently evicted-cold keys with their weights. A reinsert hitting here
    /// is admitted to the main queue instead of competing as brand new.
    ghost: VecDeque<(K, usize)>,
    ghost_map: HashMap<K, usize>,
    ghost_weight: usize,
    weight: usize,
    next_seq: u64,
}

impl<K, V> S3Inner<K, V>
where
    K: Eq + Hash + Clone,
{
    /// Issues a fresh sequence number for a queue slot. Slots are matched by
    /// sequence, so moves and re-inserts leave stale slots that eviction
    /// skips — amortized `O(1)` without an intrusive linked list.
    fn bump(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        seq
    }

    /// Pops the oldest live key from `queue`, skipping stale slots left behind
    /// by moves, updates, and removals.
    fn pop_live_key(&mut self, queue: Queue) -> Option<K> {
        let slots = match queue {
            Queue::Small => &mut self.small,
            Queue::Medium => &mut self.medium,
            Queue::Large => &mut self.large,
        };
        while let Some((seq, key)) = slots.pop_front() {
            if let Some(entry) = self.map.get(&key)
                && entry.seq == seq
                && entry.queue == queue
            {
                return Some(key);
            }
        }
        None
    }

    fn push_slot(&mut self, queue: Queue, seq: u64, key: K) {
        match queue {
            Queue::Small => self.small.push_back((seq, key)),
            Queue::Medium => self.medium.push_back((seq, key)),
            Queue::Large => self.large.push_back((seq, key)),
        }
    }

    /// Remembers a cold-evicted key for admission decisions, trimming the
    /// ghost queue to its weight and entry-count caps.
    fn remember_ghost(&mut self, key: K, weight: usize, ghost_cap: usize) {
        if self.ghost_map.contains_key(&key) {
            return;
        }
        self.ghost.push_back((key.clone(), weight));
        self.ghost_map.insert(key, weight);
        self.ghost_weight = self.ghost_weight.saturating_add(weight);
        while self.ghost_weight > ghost_cap || self.ghost.len() > GHOST_MAX_ENTRIES {
            let Some((old_key, old_weight)) = self.ghost.pop_front() else {
                break;
            };
            if self.ghost_map.remove(&old_key).is_some() {
                self.ghost_weight = self.ghost_weight.saturating_sub(old_weight);
            }
        }
    }

    /// Evicts one entry from the small queue: cold entries leave to the ghost
    /// queue, hot entries are demoted to the main queue. Returns `true` on
    /// progress (a removal or a demotion), `false` when the queue is empty.
    fn evict_from_small(&mut self, ghost_cap: usize) -> bool {
        let Some(key) = self.pop_live_key(Queue::Small) else {
            return false;
        };
        let cold = self.map.get(&key).is_some_and(|entry| entry.freq == 0);
        if cold {
            if let Some(entry) = self.map.remove(&key) {
                self.weight = self.weight.saturating_sub(entry.weight);
                self.remember_ghost(key, entry.weight, ghost_cap);
            }
        } else {
            let seq = self.bump();
            if let Some(entry) = self.map.get_mut(&key) {
                entry.freq = entry.freq.saturating_sub(1);
                entry.queue = Queue::Large;
                entry.seq = seq;
            }
            self.push_slot(Queue::Large, seq, key);
        }
        true
    }

    /// Evicts one entry from the medium queue: cold entries are dropped, hot
    /// entries move to the large queue. Returns `true` on progress,
    /// `false` when the queue is empty.
    fn evict_from_medium(&mut self) -> bool {
        let Some(key) = self.pop_live_key(Queue::Medium) else {
            return false;
        };
        let cold = self.map.get(&key).is_some_and(|entry| entry.freq == 0);
        if cold {
            if let Some(entry) = self.map.remove(&key) {
                self.weight = self.weight.saturating_sub(entry.weight);
            }
        } else {
            let seq = self.bump();
            if let Some(entry) = self.map.get_mut(&key) {
                entry.freq = entry.freq.saturating_sub(1);
                entry.queue = Queue::Large;
                entry.seq = seq;
            }
            self.push_slot(Queue::Large, seq, key);
        }
        true
    }

    /// Evicts one entry from the large queue with a second-chance decrement:
    /// hot heads are requeued with a decreased counter, cold heads are
    /// dropped. Returns `true` on progress, `false` when the queue is empty.
    fn evict_from_large(&mut self) -> bool {
        let mut progress = false;
        while let Some(key) = self.pop_live_key(Queue::Large) {
            let hot = self.map.get(&key).is_some_and(|entry| entry.freq > 0);
            if hot {
                let seq = self.bump();
                if let Some(entry) = self.map.get_mut(&key) {
                    entry.freq = entry.freq.saturating_sub(1);
                    entry.seq = seq;
                }
                self.push_slot(Queue::Large, seq, key);
                progress = true;
            } else {
                if let Some(entry) = self.map.remove(&key) {
                    self.weight = self.weight.saturating_sub(entry.weight);
                }
                return true;
            }
        }
        progress
    }

    /// Evicts until total weight fits `capacity`, draining the small queue
    /// first so one-hit floods cannot flush the main queue. The iteration
    /// bound is a backstop; queue flow (small/medium demote toward large,
    /// large strictly decrements) otherwise guarantees termination.
    fn evict_while_over(&mut self, capacity: usize, ghost_cap: usize) {
        let bound = self.map.len().saturating_mul(4).saturating_add(32);
        for _ in 0..bound {
            if self.weight <= capacity {
                break;
            }
            let progressed = self.evict_from_small(ghost_cap)
                || self.evict_from_medium()
                || self.evict_from_large();
            if !progressed {
                break;
            }
        }
    }
}

/// Scan-resistant weight-aware cache suitable for `WASM` and single-threaded
/// targets.
///
/// Eviction follows `S3-FIFO`: fresh entries enter a small `FIFO`, hits bump a
/// saturating frequency counter, and evicted-cold keys are remembered in a
/// ghost queue so an immediate reinsert is admitted to the main queue. Point
/// lookups never reorder queues, so one-hit reads and scans cannot flush hot
/// entries the way they do under plain `LRU` — the main behavioral gap to
/// `moka`'s admission filter, closed here without background threads, timers,
/// or `tokio`, keeping `wasm32` compatibility. All operations are amortized
/// `O(1)`; critical sections hold a single `futures::lock::Mutex` only for
/// queue bookkeeping with no `.await` inside.
///
/// The historic name is kept so existing `OxKvStore` instantiations and imports
/// keep compiling; new code may prefer the [`S3FifoCache`] alias.
pub struct LruCache<K, V> {
    inner: Arc<futures::lock::Mutex<S3Inner<K, V>>>,
    capacity: usize,
    ghost_cap: usize,
    weigher: Weigher<K, V>,
}

/// Preferred name for the scan-resistant policy; identical to [`LruCache`].
pub type S3FifoCache<K, V> = LruCache<K, V>;

impl<K, V> Clone for LruCache<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            capacity: self.capacity,
            ghost_cap: self.ghost_cap,
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
            inner: Arc::new(futures::lock::Mutex::new(S3Inner {
                map: HashMap::new(),
                small: VecDeque::new(),
                medium: VecDeque::new(),
                large: VecDeque::new(),
                ghost: VecDeque::new(),
                ghost_map: HashMap::new(),
                ghost_weight: 0,
                weight: 0,
                next_seq: 0,
            })),
            capacity: max_capacity,
            ghost_cap: max_capacity,
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
        let mut inner = self.inner.lock().await;
        if let Some(entry) = inner.map.get_mut(key) {
            entry.freq = entry.freq.saturating_add(1).min(MAX_FREQ);
            Some(entry.value.clone())
        } else {
            None
        }
    }

    async fn insert(&self, key: K, value: V) {
        let weight = (self.weigher)(&key, &value) as usize;
        let mut inner = self.inner.lock().await;

        if let Some(entry) = inner.map.get_mut(&key) {
            let old_weight = entry.weight;
            entry.value = value;
            entry.weight = weight;
            entry.freq = entry.freq.saturating_add(1).min(MAX_FREQ);
            inner.weight = inner
                .weight
                .saturating_sub(old_weight)
                .saturating_add(weight);
        } else {
            // Entries heavier than the whole cache are not retained, matching
            // the previous eviction behavior without pointless bookkeeping.
            if weight > self.capacity {
                return;
            }
            let promoted = match inner.ghost_map.remove(&key) {
                Some(weight) => {
                    inner.ghost_weight = inner.ghost_weight.saturating_sub(weight);
                    true
                }
                None => false,
            };
            let (queue, freq) = if promoted {
                (Queue::Medium, 1)
            } else {
                (Queue::Small, 0)
            };
            let seq = inner.bump();
            inner.map.insert(
                key.clone(),
                Entry {
                    value,
                    weight,
                    freq,
                    queue,
                    seq,
                },
            );
            inner.push_slot(queue, seq, key);
            inner.weight = inner.weight.saturating_add(weight);
        }

        inner.evict_while_over(self.capacity, self.ghost_cap);
    }

    async fn remove(&self, key: &K) {
        let mut inner = self.inner.lock().await;
        if let Some(entry) = inner.map.remove(key) {
            inner.weight = inner.weight.saturating_sub(entry.weight);
        }
        // An explicit removal must not promote a later reinsert.
        if let Some(weight) = inner.ghost_map.remove(key) {
            inner.ghost_weight = inner.ghost_weight.saturating_sub(weight);
        }
    }

    async fn contains(&self, key: &K) -> bool {
        // Presence check only: unlike the default `get`-based implementation,
        // probing must not bump frequency and shelter cold entries.
        self.inner.lock().await.map.contains_key(key)
    }
}

#[cfg(all(test, feature = "oxkv"))]
mod tests {
    use super::*;

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn weight_eviction_removes_oldest() {
        let cache = LruCache::new(10, |_: &String, v: &usize| {
            u32::try_from(*v).expect("test weight fits u32")
        });
        cache.insert("a".to_string(), 6).await;
        cache.insert("b".to_string(), 6).await;
        assert!(cache.get(&"a".to_string()).await.is_none());
        assert_eq!(cache.get(&"b".to_string()).await, Some(6));
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn touch_moves_to_back() {
        let cache = LruCache::new(10, |_: &String, v: &usize| {
            u32::try_from(*v).expect("test weight fits u32")
        });
        cache.insert("a".to_string(), 5).await;
        cache.insert("b".to_string(), 5).await;
        let _ = cache.get(&"a".to_string()).await;
        cache.insert("c".to_string(), 5).await;
        assert!(cache.get(&"b".to_string()).await.is_none());
        assert!(cache.get(&"a".to_string()).await.is_some());
        assert!(cache.get(&"c".to_string()).await.is_some());
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn remove_clears_weight() {
        let cache = LruCache::new(10, |_: &String, v: &usize| {
            u32::try_from(*v).expect("test weight fits u32")
        });
        cache.insert("a".to_string(), 6).await;
        cache.remove(&"a".to_string()).await;
        assert!(cache.get(&"a".to_string()).await.is_none());
        cache.insert("b".to_string(), 6).await;
        cache.insert("c".to_string(), 4).await;
        assert_eq!(cache.get(&"b".to_string()).await, Some(6));
        assert_eq!(cache.get(&"c".to_string()).await, Some(4));
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn scan_does_not_flush_hot_entries() {
        let cache = LruCache::new(10, |_: &String, v: &usize| {
            u32::try_from(*v).expect("test weight fits u32")
        });
        cache.insert("h1".to_string(), 3).await;
        cache.insert("h2".to_string(), 3).await;
        let _ = cache.get(&"h1".to_string()).await;
        let _ = cache.get(&"h2".to_string()).await;
        // A one-hit flood larger than the cache: plain `LRU` would evict both
        // hot entries, `S3-FIFO` absorbs it in the small queue + ghost.
        for i in 1..=5 {
            cache.insert(format!("s{i}"), 2).await;
        }
        assert!(cache.get(&"h1".to_string()).await.is_some());
        assert!(cache.get(&"h2".to_string()).await.is_some());
        assert!(cache.get(&"s1".to_string()).await.is_none());
        assert!(cache.get(&"s2".to_string()).await.is_none());
        assert!(cache.get(&"s3".to_string()).await.is_none());
        assert!(cache.get(&"s4".to_string()).await.is_some());
        assert!(cache.get(&"s5".to_string()).await.is_some());
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn hot_entry_survives_loop_larger_than_cache() {
        let cache = LruCache::new(10, |_: &String, v: &usize| {
            u32::try_from(*v).expect("test weight fits u32")
        });
        cache.insert("hot".to_string(), 4).await;
        for _ in 0..3 {
            let _ = cache.get(&"hot".to_string()).await;
        }
        for i in 1..=4 {
            cache.insert(format!("x{i}"), 3).await;
        }
        assert!(cache.get(&"hot".to_string()).await.is_some());
        assert!(cache.get(&"x1".to_string()).await.is_none());
        assert!(cache.get(&"x2".to_string()).await.is_none());
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn ghost_hit_is_readmitted_to_main_queue() {
        let cache = LruCache::new(10, |_: &String, v: &usize| {
            u32::try_from(*v).expect("test weight fits u32")
        });
        cache.insert("a".to_string(), 6).await;
        cache.insert("b".to_string(), 6).await;
        assert!(cache.get(&"a".to_string()).await.is_none());
        // Reinserting a ghost-remembered key admits it to the main queue, so
        // it survives pressure that evicts the newer one-hit entry instead.
        cache.insert("a".to_string(), 6).await;
        assert_eq!(cache.get(&"a".to_string()).await, Some(6));
        assert!(cache.get(&"b".to_string()).await.is_none());
    }

    #[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    async fn contains_does_not_shelter_entries() {
        let cache = LruCache::new(10, |_: &String, v: &usize| {
            u32::try_from(*v).expect("test weight fits u32")
        });
        cache.insert("a".to_string(), 5).await;
        for _ in 0..3 {
            assert!(cache.contains(&"a".to_string()).await);
        }
        cache.insert("b".to_string(), 5).await;
        cache.insert("c".to_string(), 5).await;
        // Probing kept `a` cold, so it is the entry the pressure evicts.
        assert!(cache.get(&"a".to_string()).await.is_none());
        assert!(cache.get(&"b".to_string()).await.is_some());
        assert!(cache.get(&"c".to_string()).await.is_some());
    }
}
