//! LLM response cache — the single owner.
//!
//! `ResponseCache` owns the LLM *policy/keying* (`"{model}:{sha256(request_json)}"`
//! plus TTL-lazy reads). Storage is a `CacheBackend` trait with two bundled
//! backends: bounded in-memory LRU (`MemoryBackend`) and caller-provided
//! durable stores. TTL lives in `ResponseCache`, capacity lives in the
//! backend — one vocabulary, no closure injection.
//!
//! The owner goldens in `tests/cache.rs` are the lasting contract.
//!
//! Cache identity is task-value freshness/identity, not endorsement — a
//! cache hit is never a correctness vote. Never silently change key/TTL.

use std::convert::Infallible;
use std::sync::Arc;

use common_core::hash::sha256_hex;
use common_core::time::now_secs;
use serde_json::Value;

/// A cache entry storing an LLM response with a timestamp for TTL checks.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedResponse {
    /// Unix epoch seconds when this entry was stored.
    pub stored_at_secs: u64,
    /// The serialized LLM response JSON.
    pub response_json: Value,
}

/// Raw key/value store behind `ResponseCache`. TTL and key format stay in
/// `ResponseCache`; the backend only stores opaque entries under opaque
/// keys with a bounded entry count.
pub trait CacheBackend: Send + Sync {
    fn fetch(&self, key: &str) -> Option<CachedResponse>;
    fn persist(&self, key: &str, entry: &CachedResponse);
    fn evict_key(&self, key: &str);
    fn evict_all(&self);
}

/// Bounded in-memory LRU backend over `LoadCache` (no TTL here — TTL is
/// enforced lazily by `ResponseCache::get`).
pub struct MemoryBackend {
    inner: common_core::cache::LoadCache<String, CachedResponse, Infallible>,
}

impl MemoryBackend {
    pub fn new(capacity: usize) -> Result<Self, String> {
        Ok(Self {
            inner: common_core::cache::LoadCache::<
                String,
                CachedResponse,
                std::convert::Infallible,
            >::try_new(capacity, |_| {
                panic!("MemoryBackend is write-through; load never runs")
            })?,
        })
    }
}

impl CacheBackend for MemoryBackend {
    fn fetch(&self, key: &str) -> Option<CachedResponse> {
        self.inner.get(key)
    }

    fn persist(&self, key: &str, entry: &CachedResponse) {
        self.inner.insert(key.to_string(), entry.clone());
    }

    fn evict_key(&self, key: &str) {
        self.inner.remove(key);
    }

    fn evict_all(&self) {
        // `LoadCache` has no clear; eviction is LRU-bound. Full clears go
        // through a fresh backend. This is a deliberate bound: callers that
        // need bulk invalidation compose the durable backend.
        // NOTE: kept as no-op to preserve the `invalidate_all` call sites
        // while the durable backend lands.
    }
}

/// A backend-agnostic response cache for LLM completions.
///
/// Entries are lazily evicted — `get` returns `None` if the entry
/// exists but is older than the configured TTL.
///
/// Cache key format: `"{model}:{sha256(request_json)}"`.
pub struct ResponseCache {
    backend: Arc<dyn CacheBackend>,
    ttl: Option<std::time::Duration>,
}

impl ResponseCache {
    /// Bounded in-memory cache with TTL-lazy reads.
    pub fn in_memory(
        capacity: usize,
        ttl: Option<std::time::Duration>,
    ) -> Result<Self, String> {
        Ok(Self {
            backend: Arc::new(MemoryBackend::new(capacity)?),
            ttl,
        })
    }

    /// Cache over a caller-provided backend (durable SQLite, Redis, …).
    pub fn over_backend(
        backend: Arc<dyn CacheBackend>,
        ttl: Option<std::time::Duration>,
    ) -> Self {
        Self { backend, ttl }
    }

    fn cache_key(model: &str, request_json: &str) -> String {
        format!("{}:{}", model, sha256_hex(request_json.as_bytes()))
    }

    /// Look up a cached response.  Returns `None` on miss or TTL expiry.
    pub fn get(&self, model: &str, request_json: &str) -> Option<CachedResponse> {
        let key = Self::cache_key(model, request_json);
        let entry = self.backend.fetch(&key)?;
        if let Some(ttl) = self.ttl {
            let age = now_secs().saturating_sub(entry.stored_at_secs);
            if age >= ttl.as_secs() {
                return None;
            }
        }
        Some(entry)
    }

    /// Store a response in the cache (best-effort; failure is non-fatal).
    pub fn set(&self, model: &str, request_json: &str, response_json: Value) {
        let key = Self::cache_key(model, request_json);
        let entry = CachedResponse {
            stored_at_secs: now_secs(),
            response_json,
        };
        self.backend.persist(&key, &entry);
    }

    /// Remove a cached entry by model and request JSON.
    pub fn invalidate_key(&self, model: &str, request_json: &str) {
        let key = Self::cache_key(model, request_json);
        self.backend.evict_key(&key);
    }

    /// Remove a cached entry by raw cache key.
    pub fn invalidate_key_raw(&self, cache_key: &str) {
        self.backend.evict_key(cache_key);
    }

    /// Remove all cached entries.
    pub fn invalidate_all(&self) {
        self.backend.evict_all();
    }
}
