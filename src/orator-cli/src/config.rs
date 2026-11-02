//! Configuration helpers: backend resolution and dotted-key access.
//!
//! The command handlers in the binary load [`OratorConfig`] once and
//! pass it down; the pure selection and get/set logic lives here so
//! tests pin it without spawning the binary. CLI flags win over file
//! values; `LLM_BASE_URL`/`LLM_MODEL` win over the `llm` section.

use fluent_wvr::FieldAccess;
use orator_core::{LlmEndpointConfig, OratorConfig};

/// Resolve a backend flag against the configured default: a non-empty
/// flag wins, otherwise the file value applies.
#[must_use]
pub fn resolve_backend(flag: &str, configured: &str) -> String {
    if flag.is_empty() {
        configured.to_string()
    } else {
        flag.to_string()
    }
}

/// Resolve the LLM base URL and model: `LLM_BASE_URL`/`LLM_MODEL` win
/// when set, otherwise the `llm` section applies.
#[must_use]
pub fn resolve_llm_base(llm: &LlmEndpointConfig) -> (String, String) {
    let explicit = std::env::var("LLM_BASE_URL").unwrap_or_default();
    let base = if explicit.is_empty() {
        format!("http://{}", llm.endpoint)
    } else {
        explicit
    };
    let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| llm.model.clone());
    (base, model)
}

/// Resolve the audio-server base URL: `LLM_BASE_URL` wins when set,
/// otherwise the `llm` section applies.
#[must_use]
pub fn audio_base_url(llm: &LlmEndpointConfig) -> String {
    let endpoint = std::env::var("LLM_BASE_URL").unwrap_or_else(|_| llm.endpoint.clone());
    if endpoint.starts_with("http") {
        endpoint
    } else {
        format!("http://{endpoint}")
    }
}

/// Every settable dotted key, in section order.
#[must_use]
pub fn config_keys() -> Vec<String> {
    let mut keys = Vec::new();
    for (section, names) in [
        ("audio", OratorConfig::default().audio.field_names()),
        ("stt", OratorConfig::default().stt.field_names()),
        ("llm", OratorConfig::default().llm.field_names()),
        ("tts", OratorConfig::default().tts.field_names()),
        ("turn", OratorConfig::default().turn.field_names()),
        ("daemon", OratorConfig::default().daemon.field_names()),
    ] {
        for name in names {
            keys.push(format!("{section}.{name}"));
        }
    }
    keys
}

fn allowed_keys() -> String {
    config_keys().join(", ")
}

/// Extra server argv for the GPU opt-in: full layer offload when set,
/// no flag at all otherwise (defaults reproduce today's argv exactly).
#[must_use]
pub fn gpu_llm_args(gpu: bool) -> Vec<String> {
    if gpu {
        vec![
            "--n-gpu-layers".to_string(),
            orator_llm::GPU_OFFLOAD_LAYERS.to_string(),
        ]
    } else {
        Vec::new()
    }
}

/// Read a dotted key (`tts.backend`) from the loaded configuration.
pub fn config_get(config: &OratorConfig, key: &str) -> Result<String, String> {
    let Some((section, field)) = key.split_once('.') else {
        return Err(format!("orator: bad key '{key}'; expected section.field ({})", allowed_keys()));
    };
    match section {
        "audio" => config.audio.get_field(field).map_err(|error| error.to_string()),
        "stt" => config.stt.get_field(field).map_err(|error| error.to_string()),
        "llm" => config.llm.get_field(field).map_err(|error| error.to_string()),
        "tts" => config.tts.get_field(field).map_err(|error| error.to_string()),
        "turn" => config.turn.get_field(field).map_err(|error| error.to_string()),
        "daemon" => config.daemon.get_field(field).map_err(|error| error.to_string()),
        _ => Err(format!(
            "orator: unknown section '{section}'; allowed keys: {}",
            allowed_keys()
        )),
    }
    .map_err(|error| format!("orator: bad key '{key}': {error}; allowed keys: {}", allowed_keys()))
}

/// Write a dotted key through the shared schema: constraints
/// (ranges, lengths) reject out-of-band values before anything is saved.
pub fn config_set(config: &mut OratorConfig, key: &str, value: &str) -> Result<(), String> {
    let Some((section, field)) = key.split_once('.') else {
        return Err(format!("orator: bad key '{key}'; expected section.field ({})", allowed_keys()));
    };
    let outcome = match section {
        "audio" => config.audio.set_field(field, value).map_err(|error| error.to_string()),
        "stt" => config.stt.set_field(field, value).map_err(|error| error.to_string()),
        "llm" => config.llm.set_field(field, value).map_err(|error| error.to_string()),
        "tts" => config.tts.set_field(field, value).map_err(|error| error.to_string()),
        "turn" => config.turn.set_field(field, value).map_err(|error| error.to_string()),
        "daemon" => config.daemon.set_field(field, value).map_err(|error| error.to_string()),
        _ => Err(format!("unknown section '{section}'")),
    };
    outcome.map_err(|error| format!("orator: bad key '{key}': {error}; allowed keys: {}", allowed_keys()))
}

#[cfg(test)]
#[path = "../tests/config.rs"]
mod tests;
