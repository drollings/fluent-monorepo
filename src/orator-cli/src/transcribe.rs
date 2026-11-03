#![forbid(unsafe_code)]

//! Offline transcription rendering: one cue list, four formats.

/// One timed transcript cue.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    /// Start in seconds.
    pub start_s: f64,
    /// End in seconds.
    pub end_s: f64,
    /// Cue text.
    pub text: String,
}

/// Output formats for `orator transcribe --format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    /// Plain text, one cue per line.
    Txt,
    /// SubRip subtitles.
    Srt,
    /// WebVTT subtitles.
    Vtt,
    /// JSON cue array.
    Json,
}

impl TranscriptFormat {
    /// Parse a format name. Unknown names are rejected, never defaulted.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "txt" => Some(Self::Txt),
            "srt" => Some(Self::Srt),
            "vtt" => Some(Self::Vtt),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

fn stamp_srt(seconds: f64) -> Result<String, String> {
    if !seconds.is_finite() {
        return Err(format!("non-finite timestamp: {seconds}"));
    }
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    Ok(format!(
        "{:02}:{:02}:{:02},{:03}",
        total_ms / 3_600_000,
        total_ms / 60_000 % 60,
        total_ms / 1000 % 60,
        total_ms % 1000
    ))
}

fn stamp_vtt(seconds: f64) -> Result<String, String> {
    Ok(stamp_srt(seconds)?.replace(',', "."))
}

/// Absolute media cap for cue stamps (24 h). Corrupt model output
/// (`i64::MAX`, overflowed centiseconds) clamps here instead of
/// becoming a giant timestamp; the cue stays renderable and finite.
pub const MAX_CUE_SECONDS: f64 = 24.0 * 3600.0;

/// Build one cue from whisper centiseconds. Negative inputs clamp to
/// zero; a reversed end clamps up to the start so the cue never runs
/// backwards; anything past the media cap clamps to it (never a giant
/// stamp). Conversions stay in `f64` from clamped `i64` (never a
/// narrowing `u32`, never `u64::MAX`).
#[must_use]
pub fn cue_from_centiseconds(start_cs: i64, end_cs: i64, text: String) -> Cue {
    let start_s = (start_cs.max(0) as f64 / 100.0).min(MAX_CUE_SECONDS);
    let end_s = (end_cs.max(0) as f64 / 100.0)
        .min(MAX_CUE_SECONDS)
        .max(start_s);
    Cue {
        start_s,
        end_s,
        text,
    }
}

/// Encode 16 kHz mono `samples` for a `Transcribe` call: base64 i16
/// LE plus the rate tag. Matches the daemon's audio decoder; the
/// daemon enforces the single-frame bound before any inference.
#[must_use]
pub fn encode_transcribe_audio(samples: &[f32]) -> serde_json::Value {
    use base64::prelude::*;
    let mut raw = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        raw.extend_from_slice(&((clamped * 32767.0) as i16).to_le_bytes());
    }
    serde_json::json!({ "audio": BASE64_STANDARD.encode(&raw), "rate": 16_000 })
}

/// Decode a `Transcribe` reply body to text. A missing text field is
/// a loud error, never silent nothing.
pub fn decode_transcribe_text(body: &serde_json::Value) -> Result<String, String> {
    body.get("text")
        .and_then(|text| text.as_str())
        .map(str::to_string)
        .ok_or_else(|| "daemon Transcribe reply carries no text".to_string())
}

/// Single cue for an audio-server transcript over `sample_count` 16 kHz
/// samples. The audio-server path keeps an intentional no-fallback
/// asymmetry with `say`: a voiceless `say` turn advances down the
/// fallback chain (silence is recoverable), but a failed transcription
/// reports honestly instead of inventing words (transcript fidelity is
/// never recoverable). Endpoint errors surface as-is; render failures
/// are usage errors.
#[must_use]
pub fn audio_server_cue(text: String, sample_count: usize) -> Cue {
    Cue {
        start_s: 0.0,
        end_s: sample_count as f64 / 16_000.0,
        text,
    }
}

/// Render `cues` in `format`. Non-finite stamps are usage errors, never
/// saturated clocks. Empty lists keep the per-format contract (txt/srt
/// empty, vtt header-only, json `[]`).
pub fn render(cues: &[Cue], format: TranscriptFormat) -> Result<String, String> {
    for cue in cues {
        if !cue.start_s.is_finite() || !cue.end_s.is_finite() {
            return Err(format!(
                "non-finite cue timestamp: {} --> {}",
                cue.start_s, cue.end_s
            ));
        }
    }
    match format {
        TranscriptFormat::Txt => Ok(cues
            .iter()
            .map(|cue| cue.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")),
        TranscriptFormat::Srt => {
            let mut parts = Vec::with_capacity(cues.len());
            for (index, cue) in cues.iter().enumerate() {
                parts.push(format!(
                    "{}\n{} --> {}\n{}\n",
                    index + 1,
                    stamp_srt(cue.start_s)?,
                    stamp_srt(cue.end_s)?,
                    cue.text
                ));
            }
            Ok(parts.join("\n"))
        }
        TranscriptFormat::Vtt => {
            use std::fmt::Write as _;
            let mut out = String::from("WEBVTT\n\n");
            for cue in cues {
                let _ = write!(
                    out,
                    "{} --> {}\n{}\n\n",
                    stamp_vtt(cue.start_s)?,
                    stamp_vtt(cue.end_s)?,
                    cue.text
                );
            }
            Ok(out)
        }
        TranscriptFormat::Json => {
            let items: Vec<String> = cues
                .iter()
                .map(|cue| {
                    format!(
                        "{{\"start\":{:.3},\"end\":{:.3},\"text\":{}}}",
                        cue.start_s,
                        cue.end_s,
                        serde_json::to_string(&cue.text).unwrap_or_default()
                    )
                })
                .collect();
            Ok(format!("[{}]", items.join(",")))
        }
    }
}

#[cfg(test)]
#[path = "../tests/transcribe.rs"]
mod tests;
