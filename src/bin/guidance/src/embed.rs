//! Embedding-backend resolution from project config (L3).
//!
//! The `models.embed` role (e.g. `"llama:embed"`) names a provider scheme
//! plus model; the provider's `base_url` comes from the matching entry in
//! `providers`, dimensions from the top-level `"embed"` object (falling
//! back to the legacy flat `embedding_dims`). Anything unresolved — no
//! config, unknown scheme, missing provider, missing dims, bad URL —
//! resolves to `None`, and callers degrade through their existing
//! embedder-absent paths. Construction is offline (no dial); the network
//! only happens at embed time, so resolution itself is hermetic-safe.

use fluent_llm::embeddings::{EmbeddingProvider, create_embedding_provider};
use guidance_core::config::ProjectConfig;

/// Build the configured embedding provider, or `None` when no backend is
/// configured or resolvable. Never throws for configuration reasons.
#[must_use]
pub fn embedder_from_config(cfg: &ProjectConfig) -> Option<Box<dyn EmbeddingProvider>> {
    let model_ref = cfg
        .models
        .get("embed")
        .cloned()
        .or_else(|| cfg.embedding_model.clone())?;
    // Split `scheme:model` for the provider-table lookup only when the
    // scheme is a plain token; full forms (`custom:http://…`) travel
    // whole into the factory, which parses them itself.
    let (name, model, scheme) = match model_ref.split_once(':') {
        Some((scheme, model)) if !scheme.contains('/') => {
            (model_ref.clone(), Some(model.to_string()), scheme.to_string())
        }
        _ => (model_ref.clone(), None, model_ref.clone()),
    };
    let base_url = cfg.providers.get(&scheme).map(|p| p.base_url.clone());
    let dims = cfg
        .embed
        .as_ref()
        .and_then(|embed| embed.dims)
        .or(cfg.embedding_dims)
        .and_then(|dims| u32::try_from(dims).ok())?;
    let cache_limit = cfg
        .embed
        .as_ref()
        .and_then(|embed| embed.cache_limit)
        .or(cfg.embedding_cache_limit);
    match create_embedding_provider(&name, model.as_deref(), base_url.as_deref(), None, dims, cache_limit, None) {
        Ok(provider) => Some(provider),
        Err(error) => {
            tracing::warn!("embedding backend {model_ref} unusable: {error}");
            None
        }
    }
}
