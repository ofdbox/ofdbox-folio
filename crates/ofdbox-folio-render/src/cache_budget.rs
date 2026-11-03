//! Shared decoded-image byte budgets across render backends.
//!
//! Each backend decodes images into its own resource type (CPU pixmaps,
//! tiny-skia pixmaps and wgpu textures), so their caches cannot be pooled. The
//! byte *budget* can be: several caches charge the decoded bytes they retain to
//! one [`CacheBudget`], and once the combined total exceeds the limit the oldest
//! charges are evicted across all participating caches. A paging session that
//! alternates between backends therefore stays within one memory limit instead
//! of one limit per backend.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Identifier of one cache entry charged to a [`CacheBudget`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheChargeId(u64);

/// One charged entry: decoded bytes plus the callback that evicts its owner.
struct Charge {
    bytes: u64,
    evict: Box<dyn FnOnce() + Send>,
}

struct BudgetInner {
    limit: u64,
    next_id: AtomicU64,
    state: Mutex<BudgetState>,
}

#[derive(Default)]
struct BudgetState {
    used: u64,
    order: VecDeque<CacheChargeId>,
    charges: HashMap<CacheChargeId, Charge>,
}

/// A byte budget for decoded images that several render backends can share.
///
/// A [`CpuRenderer`](crate::CpuRenderer), a
/// [`TinySkiaRenderer`](crate::TinySkiaRenderer) and a
/// [`WgpuBackend`](crate::WgpuBackend) can charge their decoded-image caches to
/// the same budget. When the combined size exceeds the limit, the oldest
/// charges are evicted across all participating backends.
///
/// Clones refer to the same budget. A limit of 0 disables decoded-image caching
/// for every participating backend.
#[derive(Clone)]
pub struct CacheBudget {
    inner: Arc<BudgetInner>,
}

impl fmt::Debug for CacheBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.lock();
        f.debug_struct("CacheBudget")
            .field("limit", &self.inner.limit)
            .field("used", &state.used)
            .finish()
    }
}

impl CacheBudget {
    /// Creates a budget that keeps at most `limit_bytes` decoded image bytes in
    /// total across all participating caches.
    pub fn new(limit_bytes: u64) -> Self {
        Self {
            inner: Arc::new(BudgetInner {
                limit: limit_bytes,
                next_id: AtomicU64::new(1),
                state: Mutex::new(BudgetState::default()),
            }),
        }
    }

    /// The configured limit in bytes.
    pub fn limit(&self) -> u64 {
        self.inner.limit
    }

    /// The number of bytes currently charged by all participating caches.
    pub fn used(&self) -> u64 {
        self.lock().used
    }

