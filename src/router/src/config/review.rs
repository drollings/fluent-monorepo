//! Async review configuration — the `review` section of `RouterConfig`.

use serde::{Deserialize, Serialize};

/// Async review configuration. `None` (the default)
/// disables the review worker and its endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReviewConfig {
    /// Model key for the review model — an **independent, more capable**
    /// tier so review is not self-review (§12.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_model: Option<String>,
    /// Bounded review-queue capacity (the `WorkerPool` queue cap).
    #[serde(default = "default_review_queue_capacity")]
    pub queue_capacity: usize,
    /// Credit granted up front to the review feed (backpressure, §12.6).
    #[serde(default = "default_review_credit_limit")]
    pub credit_limit: usize,
    /// Opt-in PII auto-enqueue: after a parse is
    /// recorded, PII-shaped spans detected on the request text enqueue a
    /// review candidate, bounded by the existing credit gate. `false` (the
    /// default) keeps today's behavior — review jobs only come from the
    /// explicit `POST /v1/sessions/{id}/review-parse` endpoint.
    #[serde(default)]
    pub auto_enqueue: bool,
    /// Policy-Linter flag threshold (0..1): a text token whose score against a
    /// policy rule clears this is flagged. Default 0.5. Only meaningful when a
    /// `ZeroShotTokenMatching` linter model is registered.
    #[serde(default = "default_review_pii_threshold")]
    pub pii_threshold: f64,
}

fn default_review_queue_capacity() -> usize {
    32
}

fn default_review_credit_limit() -> usize {
    16
}

fn default_review_pii_threshold() -> f64 {
    0.5
}
