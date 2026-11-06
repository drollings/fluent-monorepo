//! Scrolling-log events for `orator chat`: one enum, wall-clock
//! timestamps with millis, per-op latency fields, colored human
//! rendering plus JSON lines.
//!
//! The renderer is pure (no I/O): text lines go to stderr through
//! `note_progress` so stdout stays clean, JSON lines go to stdout so
//! pipes stay machine-clean.

use std::fmt::Write as _;
use std::time::SystemTime;

/// Log wire shape: human text on stderr, JSON lines on stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Colored human lines.
    Text,
    /// One JSON object per line.
    Json,
}

/// Parse `--log-format=json|text`. Anything else refuses with the two
/// accepted spellings named.
pub fn parse_log_format(raw: &str) -> Result<LogFormat, String> {
    match raw {
        "text" => Ok(LogFormat::Text),
        "json" => Ok(LogFormat::Json),
        _ => Err(format!(
            "orator: unknown log format: {raw:?} (expected text or json)"
        )),
    }
}

/// One scrolling-log row. Every variant carries its display text;
/// operation rows additionally carry latency plus the model and
/// backend that served them so a turn names its pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    /// Session header / mode line. Always shown.
    System {
        /// Wall-clock time of the row.
        at: SystemTime,
        /// Human detail (`Mode: ... | Pipeline: ...`).
        message: String,
    },
    /// Capture/VAD note (trigger, echo shed, cooldown).
    Audio {
        /// Wall-clock time of the row.
        at: SystemTime,
        /// Human detail.
        message: String,
    },
    /// Committed transcription with its decode cost and server.
    Asr {
        /// Wall-clock time of the row.
        at: SystemTime,
        /// Recognized text.
        text: String,
        /// Decode cost in milliseconds.
        latency_ms: u64,
        /// Model tag that served the decode.
        model: String,
        /// Backend that served the decode (`whisper`, ...).
        backend: String,
    },
    /// Assistant reply with its inference cost and server. Replies
    /// always show; tool-call detail (`detail: true`) needs
    /// `--verbosity=1` or higher.
    Agent {
        /// Wall-clock time of the row.
        at: SystemTime,
        /// Reply text (or `Calling tool: ...` detail).
        message: String,
        /// Inference cost in milliseconds.
        latency_ms: u64,
        /// Model tag that served the turn.
        model: String,
        /// Tool-call chatter (filtered at default verbosity).
        detail: bool,
    },
    /// Synthesis note with its cost and server.
    Tts {
        /// Wall-clock time of the row.
        at: SystemTime,
        /// Human detail (`Synthesizing 42 words...`).
        message: String,
        /// Synthesis cost in milliseconds.
        latency_ms: u64,
        /// Model tag that served the synthesis.
        model: String,
        /// Backend that served the synthesis (`kokoro`, ...).
        backend: String,
    },
    /// Failure line: refusals, timeouts, voice errors. Always shown.
    Error {
        /// Wall-clock time of the row.
        at: SystemTime,
        /// Exact failure text (already `orator:`-prefixed upstream).
        message: String,
    },
}

impl ChatEvent {
    /// Verbosity level that reveals this row: agent tool-call detail
    /// needs `--verbosity=1` or higher, everything else shows at the
    /// default zero.
    #[must_use]
    pub const fn verbosity(&self) -> u8 {
        match self {
            Self::Agent { detail: true, .. } => 1,
            Self::System { .. }
            | Self::Audio { .. }
            | Self::Asr { .. }
            | Self::Agent { .. }
            | Self::Tts { .. }
            | Self::Error { .. } => 0,
        }
    }

    /// Whether this row shows at `configured` verbosity.
    #[must_use]
    pub const fn shows_at(&self, configured: u8) -> bool {
        configured >= self.verbosity()
    }

    /// Row timestamp for tests and JSON output.
    #[must_use]
    pub fn at(&self) -> SystemTime {
        match self {
            Self::System { at, .. }
            | Self::Audio { at, .. }
            | Self::Asr { at, .. }
            | Self::Agent { at, .. }
            | Self::Tts { at, .. }
            | Self::Error { at, .. } => *at,
        }
    }

