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

/// Build one cue from whisper centiseconds. Negative inputs clamp to
/// zero; a reversed end clamps up to the start so the cue never runs
/// backwards. Conversions go through `u64` (never a narrowing `u32`).
#[must_use]
pub fn cue_from_centiseconds(start_cs: i64, end_cs: i64, text: String) -> Cue {
    let start_u = u64::try_from(start_cs.max(0)).unwrap_or(u64::MAX);
    let end_u = u64::try_from(end_cs.max(0)).unwrap_or(u64::MAX);
    let start_s = start_u as f64 / 100.0;
    let end_s = (end_u as f64 / 100.0).max(start_s);
    Cue {
        start_s,
        end_s,
        text,
    }
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
