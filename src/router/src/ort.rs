//! Ort ONNX registry composition.
//!
//! The router is the composition root: it turns `ModelEntry.onnx` blocks into
//! a boot-built `OrtSessionRegistry`, validates exclusivity and task-vs-model
//! sanity at boot, and hands the registry to the server (management refusals,
//! chart embedder). The llama.cpp supervisor, sidecar, and `/instances` never
//! touch onnx models — they are served by the registry alone.
//!
//! This file is a thin facade: `registry` builds the session registry, `weights`
//! the `LlmWeights` adapters, `backends` the embedder/reranker/ChatBackend and
//! the `OnnxBackend` adapter, and `prefilter` the PII/encoder/entity scorers.
//! Re-exports keep every `crate::ort::X` path stable.

mod backends;
mod prefilter;
mod registry;
mod weights;

#[cfg(feature = "onnx")]
pub use self::backends::{onnx_chart_embedder, onnx_context_backend, ColbertChartReranker};
pub use self::backends::{
    onnx_chat_backend, onnx_colbert_reranker, onnx_pool_context, OnnxBackend,
};
#[cfg(all(test, feature = "onnx"))]
pub(crate) use self::backends::{OnnxChatBackend, OnnxLlmRunner};
pub use self::prefilter::{colbert_entity_scorer, nlp_encoder_fetch, pii_prefilter};
#[cfg(all(test, feature = "onnx"))]
pub(crate) use self::prefilter::{map_annotations, score_span};
pub use self::registry::{build_onnx_registry, disambiguation_overlays, OrtRegistry};
#[cfg(feature = "onnx")]
pub use self::weights::OnnxWeights;
pub use self::weights::{onnx_weights_impls, DEFAULT_SLEEP_IDLE_SECONDS};

#[cfg(test)]
#[path = "../tests/ort.rs"]
mod tests;