    /// Charges `bytes` to the budget and returns a token for
    /// [`release`](Self::release).
    ///
    /// `evict` removes the owning cache entry. It runs immediately when the
    /// charge alone cannot fit the budget, and later when this charge is the
    /// oldest one over the limit. A `None` result means the entry was evicted
    /// right away and the caller must not keep it cached.
    pub fn charge(
        &self,
        bytes: u64,
        evict: impl FnOnce() + Send + 'static,
    ) -> Option<CacheChargeId> {
        let mut evict = Some(Box::new(evict) as Box<dyn FnOnce() + Send>);
        let limit = self.inner.limit;
        if limit == 0 || bytes == 0 || bytes > limit {
            if let Some(evict) = evict.take() {
                evict();
            }
            return None;
        }
        let id = CacheChargeId(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        {
            let mut state = self.lock();
            state.used = state.used.saturating_add(bytes);
            state.order.push_back(id);
            state.charges.insert(
                id,
                Charge {
                    bytes,
                    evict: evict.take().expect("charge callback"),
                },
            );
        }
        self.enforce_limit();
        if self.is_charged(id) { Some(id) } else { None }
    }

    /// Releases a charge without evicting its cache entry.
    ///
    /// Caches call this when they drop an entry themselves, for example when
    /// their own local budget evicts it.
    pub fn release(&self, id: CacheChargeId) {
        let mut state = self.lock();
        if let Some(charge) = state.charges.remove(&id) {
            state.used = state.used.saturating_sub(charge.bytes);
            state.order.retain(|value| *value != id);
        }
    }

    fn is_charged(&self, id: CacheChargeId) -> bool {
        self.lock().charges.contains_key(&id)
    }

    /// Evicts the oldest charges until the total fits the limit.
    ///
    /// Eviction callbacks run without the budget lock held: they lock the
    /// owning cache, and cache insert paths never call back into the budget
    /// while holding that lock, so the two locks are never nested.
    fn enforce_limit(&self) {
        loop {
            let evictions = {
                let mut state = self.lock();
                if state.used <= self.inner.limit {
                    return;
                }
                let mut evictions = Vec::new();
                while state.used > self.inner.limit {
                    let Some(id) = state.order.pop_front() else {
                        break;
                    };
                    if let Some(charge) = state.charges.remove(&id) {
                        state.used = state.used.saturating_sub(charge.bytes);
                        evictions.push(charge.evict);
                    }
                }
                evictions
            };
            if evictions.is_empty() {
                return;
            }
            for evict in evictions {
                evict();
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, BudgetState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

/// One decoded image retained by a backend cache.
struct CachedValue<V> {
    value: Arc<V>,
    bytes: u64,
    charge: Option<CacheChargeId>,
}

/// LRU cache of decoded images that can participate in a [`CacheBudget`].
///
/// The cache enforces its own `max_bytes` limit first and charges the shared
/// budget second; the shared budget may evict entries from any participating
/// cache, including this one. `max_bytes == 0` disables caching entirely.
pub(crate) struct SharedByteCache<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    entries: HashMap<K, CachedValue<V>>,
    order: VecDeque<K>,
    bytes: u64,
    max_bytes: u64,
    budget: Option<CacheBudget>,
}

impl<K, V> SharedByteCache<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    /// Creates an empty cache guarded by a mutex.
    pub(crate) fn new(max_bytes: u64, budget: Option<CacheBudget>) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            max_bytes,
            budget,
        }))
    }

    /// Looks up an entry and marks it as most recently used.
    pub(crate) fn get(state: &Arc<Mutex<Self>>, key: &K) -> Option<Arc<V>> {
        let mut cache = lock(state);
        cache.touch(key)
    }

    /// Stores a decoded image, evicting locally and then charging the shared
    /// budget.
    pub(crate) fn insert(state: &Arc<Mutex<Self>>, key: K, value: Arc<V>, bytes: u64) {
        let (released, budget) = {
            let mut cache = lock(state);
            if cache.max_bytes == 0 || bytes == 0 || bytes > cache.max_bytes {
                return;
            }
            let mut released = cache.remove_local(&key);
            while cache.bytes.saturating_add(bytes) > cache.max_bytes {
                let Some(oldest) = cache.order.pop_front() else {
                    break;
                };
                if let Some(entry) = cache.entries.remove(&oldest) {
                    cache.bytes = cache.bytes.saturating_sub(entry.bytes);
                    if let Some(charge) = entry.charge {
                        released.push(charge);
                    }
                }
            }
            cache.bytes = cache.bytes.saturating_add(bytes);
            cache.order.push_back(key.clone());
            cache.entries.insert(
                key.clone(),
                CachedValue {
                    value,
                    bytes,
                    charge: None,
                },
            );
            (released, cache.budget.clone())
        };
        let Some(budget) = budget else {
            return;
        };
        for id in released {
            budget.release(id);
        }
        let weak = Arc::downgrade(state);
        let charge_key = key.clone();
        let id = budget.charge(bytes, move || {
            if let Some(state) = weak.upgrade() {
                lock(&state).remove_charged(&charge_key);
            }
        });
        let Some(id) = id else {
            return;
        };
        let mut cache = lock(state);
        let present = cache.entries.contains_key(&key);
        if present {
            if let Some(entry) = cache.entries.get_mut(&key) {
                entry.charge = Some(id);
            }
        } else {
            drop(cache);
            budget.release(id);
        }
    }

    /// Drops every entry and releases their charges.
    pub(crate) fn clear(state: &Arc<Mutex<Self>>) {
        let (released, budget) = {
            let mut cache = lock(state);
            let released = cache
                .entries
                .drain()
                .filter_map(|(_, entry)| entry.charge)
                .collect::<Vec<_>>();
            cache.order.clear();
            cache.bytes = 0;
            (released, cache.budget.clone())
        };
        if let Some(budget) = budget {
            for id in released {
                budget.release(id);
            }
        }
    }

    /// Returns the number of cached images and their decoded bytes.
    pub(crate) fn stats(state: &Arc<Mutex<Self>>) -> (usize, u64) {
        let cache = lock(state);
        (cache.entries.len(), cache.bytes)
    }

    fn touch(&mut self, key: &K) -> Option<Arc<V>> {
        let value = Arc::clone(&self.entries.get(key)?.value);
        self.order.retain(|value| value != key);
        self.order.push_back(key.clone());
        Some(value)
    }

    /// Removes an entry and returns its charge token for the caller to release.
    fn remove_local(&mut self, key: &K) -> Vec<CacheChargeId> {
        let mut released = Vec::new();
        if let Some(entry) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
            self.order.retain(|value| value != key);
            if let Some(charge) = entry.charge {
                released.push(charge);
            }
        }
        released
    }

    /// Removes an entry already evicted by the shared budget.
    fn remove_charged(&mut self, key: &K) {
        if let Some(entry) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
            self.order.retain(|value| value != key);
        }
    }
}

fn lock<T>(state: &Arc<Mutex<T>>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}

