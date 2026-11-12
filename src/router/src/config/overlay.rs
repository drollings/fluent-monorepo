//! Async overlay configuration — the `overlay` section of `RouterConfig` —
//! plus the canonical sampling-`params` merge every params layer composes
//! through.

use serde::{Deserialize, Serialize};

/// Async overlay configuration. `None` (the default)
/// disables the overlay worker and its request-path hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
/// Async overlay configuration. `None` (the default)
/// disables the overlay worker and its request-path hook.
pub struct OverlayConfig {
    /// Opt-in entity-link overlay (M6.2): when `true`, the request path submits
    /// `EntityLink` residuals (unresolved PROPN spans) to a credit-gated
    /// worker that scores them against boot-cached concept-label embeddings and
    /// writes candidates to `overlay_candidates` (never a doc-id write).
    #[serde(default)]
    pub entity_link_enabled: bool,
    /// Minimum cosine similarity for an entity-link candidate to be accepted.
    #[serde(default = "default_entity_link_threshold")]
    pub entity_link_threshold: f64,
    /// Opt-in parse-time entity promotion (M16): when `true`, accepted
    /// entity-link candidates stamp parse-time ids through the calibrated
    /// `EntityStampGate`. `false` (default) keeps the candidates-only plane —
    /// nothing is ever stamped.
    #[serde(default)]
    pub entity_promotion_enabled: bool,
    /// Bounded overlay-queue capacity (the `WorkerPool` queue cap).
    #[serde(default = "default_overlay_queue_capacity")]
    pub queue_capacity: usize,
    /// Credit granted up front to the overlay feed (backpressure).
    #[serde(default = "default_overlay_credit_limit")]
    pub credit_limit: usize,
    /// The opt-in `arc_ready` annotation-overlay sub-config (OVERLAYS §8). `None`
    /// (the default) leaves the three arc_ready overlays off — byte-identical to
    /// a deployment with no `overlay.arc_ready` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_ready: Option<ArcReadyConfig>,
}

/// The `arc_ready` annotation-overlay configuration (OVERLAYS §8). Opt-in and
/// additive: every field defaults to off/absent so a config with no
/// `overlay.arc_ready` block is byte-identical to a deployment that never
/// mentions it. The numeric knobs mirror the overlay worker's `OverlayWorkerConfig`
/// defaults (the `CreditedFeedWorker` load-bearing constants).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
/// The `arc_ready` annotation-overlay configuration (OVERLAYS §8). Opt-in and
/// additive: every field defaults to off/absent so a config with no
/// `overlay.arc_ready` block is byte-identical to a deployment that never
/// mentions it. The numeric knobs mirror the overlay worker's `OverlayWorkerConfig`
/// defaults (the `CreditedFeedWorker` load-bearing constants).
pub struct ArcReadyConfig {
    /// Master switch. `false` (the default) leaves the overlays off.
    #[serde(default)]
    pub enabled: bool,
    /// Whether the spacy pipeline overlay is requested. `false` (the default)
    /// means the `NlpPipeline` seam is not wired → the spacy overlay is
    /// fail-open `Ok(None)`.
    #[serde(default)]
    pub nlp: bool,
    /// Name of the `models` key whose `ChatBackend` drives the LLM enrichment
    /// overlay. `None` (the default) → the LLM overlay is off.
    #[serde(default)]
    pub llm_model: Option<String>,
    /// Name of the `models` key whose `EmbeddingProvider` drives the embedding
    /// overlay. `None` (the default) → the embedding overlay is off.
    #[serde(default)]
    pub embedding_model: Option<String>,
    /// Bounded pending-node feed capacity (the `CreditedFeedWorker` `mpsc` bound).
    #[serde(default = "default_arc_ready_queue_capacity")]
    pub queue_capacity: usize,
    /// Credit granted up front to the feed's producer (backpressure).
    #[serde(default = "default_arc_ready_credit_limit")]
    pub credit_limit: usize,
    /// How many processed nodes the consumer waits for before bumping credit.
    #[serde(default = "default_arc_ready_credit_more_after")]
    pub credit_more_after: usize,
    /// Max concurrent node-derivations (the `Limiter` cap).
    #[serde(default = "default_arc_ready_max_concurrent")]
    pub max_concurrent: usize,
    /// Whether to boot-backfill nodes already missing an overlay. `false` (the
    /// default) leaves boot behavior unchanged (nodes enqueue on create only).
    #[serde(default)]
    pub backfill: bool,
}

const fn default_arc_ready_queue_capacity() -> usize {
    1024
}

const fn default_arc_ready_credit_limit() -> usize {
    256
}

const fn default_arc_ready_credit_more_after() -> usize {
    8
}

const fn default_arc_ready_max_concurrent() -> usize {
    8
}

fn default_entity_link_threshold() -> f64 {
    0.6
}

fn default_overlay_queue_capacity() -> usize {
    32
}

fn default_overlay_credit_limit() -> usize {
    16
}

/// Overlay one sampling `params` object over a base (the overlay wins),
/// returning the merged object. Objects merge per key at the top level
/// only — a nested object on both sides is replaced wholesale, never
/// deep-merged (there is no `params`-inside-`params` layer anywhere in the
/// config); non-object sides degrade to an empty object (nothing to merge —
/// the `warn_on_non_object_params` boot pass reports those loudly). This is
/// the single canonical params merge: every layer of the
/// `models.default` → model → role → pool profile → binding chain composes
/// through it. The matching profile is looked up by name-or-group so both
/// the exact-instance and group dispatch paths reach the same value.
pub fn overlay_params(
    base: Option<&serde_json::Value>,
    over: Option<&serde_json::Value>,
) -> serde_json::Value {
    use serde_json::Value;
    fn as_object_or_empty(v: &Value) -> Value {
        v.as_object()
            .map_or_else(empty_params_object, |m| Value::Object(m.clone()))
    }
    match (base, over) {
        (Some(b), Some(o)) => match (b.as_object(), o.as_object()) {
            (Some(base_map), Some(over_map)) => {
                let mut merged = base_map.clone();
                for (key, over_value) in over_map {
                    merged.insert(key.clone(), over_value.clone());
                }
                Value::Object(merged)
            }
            // A non-object side contributes nothing (reported loudly by
            // `warn_on_non_object_params`); the object side survives whole.
            (Some(base_map), None) => Value::Object(base_map.clone()),
            (None, Some(over_map)) => Value::Object(over_map.clone()),
            (None, None) => empty_params_object(),
        },
        (Some(b), None) => as_object_or_empty(b),
        (None, Some(o)) => as_object_or_empty(o),
        (None, None) => empty_params_object(),
    }
}

fn empty_params_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}