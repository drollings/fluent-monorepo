//! Catalog-backed embedding-model factory plus the index-compatibility rule.
//!
//! One reference selects exactly one backend — the sanctioned per-model-slot
//! narrowing: backends are never fanned out, and the query path never sees
//! this dispatch (fuse-always applies downstream). Unknown references miss
//! the catalog; catalogued backends without a constructor here (ONNX,
//! static) report as unwired — their session owners bind them at the daemon
//! layer. Remote construction stays grant-free by design: authorization is
//! enforced by the [`crate::grants`] gate before first use, never here.
//!
//! Rebuild policy: an existing index is reusable unless the model identity
//! (provider/model/dimension/metric) or the endpoint changed. API keys and
//! devices never participate — rotating a key or switching devices must not
//! invalidate vectors.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::catalog::{EmbeddingBackend, get_embedding_model_catalog_entry};
use crate::embeddings::{EmbeddingError, EmbeddingProvider};
use crate::gguf::{Device, GgufDependencies, GgufEmbedding, GgufModelOptions, ProductionGgufResolver, UnboundGgufRuntime};
use crate::qwen::{Qwen37TextEmbedding, Qwen3VlEmbedding, QwenTextEmbeddingV4, QwenTextOptions};


/// Factory inputs. Only the selected backend reads its own fields: `api_key`
/// / `endpoint` feed remote Qwen, `model_cache_dir` / `device` feed local
/// GGUF.
pub struct CreateModelOptions {
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub model_cache_dir: Option<PathBuf>,
    pub device: Option<String>,
    pub timeout_ms: Option<u64>,
    pub extra_headers: HashMap<String, String>,
}

/// Build the single provider for one catalog reference.
pub fn create_embedding_model(
    reference: &str,
    options: &CreateModelOptions,
) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
    let entry = get_embedding_model_catalog_entry(reference)
        .ok_or_else(|| EmbeddingError::CatalogNotFound(reference.to_string()))?;
    match entry.backend {
        EmbeddingBackend::LlamaCpp => {
            let device = match options.device.as_deref() {
                None => Device::Cpu,
                Some(device) => Device::parse(device).ok_or_else(|| {
                    EmbeddingError::InvalidDevice(format!(
                        "unknown device '{device}' for {reference}"
                    ))
                })?,
            };
            Ok(Box::new(GgufEmbedding::new(
                entry,
                GgufModelOptions {
                    model_cache_dir: options.model_cache_dir.clone(),
                    device,
                },
                GgufDependencies {
                    loader: Arc::new(UnboundGgufRuntime),
                    resolver: Arc::new(ProductionGgufResolver::new()),
                },
            )?))
        }
        EmbeddingBackend::Qwen => {
            let qwen_options = QwenTextOptions {
                api_key: options.api_key.clone(),
                endpoint: options.endpoint.clone(),
                extra_headers: options.extra_headers.clone(),
                timeout_ms: options.timeout_ms,
            };
            if entry.kind == Some("multimodal") {
                return Ok(Box::new(Qwen3VlEmbedding::new(&qwen_options)?));
            }
            match entry.model {
                "text-embedding-v4" => Ok(Box::new(QwenTextEmbeddingV4::new(&qwen_options)?)),
                "qwen3.7-text-embedding" => Ok(Box::new(Qwen37TextEmbedding::new(&qwen_options)?)),
                _ => Err(EmbeddingError::BackendNotWired {
                    reference: reference.to_string(),
                    backend: "qwen",
                }),
            }
        }
        EmbeddingBackend::Model2Vec => Err(EmbeddingError::BackendNotWired {
            reference: reference.to_string(),
            backend: "model2vec",
        }),
        EmbeddingBackend::TransformersJs => Err(EmbeddingError::BackendNotWired {
            reference: reference.to_string(),
            backend: "transformers-js",
        }),
    }
}

// ---------------------------------------------------------------------------
// Index compatibility: rebuild iff model identity or endpoint changed.
// ---------------------------------------------------------------------------

/// Stored model identity for one index. Keys and devices are deliberately
/// absent: they never invalidate vectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingIdentity {
    pub provider: String,
    pub model: String,
    pub dimension: u32,
    pub metric: String,
    pub endpoint: Option<String>,
}

/// Which half of the identity moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildField {
    Model,
    Endpoint,
}

/// A required index rebuild, carrying both sides and the operator hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildMismatch {
    pub field: RebuildField,
    pub existing: String,
    pub requested: String,
    pub rebuild_command: String,
}

impl std::fmt::Display for RebuildMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.field {
            RebuildField::Model => write!(
                formatter,
                "Existing index uses a different embedding model (existing={} requested={}). Run \"{}\" to rebuild this index with the requested model.",
                self.existing, self.requested, self.rebuild_command
            ),
            RebuildField::Endpoint => write!(
                formatter,
                "Existing index uses a different embedding endpoint (existing={} requested={}). Run \"{}\" to rebuild this index with the requested endpoint.",
                self.existing, self.requested, self.rebuild_command
            ),
        }
    }
}

impl std::error::Error for RebuildMismatch {}

/// Assert a requested model can serve an existing index. `None` existing
/// (fresh index) always passes.
pub fn require_compatible_embedding(
    existing: Option<&EmbeddingIdentity>,
    requested: &EmbeddingIdentity,
    rebuild_command: &str,
) -> Result<(), RebuildMismatch> {
    let Some(existing) = existing else {
        return Ok(());
    };
    if existing.provider != requested.provider
        || existing.model != requested.model
        || existing.dimension != requested.dimension
        || existing.metric != requested.metric
    {
        return Err(RebuildMismatch {
            field: RebuildField::Model,
            existing: format!("{}/{}", existing.provider, existing.model),
            requested: format!("{}/{}", requested.provider, requested.model),
            rebuild_command: rebuild_command.to_string(),
        });
    }
    if existing.endpoint != requested.endpoint {
        return Err(RebuildMismatch {
            field: RebuildField::Endpoint,
            existing: existing.endpoint.clone().unwrap_or_else(|| "(default)".to_string()),
            requested: requested.endpoint.clone().unwrap_or_else(|| "(default)".to_string()),
            rebuild_command: rebuild_command.to_string(),
        });
    }
    Ok(())
}