impl<K, V> Drop for SharedByteCache<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    fn drop(&mut self) {
        let Some(budget) = &self.budget else {
            return;
        };
        // A cache can be dropped without clear(); release its charges so the
        // shared budget does not keep accounting for images that are gone.
        for entry in self.entries.values() {
            if let Some(charge) = entry.charge {
                budget.release(charge);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheBudget, SharedByteCache};
    use std::sync::{Arc, Mutex};

    type TestCache = Arc<Mutex<SharedByteCache<u32, u32>>>;

    fn cache(max_bytes: u64, budget: Option<CacheBudget>) -> TestCache {
        SharedByteCache::new(max_bytes, budget)
    }

    fn insert(cache: &TestCache, key: u32, bytes: u64) {
        SharedByteCache::insert(cache, key, Arc::new(key), bytes);
    }

    #[test]
    fn local_limit_evicts_the_least_recently_used_entry() {
        let cache = cache(800, None);
        insert(&cache, 1, 400);
        insert(&cache, 2, 400);
        assert!(SharedByteCache::get(&cache, &1).is_some());
        insert(&cache, 3, 400);
        assert!(SharedByteCache::get(&cache, &2).is_none());
        assert!(SharedByteCache::get(&cache, &1).is_some());
        assert!(SharedByteCache::get(&cache, &3).is_some());
        assert_eq!(SharedByteCache::stats(&cache), (2, 800));
    }

    #[test]
    fn zero_local_budget_disables_caching() {
        let cache = cache(0, None);
        insert(&cache, 1, 400);
        assert!(SharedByteCache::get(&cache, &1).is_none());
        assert_eq!(SharedByteCache::stats(&cache), (0, 0));
    }

    #[test]
    fn usage_is_charged_to_the_shared_budget() {
        let budget = CacheBudget::new(2000);
        let cache = cache(u64::MAX, Some(budget.clone()));
        insert(&cache, 1, 600);
        insert(&cache, 2, 600);
        assert_eq!(budget.used(), 1200);
        assert!(SharedByteCache::get(&cache, &1).is_some());
        assert!(SharedByteCache::get(&cache, &2).is_some());
    }

    #[test]
    fn evicts_the_oldest_charge_across_caches() {
        let budget = CacheBudget::new(1000);
        let first = cache(u64::MAX, Some(budget.clone()));
        let second = cache(u64::MAX, Some(budget.clone()));
        insert(&first, 1, 600);
        insert(&second, 2, 600);
        assert_eq!(budget.used(), 600);
        assert!(
            SharedByteCache::get(&first, &1).is_none(),
            "the oldest charge should be evicted from the first cache"
        );
        assert!(SharedByteCache::get(&second, &2).is_some());
    }

    #[test]
    fn clearing_a_cache_releases_its_charges() {
        let budget = CacheBudget::new(1000);
        let cache = cache(u64::MAX, Some(budget.clone()));
        insert(&cache, 1, 400);
        assert_eq!(budget.used(), 400);
        SharedByteCache::clear(&cache);
        assert_eq!(budget.used(), 0);
        assert_eq!(SharedByteCache::stats(&cache), (0, 0));
    }

    #[test]
    fn dropping_a_cache_releases_its_charges() {
        let budget = CacheBudget::new(1000);
        let cache = cache(u64::MAX, Some(budget.clone()));
        insert(&cache, 1, 400);
        assert_eq!(budget.used(), 400);
        drop(cache);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn replacing_an_entry_releases_the_previous_charge() {
        let budget = CacheBudget::new(1000);
        let cache = cache(u64::MAX, Some(budget.clone()));
        insert(&cache, 1, 600);
        insert(&cache, 1, 400);
        assert_eq!(budget.used(), 400);
        assert_eq!(SharedByteCache::stats(&cache), (1, 400));
    }

    #[test]
    fn a_zero_limit_budget_disables_caching() {
        let budget = CacheBudget::new(0);
        let cache = cache(u64::MAX, Some(budget.clone()));
        insert(&cache, 1, 400);
        assert!(SharedByteCache::get(&cache, &1).is_none());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn a_charge_larger_than_the_budget_is_not_retained() {
        let budget = CacheBudget::new(100);
        let cache = cache(u64::MAX, Some(budget.clone()));
        insert(&cache, 1, 200);
        assert!(SharedByteCache::get(&cache, &1).is_none());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn keeps_the_combined_usage_within_the_limit() {
        let budget = CacheBudget::new(1000);
        let first = cache(u64::MAX, Some(budget.clone()));
        insert(&first, 1, 400);
        insert(&first, 2, 400);
        let second = cache(u64::MAX, Some(budget.clone()));
        insert(&second, 3, 600);
        assert_eq!(budget.used(), 1000);
        assert_eq!(SharedByteCache::stats(&first).0, 1);
        assert_eq!(SharedByteCache::stats(&second).0, 1);
    }
}
