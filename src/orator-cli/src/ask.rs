//! Ask preflight: strict port parsing, fail-fast weights checks, and
//! a speak-failure note that cannot be mistaken for an answer failure.

use std::path::PathBuf;

/// Ask boot failure with its exit-code class attached. Bad ports are
/// usage errors (the caller mistyped the endpoint); missing weights
/// and boot failures are errors (the machine cannot serve).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskFail {
    /// Caller error: exit 2.
    Usage(String),
    /// Serving error: exit 1.
    Error(String),
}

/// Effective ask model: the `roles.chat` head wins; an empty list
/// falls back to the `llm.model` leaf (never the top-level
/// `default` — that tag names the audio-bundle model, not the chat
/// model). Defaults seed head and leaf equal, so default configs
/// resolve byte-identically through either layer. Pure (no weights,
/// no I/O) so resolution pins without hardware.
#[must_use]
pub fn resolve_ask_model(config: &orator_core::OratorConfig) -> String {
    orator_core::resolve_role_model(&config.roles, "chat")
        .unwrap_or_else(|| config.llm.model.clone())
}

/// Effective LLM section for ask and boot: the leaf with the resolved
/// head as its model, composed `ctx`/`n_batch` applied only when the
/// tables specify them, and a table weights declaration applied when
/// the head's entry declares one. Every composed value must satisfy
/// the same bounds `config set` enforces on the leaves, so tables
/// cannot smuggle a boot the leaves would refuse. Pure: the caller
/// (supervised boot, direct session) does the I/O.
pub fn effective_llm_config(
    config: &orator_core::OratorConfig,
) -> Result<orator_core::LlmEndpointConfig, String> {
    let mut llm = config.llm.clone();
    llm.model = resolve_ask_model(config);
    let composed =
        orator_core::compose_role_params(&config.models, &config.roles, "chat", &llm.model);
    if let Some(value) = composed.get("ctx") {
        llm.ctx = composed_u16("ctx", value, 1024)?;
    }
    if let Some(value) = composed.get("n_batch") {
        llm.n_batch = composed_u16("n_batch", value, 64)?;
    }
    if let Some(entry) = config.models.models.get(&llm.model) {
        if let Some(declared) = entry
            .weights
            .as_deref()
            .filter(|weights| !weights.is_empty())
        {
            let resolved =
                orator_core::resolve_model_weights(&config.models, &config.gguf_dir, &llm.model)
                    .unwrap_or_else(|| std::path::PathBuf::from(declared));
            llm.weights = resolved.to_string_lossy().into_owned();
        }
    }
    Ok(llm)
}

/// One composed tuning value: an integer in `min..=u16::MAX` (the
/// leaf field minima: 1024 for `ctx`, 64 for `n_batch`). Anything
/// else refuses loudly naming the key.
fn composed_u16(key: &str, value: &serde_json::Value, min: u16) -> Result<u16, String> {
    let number = value
        .as_u64()
        .filter(|_| value.is_number())
        .ok_or_else(|| {
            format!("orator: bad chat {key} {value} (expected an integer {min}..=65535)")
        })?;
    u16::try_from(number)
        .ok()
        .filter(|parsed| *parsed >= min)
        .ok_or_else(|| {
            format!("orator: bad chat {key} {value} (expected an integer {min}..=65535)")
        })
}

/// Strict supervised-port parse over `llm.endpoint` (`host:port`).
/// Garbage or missing ports refuse instead of defaulting to an
/// auto-reserve that would boot elsewhere. Explicit `0` still means
/// auto-reserve (the supervisor's documented shape). Shares the one
/// endpoint grammar with `config set`, so set-time and use-time agree.
pub fn parse_ask_port(endpoint: &str) -> Result<u16, String> {
    orator_core::parse_endpoint(endpoint)
        .map(|parts| parts.port)
        .map_err(|_| {
            format!("orator: invalid port in llm.endpoint {endpoint:?} (expected host:port)")
        })
}

