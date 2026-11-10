//! Configuration helpers: backend resolution and dotted-key access.
//!
//! The command handlers in the binary load [`OratorConfig`] once and
//! pass it down; the pure selection and get/set logic lives here so
//! tests pin it without spawning the binary. CLI flags win over file
//! values; `LLM_BASE_URL`/`LLM_MODEL` win over the `llm` section.

use fluent_wvr::FieldAccess;
use orator::{LlmEndpointConfig, OratorConfig};

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
        base_url_of(&llm.endpoint)
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
    if std::env::var("LLM_BASE_URL").is_ok() {
        return endpoint;
    }
    base_url_of(&endpoint)
}

/// Resolve the audio-server base URL: `LLM_BASE_URL` wins when set,
/// otherwise the `s2s` section applies (default port 8080).
#[must_use]
pub fn s2s_base_url(s2s: &orator::S2sConfig) -> String {
    if let Ok(explicit) = std::env::var("LLM_BASE_URL") {
        return explicit;
    }
    base_url_of(&s2s.endpoint)
}

/// One rendering for every file-backed endpoint value: the shared
/// grammar when it parses, the legacy prefix guess when an old file
/// still holds an unparseable value (the dial then fails loudly with
/// its remedy — honest, never silent).
fn base_url_of(endpoint: &str) -> String {
    orator::parse_endpoint(endpoint).map_or_else(
        |_| {
            if endpoint.starts_with("http") {
                endpoint.to_string()
            } else {
                format!("http://{endpoint}")
            }
        },
        |parts| parts.base_url(),
    )
}

/// Every settable dotted key, in section order. The top-level
/// `gguf_dir`, the default-model tag `default`, and the whole-table
/// `models`/`roles` keys (JSON values) come first.
/// NOTE: this order feeds the `allowed keys` refusal strings and is
/// pinned by `config_keys_pins_the_advertised_surface` — do NOT
/// rebuild it from `OratorConfig::field_names` (the root handle lists
/// `models`/`roles` as skipped, so its order differs).
#[must_use]
pub fn config_keys() -> Vec<String> {
    let mut keys = vec![
        "gguf_dir".to_string(),
        "default".to_string(),
        "models".to_string(),
        "roles".to_string(),
    ];
    for (section, names) in [
        ("audio", OratorConfig::default().audio.field_names()),
        ("stt", OratorConfig::default().stt.field_names()),
        ("llm", OratorConfig::default().llm.field_names()),
        ("tts", OratorConfig::default().tts.field_names()),
        ("s2s", OratorConfig::default().s2s.field_names()),
        ("turn", OratorConfig::default().turn.field_names()),
        ("vad", OratorConfig::default().vad.field_names()),
        ("chat", OratorConfig::default().chat.field_names()),
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
            orator_agent::llm::GPU_OFFLOAD_LAYERS.to_string(),
        ]
    } else {
        Vec::new()
    }
}

/// Read a dotted key (`tts.backend`) from the loaded configuration.
/// The top-level `gguf_dir` and `default` model tag read without a
/// section; `models` and `roles` read as whole-table JSON.
pub fn config_get(config: &OratorConfig, key: &str) -> Result<String, String> {
    if key == "gguf_dir" {
        return Ok(config.gguf_dir.clone());
    }
    if key == "default" {
        return Ok(config.default.clone());
    }
    if key == "models" {
        return serde_json::to_string(&config.models)
            .map_err(|error| format!("orator: bad key '{key}': {error}"));
    }
    if key == "roles" {
        return serde_json::to_string(&config.roles)
            .map_err(|error| format!("orator: bad key '{key}': {error}"));
    }
    let Some((section, _field)) = key.split_once('.') else {
        return Err(format!(
            "orator: bad key '{key}'; expected section.field ({})",
            allowed_keys()
        ));
    };
    match section {
        // M7-S6: audio/turn rode along for consistency — every section
        // arm now delegates to the root nested handle, so the match is
        // only a known-section gate (unknown sections keep their
        // verbatim refusal below).
        "audio" => config.get_field(key).map_err(|error| error.to_string()),
        // M7-S5: stt/tts read through the root nested handle (same
        // leaf errors, same wrap — the round-trip suite pins them).
        "stt" => config.get_field(key).map_err(|error| error.to_string()),
        // M7-S6: llm reads through the root nested handle (endpoint
        // grammar pre-check lives in config_set, reads need none).
        "llm" => config.get_field(key).map_err(|error| error.to_string()),
        // M7-S5: stt/tts read through the root nested handle.
        "tts" => config.get_field(key).map_err(|error| error.to_string()),
        // M7-S4: s2s reads through the root nested handle (same leaf
        // error, same wrap — the round-trip suite pins it).
        "s2s" => config.get_field(key).map_err(|error| error.to_string()),
        // M7-S6: turn + chat + vad read through the root nested handle
        // (same leaf error, same wrap — the round-trip suites pin them).
        "turn" => config.get_field(key).map_err(|error| error.to_string()),
        "vad" => config.get_field(key).map_err(|error| error.to_string()),
        "chat" => config.get_field(key).map_err(|error| error.to_string()),
        // M7-S3: daemon reads through the root nested handle (same
        // leaf error, same wrap — the round-trip suite pins it).
        "daemon" => config.get_field(key).map_err(|error| error.to_string()),
        _ => Err(format!(
            "orator: unknown section '{section}'; allowed keys: {}",
            allowed_keys()
        )),
    }
    .map_err(|error| {
        format!(
            "orator: bad key '{key}': {error}; allowed keys: {}",
            allowed_keys()
        )
    })
}

