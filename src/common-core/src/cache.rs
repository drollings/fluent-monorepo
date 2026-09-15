// ─── Load Cache ────────────────────────────────────────────────────────────

use lru::LruCache;
use std::borrow::Borrow;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex};

use crate::sync::lock;

type LruLoadFn<K, V, E> = Box<dyn Fn(&K) -> Result<V, E> + Send + Sync>;

/// Outcome of one in-flight `load`.
///
/// Waiters share successes (`Ready`) and re-run the load individually on
/// `Failed` (errors are never cached, so error sharing would require an
/// `E: Clone` bound the API does not impose — the fallback is exactly the
/// pre-change behavior). `Pending` with no map entry means the loader
/// vanished (panic path cleaned up); waiters then fall back as well.
enum SlotState<V> {
    Pending,
    Ready(Arc<V>),
    Failed,
}

/// Per-key rendezvous for coalescing concurrent misses.
struct Slot<V> {
    state: Mutex<SlotState<V>>,
    ready: Condvar,
}

impl<V> Slot<V> {
    fn new() -> Self {
        Self {
            state: Mutex::new(SlotState::Pending),
            ready: Condvar::new(),
        }
    }

    fn publish(&self, state: SlotState<V>) {
        *lock(&self.state) = state;
        self.ready.notify_all();
    }
}

/// Removes one in-flight entry (and wakes its waiters) on scope exit —
/// normal return and loader panic alike. Panic-safety matters: without
/// cleanup, waiters on a dead loader's slot would block forever, which is
/// strictly worse than the pre-change behavior (where a panicking loader
/// affected no other thread).
struct Retire<'m, K, V>
where
    K: Eq + Hash,
{
    inflight: &'m Mutex<HashMap<K, Arc<Slot<V>>>>,
    slot: Arc<Slot<V>>,
    key: Option<K>,
}

impl<K, V> Drop for Retire<'_, K, V>
where
    K: Eq + Hash,
{
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            lock(self.inflight).remove(&key);
            self.slot.ready.notify_all();
        }
    }
}

/// A bounded, thread-safe get-or-load LRU cache.
///
/// Wraps a `Mutex<LruCache<K, V>>` plus a `load` closure invoked on a cache
/// miss. `get_or_load` keeps the hot path to a single lock acquisition on a
/// hit; on a miss it drops the lock, runs `load`, and re-acquires only to
/// insert the freshly loaded value.
///
/// Concurrent misses for one key coalesce: the first thread loads while the
/// rest wait on a per-key slot and share its success (no lock is ever held
/// across `load`, so distinct keys load in parallel). Loader errors are not
/// shared — each waiter falls back to its own load, exactly the pre-change
/// behavior — because errors are never cached and sharing them would impose
/// an `E: Clone` bound the API does not have.
///
/// Write-through consumers (caches filled explicitly via `insert`, never via
/// load-on-miss) can use the plain `get`/`insert`/`remove`/`contains`
/// accessors; the `load` closure is only ever invoked by `get_or_load`.
pub struct LoadCache<K, V, E> {
    inner: Mutex<LruCache<K, V>>,
    inflight: Mutex<HashMap<K, Arc<Slot<V>>>>,
    load: LruLoadFn<K, V, E>,
}