/// Fail-fast weights check: declared-and-present wins; declared-but
/// missing and undeclared both refuse before any child boots, so a
/// known-missing model never burns the 60 s health timeout. Pinned
/// entries additionally verify the file being used (size stat, then
/// SHA-256 over those bytes): a tampered `.gguf` refuses even though
/// the file exists.
pub fn check_ask_weights(endpoint: &orator_core::LlmEndpointConfig) -> Result<PathBuf, String> {
    let path = match orator_core::resolve_llm_weights(endpoint) {
        Some(path) if path.exists() => path,
        Some(path) => return Err(format!(
            "orator: no weights for model {} (missing {}; declare `llm.weights` or place {}.gguf in the llama.cpp layout)",
            endpoint.model,
            path.display(),
            endpoint.model,
        )),
        None => {
            let config_hint = orator_core::config_path().map_or_else(
                || "the config file".to_string(),
                |path| path.display().to_string(),
            );
            return Err(format!(
                "orator: no weights for model {} (declare `llm.weights` in {config_hint} or place {}.gguf in the llama.cpp layout)",
                endpoint.model, endpoint.model,
            ));
        }
    };
    // Pinned hash gate over the file being used (not the catalog's
    // resolved copy): a present file with the wrong bytes refuses
    // loudly before any boot burns the health timeout. Size stats
    // first (instant refuse on truncation/tamper); the hash runs only
    // when the size agrees.
    if let Some(entry) = orator_models::find_entry(orator_models::ModelKind::Llm, &endpoint.model) {
        if entry.pinned() {
            verify_pinned_ask_weights(&entry, &path, &endpoint.model)?;
        }
    }
    Ok(path)
}

/// Verify the file an ask turn will use against a pinned catalog entry.
///
/// Size is checked before hashing; each verifier outcome maps back to the
/// preflight refusal text, with I/O failures named as a refusal rather than a
/// silent pass. Only pinned entries reach here.
pub(crate) fn verify_pinned_ask_weights(
    entry: &orator_models::ModelEntry,
    path: &std::path::Path,
    model: &str,
) -> Result<(), String> {
    use common_core::hash::FileVerification;

    let expected_size = std::num::NonZeroU64::new(entry.size.unwrap_or(0));
    match common_core::hash::verify_sha256_file(
        path,
        expected_size,
        entry.sha256.as_deref().unwrap_or_default(),
    ) {
        Ok(FileVerification::Match) => Ok(()),
        Ok(FileVerification::SizeMismatch { actual, expected }) => Err(format!(
            "orator: weights for model {model} failed verification (size mismatch: got {actual} bytes, want {expected}; re-pull the model)"
        )),
        Ok(FileVerification::HashMismatch { .. }) => Err(format!(
            "orator: weights for model {model} failed verification (hash mismatch; re-pull the model)"
        )),
        Err(error) => Err(format!(
            "orator: weights for model {model} refused: {error}"
        )),
    }
}

/// Speak-failure note: the answer already printed, so the voice
/// failure names itself and never reads as an answer failure.
#[must_use]
pub fn speak_failure_note(code: crate::ExitCode) -> String {
    format!("orator: answer printed, voice delivery failed (speak: {code:?})")
}

/// Short spoken form of a (possibly long) reply: first two sentences
/// plus an announcement when text was cut. Short replies pass
/// through unchanged so small talk stays natural. Single home for
/// every `--speak` path (ask, chat, TUI): full text always prints,
/// voice stays real-time.
#[must_use]
pub fn concise_for_speech(reply: &str) -> String {
    const LIMIT: usize = 500;
    let trimmed = reply.trim();
    if trimmed.len() <= LIMIT {
        return trimmed.to_string();
    }
    let mut sentences = Vec::new();
    let mut current = String::new();
    for ch in trimmed.chars() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') && sentences.len() < 2 {
            sentences.push(current.trim().to_string());
            current = String::new();
        }
    }
    let mut spoken = sentences.join(" ");
    if spoken.is_empty() {
        spoken = trimmed.chars().take(LIMIT).collect::<String>();
    }
    format!("{spoken} ... Full answer printed.")
}

#[cfg(test)]
#[path = "../tests/ask.rs"]
mod tests;
