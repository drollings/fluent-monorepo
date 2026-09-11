//! Model-reference resolution order: explicit flag > existing index >
//! `GUIDANCE_EMBEDDING` environment > global default > builtin local
//! fallback. An environment value naming no catalog entry is a hard error,
//! never a silent fallback.

use std::collections::HashMap;

use crate::catalog::get_embedding_model_catalog_entry;
use crate::embeddings::EmbeddingError;

pub const EMBEDDING_ENVIRONMENT_VARIABLE: &str = "GUIDANCE_EMBEDDING";

/// Inputs to [`resolve_embedding_reference`]. `environment` carries the
/// process environment explicitly so hermetic callers pass a fixed map;
/// `None` reads the live process environment.
pub struct ResolveEmbeddingReferenceOptions {
    pub explicit: Option<String>,
    pub existing: Option<String>,
    pub global_default: Option<String>,
    pub environment: Option<HashMap<String, String>>,
    pub fallback: Option<String>,
}

/// Resolve which embedding reference to use, or `None` when no source names
/// one. Returns [`EmbeddingError::InvalidEmbeddingReference`] when the
/// environment names a reference outside the catalog.
pub fn resolve_embedding_reference(
    options: &ResolveEmbeddingReferenceOptions,
) -> Result<Option<String>, EmbeddingError> {
    if let Some(explicit) = options.explicit.as_ref() {
        return Ok(Some(explicit.clone()));
    }
    if let Some(existing) = options.existing.as_ref() {
        return Ok(Some(existing.clone()));
    }
    let environment_value = match options.environment.as_ref() {
        Some(map) => map.get(EMBEDDING_ENVIRONMENT_VARIABLE).cloned(),
        None => std::env::var(EMBEDDING_ENVIRONMENT_VARIABLE).ok(),
    };
    if let Some(reference) = non_empty(environment_value) {
        if get_embedding_model_catalog_entry(&reference).is_none() {
            return Err(EmbeddingError::InvalidEmbeddingReference(reference));
        }
        return Ok(Some(reference));
    }
    if let Some(default) = options.global_default.as_ref() {
        return Ok(Some(default.clone()));
    }
    Ok(options.fallback.clone())
}

fn non_empty(value: Option<String>) -> Option<String> {
    let trimmed = value?.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

