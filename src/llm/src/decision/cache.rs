//! Bounded in-process cache (no persistence).
//!
//! `DecisionCache` composes `LoadCache` (bounded LRU + eviction) with
//! `blake3_hex` composite keys — no bespoke map, no second hash. The sync
//! `get_or_load` seam cannot drive the async network load, so the async
//! `get_or_decide` composes `get`/`insert` around the same eviction engine
//! and inserts only `Ok` decisions: `Unavailable`/`Decode` errors never
//! populate the cache (pinned by the must-not-cache controls).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use common_core::sync::lock;

use crate::decision::{Decision, DecisionError};

/// One in-flight load shared by concurrent misses on a key.
struct Inflight {
    notify: tokio::sync::Notify,
    result: Mutex<Option<Result<Decision, DecisionError>>>,
}

impl Inflight {
    fn new() -> Self {
        Self {
            notify: tokio::sync::Notify::new(),
            result: Mutex::new(None),
        }
    }
}

/// Removes the leader's entry on scope exit — normal return, error, and
/// loader cancel alike — and wakes waiters so none strands. Removal is
/// conditional on identity: a successor election that already replaced
/// the entry is left alone.
struct Retire<'t> {
    table: &'t Mutex<HashMap<String, Arc<Inflight>>>,
    key: String,
    shared: Arc<Inflight>,
}

impl Drop for Retire<'_> {
    fn drop(&mut self) {
        let mut table = lock(self.table);
        let stale = table
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.shared));
        if stale {
            table.remove(&self.key);
        }
        self.shared.notify.notify_waiters();
    }
}

/// Composite cache-key input: every identity axis in one struct so the key
/// can grow (fingerprint today) without another signature break. `key()`
/// frames each part with its length; an absent fingerprint contributes
/// nothing, so legacy callers hash exactly as before.
///
/// ```rust
/// use fluent_llm::decision::CacheKeyInput;
///
/// let input = CacheKeyInput {
///     state: "s".to_string(),
///     question: "q?".to_string(),
///     question_id: "id".to_string(),
///     version: 1,
///     model: "m".to_string(),
///     endpoint: "http://a".to_string(),
///     extras: serde_json::json!({}),
///     think: None,
///     fingerprint: None,
/// };
/// assert_eq!(input.key().len(), 64);
/// ```
#[derive(Debug, Clone)]
/// Composite cache-key input: every identity axis in one struct so the key
/// can grow (fingerprint today) without another signature break. `key()`
/// frames each part with its length; an absent fingerprint contributes
/// nothing, so legacy callers hash exactly as before.
/// 
/// ```rust
/// use fluent_llm::decision::CacheKeyInput;
/// 
/// let input = CacheKeyInput {
/// state: "s".to_string(),
/// question: "q?".to_string(),
/// question_id: "id".to_string(),
/// version: 1,
/// model: "m".to_string(),
/// endpoint: "http://a".to_string(),
/// extras: serde_json::json!({}),
/// think: None,
/// fingerprint: None,
/// };
/// assert_eq!(input.key().len(), 64);
/// ```
pub struct CacheKeyInput {
    pub state: String,
    pub question: String,
    pub question_id: String,
    pub version: u64,
    pub model: String,
    pub endpoint: String,
    pub extras: serde_json::Value,
    pub think: Option<bool>,
    /// Canary fingerprint of the weights actually probed (`None` = unscoped).
    pub fingerprint: Option<String>,
}

impl CacheKeyInput {
    /// Hash the framed identity tuple.
    #[must_use]
    pub fn key(&self) -> String {
        let extras_json = serde_json::to_string(&self.extras).unwrap_or_default();
        let think_byte = match self.think {
            None => "n",
            Some(true) => "t",
            Some(false) => "f",
        };
        let mut bytes = Vec::new();
        let mut framed = |part: &[u8]| {
            bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
            bytes.extend_from_slice(part);
        };
        framed(self.state.as_bytes());
        framed(self.question.as_bytes());
        framed(self.question_id.as_bytes());
        framed(extras_json.as_bytes());
        framed(self.endpoint.as_bytes());
        if let Some(fingerprint) = &self.fingerprint {
            framed(fingerprint.as_bytes());
        }
        bytes.extend_from_slice(&self.version.to_le_bytes());
        bytes.extend_from_slice(&(self.model.len() as u64).to_le_bytes());
        bytes.extend_from_slice(self.model.as_bytes());
        bytes.extend_from_slice(think_byte.as_bytes());
        common_core::hash::blake3_hex(&bytes)
    }
}

/// Legacy constructor: identical bytes to the struct form with no
/// fingerprint. Prefer `CacheKeyInput` for new callers.
///
/// ```rust
/// use fluent_llm::decision::decision_cache_key;
///
/// let extras = serde_json::json!({});
/// let a = decision_cache_key("s", "q?", "id", 1, "m", "http://a", &extras, None);
/// let b = decision_cache_key("s", "q!", "id", 1, "m", "http://a", &extras, None);
/// assert_ne!(a, b);
/// ```
#[must_use]
#[allow(clippy::too_many_arguments)]
/// Legacy constructor: identical bytes to the struct form with no
/// fingerprint. Prefer `CacheKeyInput` for new callers.
/// 
/// ```rust
/// use fluent_llm::decision::decision_cache_key;
/// 
/// let extras = serde_json::json!({});
/// let a = decision_cache_key("s", "q?", "id", 1, "m", "http://a", &extras, None);
/// let b = decision_cache_key("s", "q!", "id", 1, "m", "http://a", &extras, None);
/// assert_ne!(a, b);
/// ```
pub fn decision_cache_key(
    state: &str,
    question: &str,
    question_id: &str,
    version: u64,
    model: &str,
    endpoint: &str,
    extras: &serde_json::Value,
    think: Option<bool>,
) -> String {
    CacheKeyInput {
        state: state.to_string(),
        question: question.to_string(),
        question_id: question_id.to_string(),
        version,
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        extras: extras.clone(),
        think,
        fingerprint: None,
    }
    .key()
}

