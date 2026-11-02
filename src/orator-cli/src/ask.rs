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
/// known-missing model never burns the 60 s health timeout.
pub fn check_ask_weights(
    endpoint: &orator_core::LlmEndpointConfig,
) -> Result<PathBuf, String> {
    match orator_core::resolve_llm_weights(endpoint) {
        Some(path) if path.exists() => Ok(path),
        Some(path) => Err(format!(
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
            Err(format!(
                "orator: no weights for model {} (declare `llm.weights` in {config_hint} or place {}.gguf in the llama.cpp layout)",
                endpoint.model, endpoint.model,
            ))
        }
    }
}

/// Speak-failure note: the answer already printed, so the voice
/// failure names itself and never reads as an answer failure.
#[must_use]
pub fn speak_failure_note(code: crate::ExitCode) -> String {
    format!("orator: answer printed, voice delivery failed (speak: {code:?})")
}

#[cfg(test)]
#[path = "../tests/ask.rs"]
mod tests;
