//! Frame-resolution worker configuration — the `frame_resolution` section of
//! `RouterConfig`.

use serde::{Deserialize, Serialize};

/// Frame-resolution worker configuration. `None` (the default, i.e. no
/// `frame_resolution` block) leaves the worker unbuilt — byte-identical to a
/// deployment without it. `Some` composes the worker behind its runtime gate
/// (open at boot, closable via `POST /admin/frame/rollback`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Frame-resolution worker configuration. `None` (the default, i.e. no
/// `frame_resolution` block) leaves the worker unbuilt — byte-identical to a
/// deployment without it. `Some` composes the worker behind its runtime gate
/// (open at boot, closable via `POST /admin/frame/rollback`).
pub struct FrameResolutionConfig {
    /// Model key for the resolution model (batched, grammar-shaped JSON).
    /// Falls back to the classifier key, then the onnx LLM key when absent —
    /// no key resolvable means the worker is skipped (fail-open).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Bounded frame-resolution queue capacity (the pool queue cap).
    #[serde(default = "default_frame_queue_capacity")]
    pub queue_capacity: usize,
    /// Credit granted up front to the frame feed (backpressure).
    #[serde(default = "default_frame_credit_limit")]
    pub credit_limit: usize,
}

fn default_frame_queue_capacity() -> usize {
    32
}

fn default_frame_credit_limit() -> usize {
    16
}