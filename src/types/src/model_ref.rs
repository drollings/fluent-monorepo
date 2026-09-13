//! Canonical model-reference parsing: the single home for
//! `provider:model` references shared by config resolution and (from M11)
//! provenance parsing.
//!
//! The separator is `:` and only the first colon splits (`"a:b:c"` is
//! provider `"a"`, model `"b:c"`). A bare name (`"model"`) carries the
//! default provider. No dash/punctuation stripping exists anywhere in the
//! tree — none is done here either (behavior wins over invention).

/// A parsed `provider:model` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    /// Provider segment (`"default"` when the reference carries none).
    pub provider: String,
    /// Model segment (everything after the first colon, or the whole input).
    pub model: String,
}

impl ModelRef {
    /// Parse a reference; never fails (bare names take the default provider).
    #[must_use]
    pub fn parse(model_ref: &str) -> Self {
        let (provider, model) = model_ref.split_once(':').unwrap_or(("default", model_ref));
        Self {
            provider: provider.to_string(),
            model: model.to_string(),
        }
    }

    /// Strip the provider prefix (`"ollama:llama3"` → `"llama3"`).
    #[must_use]
    pub fn model_name(model_ref: &str) -> &str {
        model_ref.split_once(':').map_or(model_ref, |(_, name)| name)
    }

    /// Split into `(provider, model)`; `None` when no colon is present.
    #[must_use]
    pub fn split(model_ref: &str) -> Option<(&str, &str)> {
        model_ref.split_once(':')
    }
}

/// Strip the provider prefix from a model reference.
/// e.g. "ollama:llama3" -> "llama3", "model" -> "model"
#[must_use]
pub fn model_name(model_ref: &str) -> &str {
    ModelRef::model_name(model_ref)
}

/// Resolve a model reference to its `(provider, model)` halves.
#[must_use]
pub fn parse_model_ref(model_ref: &str) -> Option<(&str, &str)> {
    ModelRef::split(model_ref)
}

#[cfg(test)]
#[path = "../tests/model_ref.rs"]
mod tests;
