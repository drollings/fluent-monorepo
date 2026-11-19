//! Async get-or-compute cache with singleflight miss coalescing.
//!
//! The async twin of `common_core::cache::LoadCache`: bounded LRU plus a
//! per-key election so simultaneous misses on one key run exactly one
//! `fut` while the rest wait on a `tokio::sync::Notify` slot and share its
//! success. Errors are never shared — each waiter falls back to its own
//! compute — and a cancelled leader wakes waiters into a fresh election.
//! Lives here (not `common-core`) because the rendezvous is
//! `tokio`-coupled; the sync `Condvar` version stays in `common-core`.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use common_core::sync::lock;
use lru::LruCache;

/// One in-flight load shared by concurrent misses on a key.
struct Inflight {
    notify: tokio::sync::Notify,
    published: AtomicBool,
}

impl Inflight {
    fn new() -> Self {
        Self {
            notify: tokio::sync::Notify::new(),
            published: AtomicBool::new(false),
        }
    }
}

/// Removes the leader's entry on scope exit — normal return, error, and
/// loader cancel alike — and wakes waiters so none strands. Removal is
/// conditional on identity: a successor election that already replaced the
/// entry is left alone.
struct Retire<'t, K>
where
    K: Eq + Hash,
{
    table: &'t Mutex<HashMap<K, Arc<Inflight>>>,
    key: K,
    shared: Arc<Inflight>,
}

impl<K> Drop for Retire<'_, K>
where
    K: Eq + Hash,
{
    fn drop(&mut self) {
        // `HashMap<K, _>` needs `K` by value to remove; `key` was cloned at
        // election time, so take it here. `Mutex` poisoning recovers via the
        // shared [`common_core::sync`] guard.
        let table = lock(self.table);
        let stale = table
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.shared));
        // Drop the read guard before mutating: re-lock for the removal.
        drop(table);
        if stale {
            lock(self.table).remove(&self.key);
        }
        self.shared.notify.notify_waiters();
    }
}

/// Bounded async LRU cache with singleflight miss coalescing.
pub struct AsyncLoadCache<K, V, E> {
    inner: Mutex<LruCache<K, V>>,
    inflight: Mutex<HashMap<K, Arc<Inflight>>>,
    // `E` is only named in `get_or_compute`; hold it so the type has a
    // single owner for the error without a loader closure.
    _error: std::marker::PhantomData<E>,
}

impl<K, V, E> AsyncLoadCache<K, V, E>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    /// Create a bounded cache holding up to `capacity` entries.
    pub fn try_new(capacity: usize) -> Result<Self, String> {
        let cap = common_core::capacity::nonzero_capacity(capacity, "cache")?;
        Ok(Self {
            inner: Mutex::new(LruCache::new(cap)),
            inflight: Mutex::new(HashMap::new()),
            _error: std::marker::PhantomData,
        })
    }

    /// Look up `key` without computing. Returns a clone on hit.
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        lock(&self.inner).get(key).cloned()
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

    /// Look up `key`, running `compute` on a miss with singleflight
    /// coalescing. See the module docs for the election contract.
    pub async fn get_or_compute<Fut>(&self, key: K, compute: Fut) -> Result<V, E>
    where
        Fut: std::future::Future<Output = Result<V, E>> + Send,
    {
        self.get_or_compute_with(key, compute, |_| true).await
    }

    /// Look up `key`, running `compute` on a miss, caching only values the
    /// `admit` predicate accepts. Rejected values are usable once by this
    /// caller but never stored (e.g. degraded decisions).
    pub async fn get_or_compute_with<Fut>(
        &self,
        key: K,
        compute: Fut,
        admit: impl Fn(&V) -> bool,
    ) -> Result<V, E>
    where
        Fut: std::future::Future<Output = Result<V, E>> + Send,
    {
        loop {
            if let Some(hit) = self.get(&key) {
                return Ok(hit);
            }
            let shared = match lock(&self.inflight).entry(key.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let shared = Arc::new(Inflight::new());
                    entry.insert(Arc::clone(&shared));
                    None
                }
                std::collections::hash_map::Entry::Occupied(entry) => {
                    Some(Arc::clone(entry.get()))
                }
            };
            if let Some(shared) = shared {
                let notified = shared.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if shared.published.load(Ordering::Acquire) {
                    continue;
                }
                notified.await;
                continue;
            }
            let shared = lock(&self.inflight)
                .get(&key)
                .map(Arc::clone)
                .expect("leader entry");
            let _retire = Retire {
                table: &self.inflight,
                key: key.clone(),
                shared: Arc::clone(&shared),
            };
            if let Some(hit) = self.get(&key) {
                shared.published.store(true, Ordering::Release);
                return Ok(hit);
            }
            match compute.await {
                Ok(value) => {
                    if admit(&value) {
                        self.insert(key, value.clone());
                    }
                    shared.published.store(true, Ordering::Release);
                    return Ok(value);
                }
                outcome => {
                    shared.published.store(true, Ordering::Release);
                    return outcome;
                }
            }
        }
    }

    /// Number of entries currently held.
    pub fn len(&self) -> usize {
        lock(&self.inner).len()
    }

    /// `true` when the cache holds no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The maximum number of entries before LRU eviction.
    pub fn capacity(&self) -> usize {
        lock(&self.inner).cap().get()
    }
}