    /// Short kind label shared by both renderers.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::System { .. } => "SYSTEM",
            Self::Audio { .. } => "AUDIO",
            Self::Asr { .. } => "ASR",
            Self::Agent { .. } => "AGENT",
            Self::Tts { .. } => "TTS",
            Self::Error { .. } => "ERROR",
        }
    }

    /// Icon shared by both renderers (matches the scrolling-log contract).
    #[must_use]
    pub const fn icon(&self) -> &'static str {
        match self {
            Self::System { .. } => "⚙",
            Self::Audio { .. } => "🎙",
            Self::Asr { .. } => "📝",
            Self::Agent { .. } => "🤖",
            Self::Tts { .. } => "🔊",
            Self::Error { .. } => "❌",
        }
    }

    /// Latency in milliseconds when the row carries one.
    #[must_use]
    pub const fn latency_ms(&self) -> Option<u64> {
        match self {
            Self::Asr { latency_ms, .. }
            | Self::Agent { latency_ms, .. }
            | Self::Tts { latency_ms, .. } => Some(*latency_ms),
            Self::System { .. } | Self::Audio { .. } | Self::Error { .. } => None,
        }
    }
}

/// Format `at` as `[HH:MM:SS.mmm]` in UTC (24 h wrap). Pure and
/// deterministic: tests pin exact strings from fixed timestamps.
#[must_use]
pub fn format_timestamp(at: SystemTime) -> String {
    let (secs, millis) = match at.duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => (elapsed.as_secs() % 86_400, elapsed.subsec_millis()),
        Err(_) => (0, 0),
    };
    let hours = secs / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    format!("[{hours:02}:{minutes:02}:{seconds:02}.{millis:03}]")
}

/// Render one row as colored (ANSI) or plain human text. Latency
/// renders as `[Nms]`; server identity renders as `(model via
/// backend)` (agent rows name the model only).
pub fn render_text(event: &ChatEvent, color: bool) -> String {
    let stamp = format_timestamp(event.at());
    let head = format!("{} {} {}", stamp, event.icon(), event.kind());
    let head = if color {
        format!("\u{1b}[36m{head}\u{1b}[0m")
    } else {
        head
    };
    let (message, latency, served) = match event {
        ChatEvent::System { message, .. } | ChatEvent::Audio { message, .. } => {
            (message.clone(), None, None)
        }
        ChatEvent::Error { message, .. } => (message.clone(), None, None),
        ChatEvent::Asr {
            text,
            latency_ms,
            model,
            backend,
            ..
        } => (
            format!("\"{text}\""),
            Some(*latency_ms),
            Some(format!("{model} via {backend}")),
        ),
        ChatEvent::Agent {
            message,
            latency_ms,
            model,
            ..
        } => (message.clone(), Some(*latency_ms), Some(model.clone())),
        ChatEvent::Tts {
            message,
            latency_ms,
            model,
            backend,
            ..
        } => (
            message.clone(),
            Some(*latency_ms),
            Some(format!("{model} via {backend}")),
        ),
    };
    let mut line = format!("{head} | {message}");
    if let Some(latency) = latency {
        let _ = write!(line, " [{latency}ms]");
    }
    if let Some(served) = served {
        let _ = write!(line, " ({served})");
    }
    line
}

/// Render one row as a single JSON object line (no color, no icons).
/// Keys stay stable: `ts`, `kind`, `message`, plus `latency_ms`,
/// `model`, `backend` exactly when the row carries them.
pub fn render_json(event: &ChatEvent) -> String {
    let mut value = serde_json::json!({
        "ts": format_timestamp(event.at()),
        "kind": event.kind(),
    });
    match event {
        ChatEvent::System { message, .. }
        | ChatEvent::Audio { message, .. }
        | ChatEvent::Agent { message, .. }
        | ChatEvent::Tts { message, .. }
        | ChatEvent::Error { message, .. } => {
            value["message"] = serde_json::Value::String(message.clone());
        }
        ChatEvent::Asr { text, .. } => {
            value["message"] = serde_json::Value::String(text.clone());
        }
    }
    if let Some(latency) = event.latency_ms() {
        value["latency_ms"] = serde_json::Value::from(latency);
    }
    match event {
        ChatEvent::Asr { model, backend, .. } | ChatEvent::Tts { model, backend, .. } => {
            value["model"] = serde_json::Value::String(model.clone());
            value["backend"] = serde_json::Value::String(backend.clone());
        }
        ChatEvent::Agent { model, .. } => {
            value["model"] = serde_json::Value::String(model.clone());
        }
        ChatEvent::System { .. } | ChatEvent::Audio { .. } | ChatEvent::Error { .. } => {}
    }
    value.to_string()
}

/// Emit one row: JSON lines go to stdout (machine-clean pipes),
/// human text goes to stderr through the single progress home so
/// piped transcripts never see log spam.
pub fn emit(event: &ChatEvent, format: LogFormat, verbosity: u8) {
    if !event.shows_at(verbosity) {
        return;
    }
    match format {
        LogFormat::Json => println!("{}", render_json(event)),
        LogFormat::Text => crate::dictate::note_progress(&render_text(event, true)),
    }
}

#[cfg(test)]
#[path = "../tests/chatlog.rs"]
mod tests;
