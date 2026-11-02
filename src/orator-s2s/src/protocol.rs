//! Audio-endpoint wire codec: WAV framing, message builders, SSE
//! audio deltas.
//!
//! Requests carry 16 kHz mono WAV (base64) as `input_audio` content
//! parts; the server streams SSE `data:` lines whose deltas carry
//! `content` text and/or `audio_chunk` base64 little-endian f32 PCM at
//! 24 kHz. `reset_context` scopes server-side state per turn mode.

/// ASR single-shot system prompt.
pub const SYSTEM_ASR: &str = "Perform ASR.";
/// Interleaved chat system prompt.
pub const SYSTEM_INTERLEAVED: &str = "Respond with interleaved text and audio.";

/// One parsed stream event.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Spoken-audio samples at 24 kHz.
    Audio(Vec<f32>),
    /// Reply text delta.
    Text(String),
    /// The server finished this turn.
    Done,
}

/// Encode mono f32 samples as 16-bit PCM WAV bytes.
#[must_use]
pub fn encode_wav_pcm16(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        out.extend_from_slice(&((clamped * 32767.0) as i16).to_le_bytes());
    }
    out
}

/// Base64 (standard) encode bytes for a message part.
#[must_use]
pub fn b64(bytes: &[u8]) -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(bytes)
}

/// Base64 decode, naming the failure.
pub fn unb64(text: &str) -> Result<Vec<u8>, String> {
    use base64::prelude::*;
    BASE64_STANDARD.decode(text.trim()).map_err(|error| format!("bad base64: {error}"))
}

/// Little-endian f32 PCM bytes to samples (the `audio_chunk` shape).
/// A trailing partial sample is dropped, never fabricated.
#[must_use]
pub fn pcm_bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// System message part.
#[must_use]
pub fn system_msg(prompt: &str) -> serde_json::Value {
    serde_json::json!({ "role": "system", "content": prompt })
}

/// Text user message part.
#[must_use]
pub fn text_msg(text: &str) -> serde_json::Value {
    serde_json::json!({ "role": "user", "content": text })
}

/// Audio user message part: base64 WAV as an `input_audio` block.
#[must_use]
pub fn audio_msg(wav_bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "role": "user",
        "content": [{
            "type": "input_audio",
            "input_audio": { "data": b64(wav_bytes), "format": "wav" },
        }],
    })
}

/// Completions endpoint URL: `{base}/v1/chat/completions` (a base
/// already ending there passes through). The audio contract is the
/// versioned route — the reference audio server serves only it,
/// while OpenAI-compatible gateways accept it.
#[must_use]
pub fn completions_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/v1/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/chat/completions")
    }
}

/// Streaming chat body with the context-reset scope.
#[must_use]
pub fn chat_body(
    model: &str,
    messages: &[serde_json::Value],
    max_tokens: u16,
    reset_context: bool,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "max_tokens": max_tokens,
        "extra_body": { "reset_context": reset_context },
    })
}

/// Parse one SSE `data:` line into stream events. `None` for
/// keep-alives and unparseable lines (skipped, never fatal);
/// `[DONE]` yields `Done`.
#[must_use]
pub fn parse_sse_line(line: &str) -> Option<Vec<StreamEvent>> {
    let line = line.trim();
    let data = line.strip_prefix("data:")?.trim();
    if data == "[DONE]" {
        return Some(vec![StreamEvent::Done]);
    }
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let choice = value.get("choices")?.as_array()?.first()?;
    if choice.get("finish_reason").and_then(|reason| reason.as_str()) == Some("stop") {
        return Some(vec![StreamEvent::Done]);
    }
    let delta = choice.get("delta")?;
    let mut events = Vec::new();
    if let Some(text) = delta.get("content").and_then(|content| content.as_str()) {
        if !text.is_empty() {
            events.push(StreamEvent::Text(text.to_string()));
        }
    }
    if let Some(chunk) = delta.get("audio_chunk") {
        let encoded = match chunk {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Object(_) => chunk
                .get("data")
                .and_then(|data| data.as_str())
                .map(str::to_string),
            _ => None,
        };
        if let Some(encoded) = encoded {
            if let Ok(bytes) = unb64(&encoded) {
                events.push(StreamEvent::Audio(pcm_bytes_to_f32(&bytes)));
            }
        }
    }
    Some(events)
}

#[cfg(test)]
#[path = "../tests/protocol.rs"]
mod tests;