impl<K, V, E> LoadCache<K, V, E>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    /// Create a bounded cache holding up to `capacity` entries.
    ///
    /// `load` produces a value for a missing key (and may fail with `E`).
    /// Returns an error when `capacity` is zero.
    pub fn new(
        capacity: usize,
        load: impl Fn(&K) -> Result<V, E> + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let cap = NonZeroUsize::new(capacity)
            .ok_or_else(|| format!("cache capacity must be non-zero, got {capacity}"))?;
        Ok(Self {
            inner: Mutex::new(LruCache::new(cap)),
            inflight: Mutex::new(HashMap::new()),
            load: Box::new(load),
        })
    }

    /// Look up `key` without invoking the load closure. Returns a clone of the
    /// cached value, if present.
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        lock(&self.inner).get(key).cloned()
    }

    /// `true` when `key` is present in the cache (never loads on a miss).
    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get(key).is_some()
    }

    /// Insert `value` under `key`, replacing any existing entry.
    pub fn insert(&self, key: K, value: V) {
        lock(&self.inner).put(key, value);
    }

    /// Remove `key`, returning the evicted value if present.
    pub fn remove<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        lock(&self.inner).pop(key)
    }

    /// Look up `key`, loading and caching it on a miss via the `load` closure.
    ///
    /// Concurrent misses for one key run exactly one `load`: the election
    /// winner loads (re-checking the cache first, in case a staggered
    /// arrival populated it) while losers wait on the key's slot and share
    /// its success. Losers observe loader errors — or a vanished loader —
    /// as a fallback to their own individual load, the pre-change
    /// behavior. No lock is held across `load`, so distinct keys proceed
    /// in parallel and re-entrant `load` closures cannot self-deadlock on
    /// the cache mutex (they behave like concurrent misses).
    pub fn get_or_load(&self, key: K) -> Result<V, E> {
        if let Some(value) = self.get(&key) {
            return Ok(value);
        }
        // Elect exactly one loader per key via the in-flight map. The
        // election scope ends before any waiting/loading, so no guard is
        // held across the blocking sections below.
        enum Arrival<V> {
            Loader(Arc<Slot<V>>),
            Waiter(Arc<Slot<V>>),
        }
        let arrival = {
            let mut inflight = lock(&self.inflight);
            match inflight.entry(key.clone()) {
                Entry::Vacant(entry) => {
                    let slot = Arc::new(Slot::new());
                    entry.insert(Arc::clone(&slot));
                    Arrival::Loader(slot)
                }
                Entry::Occupied(entry) => Arrival::Waiter(Arc::clone(entry.get())),
            }
        };
        let slot = match arrival {
            Arrival::Waiter(slot) => return self.wait_for_slot(&key, &slot),
            Arrival::Loader(slot) => slot,
        };
        // Loader path. The retire guard removes the entry (and wakes
        // stragglers) on every exit, including a panicking `load`.
        let _retire = Retire {
            inflight: &self.inflight,
            slot: Arc::clone(&slot),
            key: Some(key.clone()),
        };
        // A staggered arrival may have populated the cache between the
        // outer miss and the election win — share it instead of reloading.
        if let Some(value) = self.get(&key) {
            slot.publish(SlotState::Ready(Arc::new(value.clone())));
            return Ok(value);
        }
        match (self.load)(&key) {
            Ok(value) => {
                self.insert(key, value.clone());
                let shared = Arc::new(value);
                slot.publish(SlotState::Ready(Arc::clone(&shared)));
                // Sole-owner fast path: no waiter cloned the outcome, so
                // return it without a second clone.
                Ok(Arc::try_unwrap(shared).unwrap_or_else(|s| s.as_ref().clone()))
            }
            Err(e) => {
                slot.publish(SlotState::Failed);
                Err(e)
            }
        }
    }

    /// Waiter path: share the in-flight load's success, or fall back to an
    /// individual load when the loader failed or vanished.
    fn wait_for_slot(&self, key: &K, slot: &Arc<Slot<V>>) -> Result<V, E> {
        let mut state = lock(&slot.state);
        loop {
            match &*state {
                SlotState::Ready(shared) => return Ok(shared.as_ref().clone()),
                SlotState::Failed => break,
                SlotState::Pending => {
                    let loader_gone = lock(&self.inflight)
                        .get(key)
                        .is_none_or(|s| !Arc::ptr_eq(s, slot));
                    if loader_gone {
                        break;
                    }
                    state = slot
                        .ready
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
        drop(state);
        let value = (self.load)(key)?;
        self.insert(key.clone(), value.clone());
        Ok(value)
    }

    /// Number of entries currently held.
    pub fn len(&self) -> usize {
        lock(&self.inner).len()
    }

    /// `true` when the cache holds no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The maximum number of entries before the least-recently-used entry is
    /// evicted.
    pub fn capacity(&self) -> usize {
        lock(&self.inner).cap().get()
    }
}

/// A bounded, thread-safe cache that stores `Arc<V>` values so `get` returns
/// an `Arc::clone` (atomic refcount bump) instead of a `V::clone` (e.g. a
/// `Vec<f32>` 768-dim clone). Wraps `LoadCache<K, Arc<V>, E>` and reuses its
/// LRU + eviction logic without a second implementation.
///
/// Additive: `LoadCache<K,V,E>` is unchanged; this is the hot-path
/// optimization for large `V` (embeddings, `ContentNode` snapshots).
pub struct ArcLoadCache<K, V, E>(LoadCache<K, std::sync::Arc<V>, E>);

impl<K, V, E> ArcLoadCache<K, V, E>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    /// Create a bounded `ArcLoadCache` holding up to `capacity` entries.
    pub fn new(
        capacity: usize,
        load: impl Fn(&K) -> Result<std::sync::Arc<V>, E> + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Ok(Self(LoadCache::new(capacity, load)?))
    }

    /// Look up `key`, returning an `Arc::clone` on hit (no `V::clone`).
    pub fn get<Q>(&self, key: &Q) -> Option<std::sync::Arc<V>>
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.0.get(key)
    }

    /// Insert `value` under `key` (wrapped in `Arc`).
    pub fn insert(&self, key: K, value: std::sync::Arc<V>) {
        self.0.insert(key, value);
    }

    /// Convenience: insert an owned `V` by wrapping it in `Arc`.
    pub fn insert_owned(&self, key: K, value: V) {
        self.0.insert(key, std::sync::Arc::new(value));
    }

    /// Remove `key`, returning the evicted `Arc<V>` if present.
    pub fn remove<Q>(&self, key: &Q) -> Option<std::sync::Arc<V>>
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.0.remove(key)
    }

    /// Look up `key`, loading and caching it on a miss via the `load` closure.
    pub fn get_or_load(&self, key: K) -> Result<std::sync::Arc<V>, E> {
        self.0.get_or_load(key)
    }

    /// Number of entries currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The maximum number of entries before eviction.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