/// Write a dotted key through the shared schema: constraints
/// (ranges, lengths) reject out-of-band values before anything is saved.
/// Endpoint leaves (`llm.endpoint`, `s2s.endpoint`) additionally parse
/// through the one endpoint grammar the dialers use, so garbage refuses
/// at the cause instead of failing later far from it. The top-level
/// `gguf_dir` and the default-model tag write verbatim; `models` and
/// `roles` take whole-table JSON (parsed before anything is saved, so
/// a bad document changes nothing).
pub fn config_set(config: &mut OratorConfig, key: &str, value: &str) -> Result<(), String> {
    if key == "gguf_dir" {
        config.gguf_dir = value.to_string();
        return Ok(());
    }
    if key == "default" {
        config.default = value.to_string();
        return Ok(());
    }
    if key == "models" {
        config.models = serde_json::from_str(value).map_err(|error| {
            format!("orator: bad key '{key}': expected a models table as JSON: {error}")
        })?;
        return Ok(());
    }
    if key == "roles" {
        config.roles = serde_json::from_str(value).map_err(|error| {
            format!("orator: bad key '{key}': expected a roles table as JSON: {error}")
        })?;
        return Ok(());
    }
    let Some((section, _field)) = key.split_once('.') else {
        return Err(format!(
            "orator: bad key '{key}'; expected section.field ({})",
            allowed_keys()
        ));
    };
    if matches!(key, "llm.endpoint" | "s2s.endpoint") {
        if let Err(detail) = orator::parse_endpoint(value) {
            return Err(format!(
                "orator: bad key '{key}': {detail}; allowed keys: {}",
                allowed_keys()
            ));
        }
    }
    let outcome = match section {
        // M7-S6: audio/turn rode along for consistency (see get).
        "audio" => config
            .set_field(key, value)
            .map_err(|error| error.to_string()),
        // M7-S5: stt/tts write through the root nested handle.
        "stt" => config
            .set_field(key, value)
            .map_err(|error| error.to_string()),
        // M7-S6: llm writes through the root nested handle (the
        // endpoint grammar pre-check above still runs first).
        "llm" => config
            .set_field(key, value)
            .map_err(|error| error.to_string()),
        // M7-S5: stt/tts write through the root nested handle.
        "tts" => config
            .set_field(key, value)
            .map_err(|error| error.to_string()),
        // M7-S4: s2s writes through the root nested handle (same leaf
        // error, same wrap — the round-trip suite pins it; the endpoint
        // grammar pre-check above still runs first).
        "s2s" => config
            .set_field(key, value)
            .map_err(|error| error.to_string()),
        // M7-S6: turn + chat + vad write through the root nested handle.
        "turn" => config
            .set_field(key, value)
            .map_err(|error| error.to_string()),
        "vad" => config
            .set_field(key, value)
            .map_err(|error| error.to_string()),
        "chat" => config
            .set_field(key, value)
            .map_err(|error| error.to_string()),
        // M7-S3: daemon writes through the root nested handle (same
        // leaf error, same wrap — the round-trip suite pins it).
        "daemon" => config
            .set_field(key, value)
            .map_err(|error| error.to_string()),
        _ => Err(format!("unknown section '{section}'")),
    };
    outcome.map_err(|error| {
        format!(
            "orator: bad key '{key}': {error}; allowed keys: {}",
            allowed_keys()
        )
    })
}

#[cfg(test)]
#[path = "../tests/config.rs"]
mod tests;
