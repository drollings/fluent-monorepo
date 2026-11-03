#![forbid(unsafe_code)]

//! Voice fallback decisions shared by the `say` path.
//!
//! The helpers here are pure: no sockets, no spawns, no audio. The
//! serving path in `main` keeps the I/O; this module keeps the
//! decisions so they stay testable and identical on every arm.

use orator_s2s::S2sError;

/// Daemon voice-endpoint outcome for one lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonVoiceHint {
    /// The daemon serves an endpoint URL.
    Available(String),
    /// The daemon is down or declined; the caller may boot its own.
    Unavailable,
    /// The daemon ejected the endpoint after consecutive failures.
    /// Callers must not boot while ejected; fall through to the chain.
    Ejected(String),
    /// The daemon is over its concurrency bound. Callers must not
    /// boot; fall through to the chain with the bound named.
    Saturated(String),
    /// A sibling boot owns the server; retry shortly with progress,
    /// never a read failure and never a second boot storm.
    BootContention(String),
    /// A peer was too slow for the handler budget. Transport-level
    /// only: the caller may retry and may boot its own server.
    SlowPeer(String),
}

/// Extract a structured `kind` marker from a daemon refusal line when
/// the envelope carried one. Matches the JSON `"kind":"X"` shape the
/// daemon emits; returns `None` for old daemons that only sent wording.
fn structured_kind(body: &str) -> Option<&str> {
    for kind in [
        "Ejected",
        "Saturated",
        "BootContention",
        "SlowPeer",
        "Disabled",
        "ShuttingDown",
        "NoBinary",
        "Boot",
        "Backend",
        "NoAudio",
        "Overflow",
        "Aborted",
        "UnknownSession",
    ] {
        let marker = format!("\"kind\":\"{kind}\"");
        if body.contains(&marker) {
            return match kind {
                "Ejected" => Some("Ejected"),
                "Saturated" => Some("Saturated"),
                "BootContention" => Some("BootContention"),
                "SlowPeer" => Some("SlowPeer"),
                _ => Some("Other"),
            };
        }
    }
    None
}

/// Classify a daemon `VoiceEndpoint` failure body.
///
/// The daemon reports ejection, saturation, and boot contention as
/// `ok: false` with the reason in the body; absence or refusal
/// without those markers is `Unavailable` (the caller may boot). The
/// structured `kind` marker wins when present; the wording match stays
/// only for old daemons that omitted it.
#[must_use]
pub fn classify_daemon_voice_error(body: &str) -> DaemonVoiceHint {
    if let Some(kind) = structured_kind(body) {
        match kind {
            "Ejected" => return DaemonVoiceHint::Ejected(body.to_string()),
            "Saturated" => return DaemonVoiceHint::Saturated(body.to_string()),
            "BootContention" => return DaemonVoiceHint::BootContention(body.to_string()),
            "SlowPeer" => return DaemonVoiceHint::SlowPeer(body.to_string()),
            _ => {}
        }
    }
    let lower = body.to_lowercase();
    if lower.contains("ejected") {
        DaemonVoiceHint::Ejected(body.to_string())
    } else if lower.contains("saturated") || lower.contains("admission") {
        DaemonVoiceHint::Saturated(body.to_string())
    } else if lower.contains("concurrent boot")
        || lower.contains("boot in progress")
        || lower.contains("bootcontention")
    {
        DaemonVoiceHint::BootContention(body.to_string())
    } else {
        DaemonVoiceHint::Unavailable
    }
}

/// Classify a transport-level daemon-call failure for the voice path.
///
/// `text` is the quiet-core failure line when the daemon answered
/// `ok: false`, or `None` when the call never reached the daemon.
/// `None` and unmarked refusals are `Unavailable`; ejected,
/// saturated, and contention bodies become the chain-skipping arms.
#[must_use]
pub fn classify_daemon_call_failure(text: Option<&str>) -> DaemonVoiceHint {
    match text {
        Some(body) => match classify_daemon_voice_error(body) {
            DaemonVoiceHint::Ejected(reason) => DaemonVoiceHint::Ejected(reason),
            DaemonVoiceHint::Saturated(reason) => DaemonVoiceHint::Saturated(reason),
            DaemonVoiceHint::BootContention(reason) => DaemonVoiceHint::BootContention(reason),
            DaemonVoiceHint::SlowPeer(reason) => DaemonVoiceHint::SlowPeer(reason),
            DaemonVoiceHint::Available(url) => DaemonVoiceHint::Available(url),
            DaemonVoiceHint::Unavailable => DaemonVoiceHint::Unavailable,
        },
        None => DaemonVoiceHint::Unavailable,
    }
}

/// Whether `error` advances the voice fallback chain.
///
/// Single home for the decision: the serving arms call this instead
/// of matching variants inline.
#[must_use]
pub fn should_advance(error: &S2sError) -> bool {
    orator_s2s::should_advance_chain(error)
}

/// Split `text` for speech after normalizing once.
///
/// Fenced code, markup, and bare URLs never reach the voice: the
/// reply on screen keeps them, the spoken form drops them. The split
/// runs on the normalized form so chunk boundaries match what is
/// actually synthesized.
#[must_use]
pub fn normalized_chunks(text: &str) -> Vec<String> {
    let normalized = orator_tts::normalize_for_speech(text);
    orator_tts::split_all(&normalized)
        .into_iter()
        .map(|chunk| chunk.text)
        .collect()
}

/// Remaining text after `played_chars` chars were already heard.///
/// The daemon last resort speaks only the unsynthesized suffix so an
/// already-played prefix is never spoken twice. Splits on a char
/// boundary; short or over-long counts clamp to the ends.
#[must_use]
pub fn unsynthesized_suffix(full: &str, played_chars: usize) -> &str {
    if played_chars == 0 {
        return full;
    }
    let total: usize = full.chars().count();
    if played_chars >= total {
        return "";
    }
    let byte: usize = full
        .char_indices()
        .nth(played_chars)
        .map_or(full.len(), |(index, _)| index);
    &full[byte..]
}

/// Count characters already voiced across fully played chunks.
#[must_use]
pub fn played_chars_for_chunks(chunks: &[String], played_chunks: usize) -> usize {
    chunks
        .iter()
        .take(played_chunks)
        .map(|chunk| chunk.chars().count())
        .sum()
}

/// Decode a daemon `Say` audio body: base64 little-endian f32 PCM plus
/// the sample rate. Counts without audio are refused — a turn with no
/// samples is a failed turn, never a silent success.
pub fn decode_say_audio(body: &serde_json::Value) -> Result<(Vec<f32>, u32), String> {
    let encoded = body
        .get("audio")
        .and_then(|audio| audio.as_str())
        .ok_or_else(|| "daemon Say reply carries no audio".to_string())?;
    let rate = body
        .get("rate")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "daemon Say reply carries no rate".to_string())?;
    let rate = u32::try_from(rate).map_err(|_| "daemon Say rate overflows u32".to_string())?;
    if rate == 0 {
        return Err("daemon Say rate is zero".to_string());
    }
    use base64::prelude::*;
    let raw = BASE64_STANDARD
        .decode(encoded.trim())
        .map_err(|error| format!("daemon Say audio is not base64: {error}"))?;
    if raw.len() % 4 != 0 {
        return Err("daemon Say audio length is not a whole f32 count".to_string());
    }
    if raw.is_empty() {
        return Err("daemon Say returned no samples".to_string());
    }
    let samples: Vec<f32> = raw
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    Ok((samples, rate))
}

#[cfg(test)]
#[path = "../tests/say_chain.rs"]
mod tests;