// NOTE (ROADMAP_20260903_LLM M11): the LLM response cache
// (`CachedResponse`/`ResponseCache` with TTL lazy-eviction and the
// `"{model}:{sha256(request_json)}"` keying) lived here through M10 as
// deprecated byte-identical shims of `fluent_llm::cache`; M11 deleted them.
// The generic cache mechanism in this file (`LoadCache`, `ArcLoadCache`,
// the weighted-LRU eviction engine) stays — it is not LLM policy.

// ─── Weighted-LRU eviction engine ─────────────────────────────────────────
//
// The canonical "evict the largest × coldest until under budget" engine. The
// residency/admission loop (`InstancePool`) composes these three functions.
// `ColdSnapshotIndex::evict` (the router's TTL metadata sweep) is a *predicate*
// filter, not a byte-budget eviction, so it intentionally does not use this
// engine.

/// The maximum coldness used as an overflow guard, and the coldness assigned
/// to an entity that was never used (`last_used < 0`). ~35k years in seconds.
const COLD_CAP: i64 = 1 << 40;

/// Eviction priority score: `freed_bytes * coldness`, where coldness is
/// seconds since `last_used` (capped at `COLD_CAP`; an entity never used is
/// maximally cold). A "cost of keeping" heuristic: the unit whose resident
/// footprint times its idle time is largest is the most valuable to evict. It
/// makes big cold footprints (a model's weights) outrank small hot ones, so
/// memory pressure reclaims the largest chunks while a just-used entity scores
/// near zero and stays.
pub fn eviction_score(freed_bytes: u64, last_used: i64, now: i64) -> u64 {
    let coldness = if last_used < 0 {
        COLD_CAP
    } else {
        now.saturating_sub(last_used).clamp(1, COLD_CAP)
    };
    freed_bytes.saturating_mul(coldness as u64)
}

/// Order `candidates` best-eviction-first: score descending, then `last_used`
/// descending (the newer of two equal-scoring units is kept).
///
/// Returns the same candidates reordered (an owned `Vec<C>` — the caller
/// supplies the candidates it gathered and gets back an ordering it can feed
/// to [`evict_until_fit`]).
pub fn eviction_order<C>(
    candidates: Vec<C>,
    now: i64,
    freed_of: impl Fn(&C) -> u64,
    last_used_of: impl Fn(&C) -> i64,
) -> Vec<C> {
    let mut ordered = candidates;
    ordered.sort_by(|a, b| {
        eviction_score(freed_of(b), last_used_of(b), now)
            .cmp(&eviction_score(freed_of(a), last_used_of(a), now))
            .then_with(|| last_used_of(b).cmp(&last_used_of(a)))
    });
    ordered
}

/// Evict candidates (already in [`eviction_order`]) until `used <= budget` or
/// `batch` candidates have been evicted.
///
/// `evict(&candidate)` performs the actual eviction and returns the freed
/// bytes, or `None` when the eviction failed (not counted toward `batch`).
/// Returns the updated `used` total and the number of successful evictions.
pub async fn evict_until_fit<C, F, Fut>(
    mut used: u64,
    budget: u64,
    batch: usize,
    candidates: Vec<C>,
    evict: F,
) -> (u64, usize)
where
    F: Fn(&C) -> Fut,
    Fut: std::future::Future<Output = Option<u64>>,
{
    let mut evicted = 0usize;
    for candidate in candidates {
        if used <= budget || evicted >= batch {
            break;
        }
        if let Some(freed) = evict(&candidate).await {
            evicted += 1;
            used = used.saturating_sub(freed);
        }
    }
    (used, evicted)
}

pub struct Budget {
    pub total: u64,
    pub minimum_remaining: u64,
}
impl Budget {
    pub fn allocation_budget(&self) -> Option<u64> {
        if self.total == 0 && self.minimum_remaining == 0 {
            return None;
        }
        Some(self.total.saturating_sub(self.minimum_remaining))
    }
    pub fn is_over_budget(&self, used: u64) -> bool {
        if let Some(budget) = self.allocation_budget() {
            used > budget
        } else {
            false
        }
    }
}

