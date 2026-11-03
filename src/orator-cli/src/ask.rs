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

/// Strict supervised-port parse over `llm.endpoint` (`host:port`).
/// Garbage or missing ports refuse instead of defaulting to an
/// auto-reserve that would boot elsewhere. Explicit `0` still means
/// auto-reserve (the supervisor's documented shape).
pub fn parse_ask_port(endpoint: &str) -> Result<u16, String> {
    let raw = endpoint.rsplit(':').next().unwrap_or("");
    raw.parse::<u16>().map_err(|_| {
        format!("orator: invalid port in llm.endpoint {endpoint:?} (expected host:port)")
    })
}

/// Fail-fast weights check: declared-and-present wins; declared-but
/// missing and undeclared both refuse before any child boots, so a
/// known-missing model never burns the 60 s health timeout. Pinned
/// entries additionally verify the file being used (size stat, then
/// SHA-256 over those bytes): a tampered `.gguf` refuses even though
/// the file exists.
pub fn check_ask_weights(
    endpoint: &orator_core::LlmEndpointConfig,
) -> Result<PathBuf, String> {
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
    if let Some(entry) = orator_models::find_entry(
        orator_models::ModelKind::Llm,
        &endpoint.model,
    ) {
        if entry.pinned() {
            if let Some(want) = entry.size {
                if want > 0 {
                    match std::fs::metadata(&path) {
                        Ok(meta) if meta.len() != want => {
                            return Err(format!(
                                "orator: weights for model {} failed verification (size mismatch: got {} bytes, want {want}; re-pull the model)",
                                endpoint.model,
                                meta.len(),
                            ));
                        }
                        Err(error) => {
                            return Err(format!(
                                "orator: weights for model {} refused: {error}",
                                endpoint.model
                            ));
                        }
                        _ => {}
                    }
                }
            }
            if let Some(want_digest) = entry.sha256.clone() {
                match common_core::hash::hash_file(
                    &path,
                    common_core::hash::HashAlgorithm::Sha256,
                ) {
                    Ok(digest) if digest == want_digest => {}
                    Ok(_) => {
                        return Err(format!(
                            "orator: weights for model {} failed verification (hash mismatch; re-pull the model)",
                            endpoint.model
                        ));
                    }
                    Err(error) => {
                        return Err(format!(
                            "orator: weights for model {} refused: {error}",
                            endpoint.model
                        ));
                    }
                }
            }
        }
    }
    Ok(path)
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
