//! Bounded in-process cache (no persistence).
//!
//! `DecisionCache` composes `LoadCache` (bounded LRU + eviction) with
//! `blake3_hex` composite keys — no bespoke map, no second hash. The sync
//! `get_or_load` seam cannot drive the async network load, so the async
//! `get_or_decide` composes `get`/`insert` around the same eviction engine
//! and inserts only `Ok` decisions: `Unavailable`/`Decode` errors never
//! populate the cache (pinned by the must-not-cache controls).

use std::future::Future;

use fluent_concurrency::cache::AsyncLoadCache;

use crate::decision::{Decision, DecisionError};

/// One in-flight load is owned by `AsyncLoadCache` (singleflight
/// election, cancel-safe retire); this module owns only the key framing
/// (`CacheKeyInput`) and the admission policy (degraded never stored).
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

/// Bounded LRU cache of decisions keyed by [`CacheKeyInput::key`].
///
/// ```rust
/// use fluent_llm::decision::{Decision, DecisionCache, DecisionError};
/// use fluent_llm::client::block_on;
///
/// let cache = DecisionCache::try_new(16).expect("capacity");
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
    inner: AsyncLoadCache<String, Decision, DecisionError>,
}

impl DecisionCache {
    /// Create a cache holding up to `capacity` decisions.
    pub fn try_new(capacity: usize) -> Result<Self, String> {
        Ok(Self {
            inner: AsyncLoadCache::try_new(capacity)?,
        })
    }

    /// Look up `key`, running `decide` on a miss and caching only success.
    ///
    /// Singleflight election lives in `AsyncLoadCache`; the policy here is
    /// admission only: degraded successes are usable once, never stored
    /// (caching them would cement an uncalibrated prior as a result).
    pub async fn get_or_decide<Fut>(
        &self,
        key: String,
        decide: Fut,
    ) -> Result<Decision, DecisionError>
    where
        Fut: Future<Output = Result<Decision, DecisionError>> + Send,
    {
        if let Some(hit) = self.inner.get(&key) {
            if !hit.degraded {
                let mut decision = hit;
                decision.cached = true;
                return Ok(decision);
            }
        }
        let mut outcome = self
            .inner
            .get_or_compute_with(key, decide, |d| !d.degraded)
            .await?;
        outcome.cached = false;
        Ok(outcome)
    }

    /// Look up `key` without loading. Hits are marked `cached`.
    ///
    /// ```rust
    /// use fluent_llm::decision::DecisionCache;
    ///
    /// let cache = DecisionCache::try_new(4).expect("capacity");
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