/// Bounded LRU cache of decisions keyed by [`decision_cache_key`].
///
/// ```rust
/// use fluent_llm::decision::{Decision, DecisionCache, DecisionError};
/// use fluent_llm::client::block_on;
///
/// let cache = DecisionCache::new(16).expect("capacity");
/// let decision = block_on(cache.get_or_decide("k".to_string(), async {
///     Ok::<_, DecisionError>(Decision::from_distribution(
///         "A",
///         vec![("A".to_string(), 1.0)],
///         "m".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"))
/// }))
/// .expect("load");
/// assert!(!decision.cached);
/// ```
pub struct DecisionCache {
    inner: common_core::cache::LoadCache<String, Decision, DecisionError>,
    inflight: Mutex<HashMap<String, Arc<Inflight>>>,
}

impl DecisionCache {
    /// Create a cache holding up to `capacity` decisions.
    pub fn new(capacity: usize) -> Result<Self, String> {
        Ok(Self {
            inner: common_core::cache::LoadCache::new(capacity, |_| {
                Err(DecisionError::Unavailable(
                    "sync load unsupported; use get_or_decide".to_string(),
                ))
            })?,
            inflight: Mutex::new(HashMap::new()),
        })
    }

    /// Look up `key`, running `decide` on a miss and caching only success.
    ///
    /// Simultaneous misses on one key coalesce: the first caller loads
    /// while the rest wait on the in-flight slot and share its success
    /// (re-checking the cache first, so a staggered arrival wins without
    /// loading). Errors are never shared — each waiter falls back to its
    /// own load, exactly the pre-change behavior — and a canceled leader
    /// wakes its waiters into a fresh election instead of stranding them.
    pub async fn get_or_decide<Fut>(
        &self,
        key: String,
        decide: Fut,
    ) -> Result<Decision, DecisionError>
    where
        Fut: Future<Output = Result<Decision, DecisionError>> + Send,
    {
        loop {
            if let Some(hit) = self.inner.get(&key) {
                let mut decision = hit;
                decision.cached = true;
                return Ok(decision);
            }
            // Elect exactly one loader per key; no lock is held across the
            // blocking sections below.
            let shared = match lock(&self.inflight).entry(key.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let shared = Arc::new(Inflight::new());
                    entry.insert(Arc::clone(&shared));
                    None
                }
                std::collections::hash_map::Entry::Occupied(entry) => Some(Arc::clone(entry.get())),
            };
            if let Some(shared) = shared {
                // Waiter: share the outcome when published, else re-loop
                // into a fresh election (leader failed or vanished).
                let notified = shared.notify.notified();
                if lock(&shared.result).is_some() {
                    continue;
                }
                notified.await;
                continue;
            }
            // Leader path. The retire guard removes the entry (and wakes
            // stragglers) on every exit, including a cancelled `decide`.
            let shared = lock(&self.inflight)
                .get(&key)
                .map(Arc::clone)
                .expect("leader entry");
            let _retire = Retire {
                table: &self.inflight,
                key: key.clone(),
                shared: Arc::clone(&shared),
            };
            // A staggered arrival may have populated the cache between the
            // outer miss and the election win.
            if let Some(hit) = self.inner.get(&key) {
                *lock(&shared.result) = Some(Ok(hit.clone()));
                let mut decision = hit;
                decision.cached = true;
                return Ok(decision);
            }
            match decide.await {
                // Degraded successes are usable once, never stored: caching
                // them would cement an uncalibrated prior as a result.
                Ok(decision) if !decision.degraded => {
                    self.inner.insert(key, decision.clone());
                    *lock(&shared.result) = Some(Ok(decision.clone()));
                    return Ok(decision);
                }
                outcome => {
                    *lock(&shared.result) = Some(outcome.clone());
                    return outcome;
                }
            }
        }
    }

    /// Look up `key` without loading. Hits are marked `cached`.
    ///
    /// ```rust
    /// use fluent_llm::decision::DecisionCache;
    ///
    /// let cache = DecisionCache::new(4).expect("capacity");
    /// assert!(cache.get("missing").is_none());
    /// assert_eq!(cache.len(), 0);
    /// assert!(cache.is_empty());
    /// ```
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Decision> {
        self.inner.get(key).map(|mut hit| {
            hit.cached = true;
            hit
        })
    }

    /// Insert `decision` under `key`, replacing any existing entry.
    pub fn insert(&self, key: String, decision: Decision) {
        self.inner.insert(key, decision);
    }

    /// Number of entries currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The maximum entries before LRU eviction.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }
}