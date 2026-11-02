//! Audio endpoint session: single-shot ASR/TTS plus interleaved
//! voice chat, and the turn-windowed duplex engine.
//!
//! Text always flows alongside audio: transcripts land as committed
//! words, replies as spoken text, so tools, history, and the event
//! log work identically whether or not the server voiced audio this
//! turn. When the server streams no `audio_chunk` deltas, speech
//! calls report it instead of synthesizing silence — the cascade
//! (Kokoro) is the configured fallback, never a hidden one.

use std::sync::Arc;

use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::{
    Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit,
    impl_component,
};
use internment::ArcIntern;
use orator_core::{AudioChunk, DuplexEngine, DuplexError, DuplexOut, Epoch, Frame};
use thiserror::Error;

use crate::config::{S2sConfig, AUDIO_IN_RATE, AUDIO_OUT_RATE};
use crate::protocol::{
    audio_msg, chat_body, encode_wav_pcm16, parse_sse_line, system_msg, text_msg,
    StreamEvent, SYSTEM_ASR, SYSTEM_INTERLEAVED,
};

/// Interleaved history cap in entries (50 turns). Over-cap pushes drop
/// the oldest and keep the newest turn protected.
pub const MAX_HISTORY_ENTRIES: usize = 100;

/// Push one history entry under the rolling cap.
fn push_history(history: &mut Vec<serde_json::Value>, entry: serde_json::Value) {
    history.push(entry);
    if history.len() > MAX_HISTORY_ENTRIES {
        let excess = history.len() - MAX_HISTORY_ENTRIES;
        history.drain(0..excess);
    }
}

/// Session failures. Missing audio is reported, never faked.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum S2sError {
    /// Transport or endpoint failure.
    #[error("audio endpoint failed: {0}")]
    Backend(String),
    /// The turn was cancelled.
    #[error("audio turn aborted")]
    Aborted,
    /// The streamed audio passed the configured bound.
    #[error("audio stream exceeded its bound: {0}")]
    Overflow(String),
    /// The server sent no audio for a speech request.
    #[error("server sent no audio")]
    NoAudio,
}

/// Whether the endpoint reports it cannot encode audio input.
///
/// Pinned by transcript goldens: the reference text-only gateway
/// answers 400 with `audio input is not supported` in the body.
/// Every other failure shape advances no audio fallback.
#[must_use]
pub fn audio_input_unsupported(error: &S2sError) -> bool {
    matches!(error, S2sError::Backend(message) if message.contains("audio input is not supported"))
}

/// Whether a voice failure advances the fallback chain.
///
/// Advances on voiceless turns (`NoAudio`) and on transport failures
/// that produced no audio (`Backend`/`Overflow` with empty buffers).
/// Terminal on cancellation (`Aborted`): an interrupted turn never
/// synthesizes speech. Usage and validation failures never reach
/// this gate; callers return them before consulting it.
#[must_use]
pub const fn should_advance_chain(error: &S2sError) -> bool {
    match error {
        S2sError::NoAudio | S2sError::Backend(_) | S2sError::Overflow(_) => true,
        S2sError::Aborted => false,
    }
}

/// Truncate an error body to `max` chars (single line) so endpoint
/// diagnostics stay informative without flooding the terminal.
fn truncate(body: &str, max: usize) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).collect::<String>() + "…"
    }
}

/// One completed turn: spoken audio plus its text.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatTurn {    /// Reply text deltas joined.
    pub text: String,
    /// Spoken samples at 24 kHz (empty when the server sent none).
    pub audio: Vec<f32>,
    /// Whether the server voiced audio this turn.
    pub voiced: bool,
}

/// Audio endpoint session. History holds text only (the model sees
/// transcripts, never raw audio back); the first interleaved turn
/// carries the system prompt with a context reset.
#[allow(clippy::struct_excessive_bools)]
pub struct AudioSession {
    config: S2sConfig,
    // Streaming transport with a 120 s bound: turns carry audio and
    // outlive the shared client's 30 s default, so the session keeps
    // its own client. Probes use the shared client; see `ensure`.
    client: reqwest::Client,
    history: Vec<serde_json::Value>,
    first_turn: bool,
    heard_audio: bool,
    mic_buffer: Vec<f32>,
    epoch: Epoch,
    // Barge-in signal: `DuplexEngine::step` turns against this, so one
    // cancel cuts exactly the in-flight stream; `abort_handle` hands
    // the same signal out.
    abort: StreamAbort,
    // First-audio latency per voiced turn, plus the count of turns
    // that voiced nothing: the task-value signal behind fallback
    // advances, measured here instead of a second metrics stack.
    first_audio_latency: common_core::metrics::LatencyHistogram,
    no_audio_turns: u64,
    // Per-frame duplex state: VAD scores every 20 ms frame immediately;
    // hot frames open the in-flight turn, cold past hangover closes it.
    // `turn_window_ms` is the maximum turn length (force-commit), not
    // the minimum quantum. Thresholds stay exactly as configured.
    vad: orator_vad::EnergyVad,
    in_turn: bool,
    cold_frames: u32,
    // Retired-epoch frames drop with a counter, never an error.
    epoch_drops: u64,
    // In-flight partial transcript, updated as SSE text deltas arrive.
    // Separate `Arc` so a slow chunked turn can be observed pre-close
    // without holding the session borrow.
    live_text: std::sync::Arc<std::sync::Mutex<String>>,
    // Whether the last completed turn voiced audio: the mic stays open
    // while the server voices, so barge-in can cut in.
    voicing: bool,
}

impl AudioSession {
    /// Session over `config` with a fresh transport.
    #[must_use]
    pub fn new(config: S2sConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            history: Vec::new(),
            first_turn: true,
            heard_audio: false,
            mic_buffer: Vec::new(),
            epoch: Epoch(0),
            abort: StreamAbort::new(),
            first_audio_latency: common_core::metrics::LatencyHistogram::new(),
            no_audio_turns: 0,
            vad: orator_vad::EnergyVad::default(),
            in_turn: false,
            cold_frames: 0,
            epoch_drops: 0,
            live_text: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            voicing: false,
        }
    }

    /// Whether any turn so far streamed audio (capability observed,
    /// never assumed).
    #[must_use]
    pub const fn heard_audio(&self) -> bool {
        self.heard_audio
    }

    /// Barge-in handle: firing it aborts the in-flight stream; the
    /// duplex step turns against the same signal.
    #[must_use]
    pub fn abort_handle(&self) -> StreamAbort {
        self.abort.clone()
    }

    /// First-audio latency per voiced turn, for calibration.
    #[must_use]
    pub fn first_audio_latency(&self) -> &common_core::metrics::LatencyHistogram {
        &self.first_audio_latency
    }

    /// Turns that produced no audio: the count behind fallback advances.
    #[must_use]
    pub const fn no_audio_turns(&self) -> u64 {
        self.no_audio_turns
    }

    /// Retired-epoch frames dropped without error.
    #[must_use]
    pub const fn epoch_drops(&self) -> u64 {
        self.epoch_drops
    }

    /// Buffered in-flight mic samples not yet committed to a turn.
    #[must_use]
    pub fn pending_samples(&self) -> usize {
        self.mic_buffer.len()
    }

    /// Shared handle to the in-flight partial transcript, updated as
    /// SSE text deltas arrive. Clone before spawning a slow turn, then
    /// poll while the turn runs to observe text pre-close.
    #[must_use]
    pub fn live_text_handle(&self) -> std::sync::Arc<std::sync::Mutex<String>> {
        std::sync::Arc::clone(&self.live_text)
    }

    /// Whether the last completed turn voiced audio. The mic stays open
    /// while voicing, so user speech can barge in.
    #[must_use]
    pub const fn is_voicing(&self) -> bool {
        self.voicing
    }

    /// Transcribe 16 kHz mic samples (single-shot ASR).
    pub fn transcribe_blocking(&mut self, samples: &[f32]) -> Result<String, S2sError> {
        let wav = encode_wav_pcm16(samples, AUDIO_IN_RATE);
        let messages = vec![system_msg(SYSTEM_ASR), audio_msg(&wav)];
        let abort = self.abort.clone();
        let turn = self.stream_turn(&messages, true, &abort)?;
        Ok(turn.text)
    }

    /// Speak `text` to 24 kHz samples (single-shot TTS). Errors when
    /// the server sends text only — the caller falls back to the
    /// configured cascade voice.
    pub fn speak_blocking(&mut self, text: &str) -> Result<Vec<f32>, S2sError> {
        let messages = vec![
            system_msg(&self.config.voice_prompt.clone()),
            text_msg(text),
        ];
        let abort = self.abort.clone();
        let turn = self.stream_turn(&messages, true, &abort)?;
        if turn.voiced {
            Ok(turn.audio)
        } else {
            Err(S2sError::NoAudio)
        }
    }

    /// One interleaved turn: optional text plus optional 16 kHz audio
    /// in, reply text plus 24 kHz audio out. History accrues text.
    pub fn chat_turn(
        &mut self,
        text: Option<&str>,
        audio_16k: Option<&[f32]>,
        abort: &StreamAbort,
    ) -> Result<ChatTurn, S2sError> {
        if abort.is_cancelled() {
            return Err(S2sError::Aborted);
        }
        let mut outgoing = Vec::new();
        if self.first_turn {
            outgoing.push(system_msg(SYSTEM_INTERLEAVED));
        }
        if let Some(text) = text {
            outgoing.push(text_msg(text));
            push_history(&mut self.history, text_msg(text));
        }
        if let Some(samples) = audio_16k {
            outgoing.push(audio_msg(&encode_wav_pcm16(samples, AUDIO_IN_RATE)));
        }
        let reset = self.first_turn;
        let turn = self.stream_turn(&outgoing, reset, abort)?;
        self.first_turn = false;
        if !turn.text.is_empty() {
            push_history(
                &mut self.history,
                serde_json::json!({
                    "role": "assistant",
                    "content": turn.text,
                }),
            );
        }
        Ok(turn)
    }

    /// Forget interleaved history; the next turn resets context.
    pub fn reset(&mut self) {
        self.history.clear();
        self.first_turn = true;
    }

    /// Best-effort serving hint for error messages: which model ids
    /// the endpoint lists (`/models`, then `/v1/models`). Empty when
    /// the endpoint won't say — the caller still reports the status.
    async fn served_models_hint(&self) -> String {
        crate::ensure::served_models_hint(&self.config.endpoint).await
    }

    /// Adopt the reply-generation epoch barge-in reports. Late turns
    /// under a retired epoch never submit: buffered frames from the old
    /// epoch drop with a counter instead of turning.
    pub fn set_epoch(&mut self, epoch: Epoch) {
        if epoch != self.epoch {
            if !self.mic_buffer.is_empty() || self.in_turn {
                self.epoch_drops += 1;
            }
            self.mic_buffer.clear();
            self.in_turn = false;
            self.cold_frames = 0;
            common_core::sync::lock(&self.live_text).clear();
        }
        self.epoch = epoch;
    }

    fn stream_turn(
        &mut self,
        messages: &[serde_json::Value],
        reset_context: bool,
        abort: &StreamAbort,
    ) -> Result<ChatTurn, S2sError> {
        if abort.is_cancelled() {
            return Err(S2sError::Aborted);
        }
        // New turn, new partial: clear before the first chunk so
        // pre-close polling sees exactly this turn's text.
        common_core::sync::lock(&self.live_text).clear();
        let started = std::time::Instant::now();
        let url = crate::protocol::completions_url(&self.config.endpoint);
        let body = chat_body(
            &self.config.model.clone(),
            messages,
            self.config.max_tokens,
            reset_context,
        );
        // The request builds inside the async block, not in the
        // `block_on` argument: `RequestBuilder::send()` arms the
        // client timeout eagerly at construction, which needs the
        // runtime context the bridge only enters on first poll.
        let mut response = common_core::runtime::block_on(async {
            self.client
                .post(&url)
                .header("Authorization", "Bearer dummy")
                .json(&body)
                .send()
                .await
        })
        .map_err(|error| {
            crate::ensure::invalidate_hint_cache(&self.config.endpoint);
            S2sError::Backend(error.to_string())
        })?;
        if !response.status().is_success() {
            crate::ensure::invalidate_hint_cache(&self.config.endpoint);
            let status = response.status();
            let detail = common_core::runtime::block_on(response.text())
                .map(|body| truncate(&body, 300))
                .unwrap_or_default();
            let hint = common_core::runtime::block_on(self.served_models_hint());
            return Err(S2sError::Backend(format!(
                "HTTP {status} from {url} for model '{}'{}{}",
                self.config.model,
                if detail.is_empty() { String::new() } else { format!(": {detail}") },
                hint,
            )));
        }
        // Stream the body chunk by chunk: complete SSE lines parse as
        // they arrive, so first-audio latency is chunk time, not turn
        // time. The line buffer holds one partial line; audio stops at
        // the configured cap; DONE ends the turn without waiting for
        // close. Each read races the abort: firing it ends the turn,
        // and dropping the response cancels the request.
        let mut buffer = Vec::new();
        let mut text = String::new();
        let mut audio = Vec::new();
        let cap = usize::try_from(self.config.max_audio_samples).unwrap_or(usize::MAX);
        loop {
            if abort.is_cancelled() {
                return Err(S2sError::Aborted);
            }
            let chunk = common_core::runtime::block_on(async {
                tokio::select! {
                    chunk = response.chunk() => chunk.map_err(|error| {
                        S2sError::Backend(format!("stream read: {error}"))
                    }),
                    () = abort.cancelled() => Err(S2sError::Aborted),
                }
            });
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(S2sError::Backend(message)) => {
                    crate::ensure::invalidate_hint_cache(&self.config.endpoint);
                    return Err(S2sError::Backend(message));
                }
                Err(other) => return Err(other),
            };
            let Some(bytes) = chunk else {
                break;
            };
            let mut turn_done = false;
            for line in fluent_llm::sse::drain_sse_lines(&mut buffer, &bytes) {
                for event in parse_sse_line(&line).into_iter().flatten() {
                    match event {
                        StreamEvent::Text(delta) => {
                            text.push_str(&delta);
                            common_core::sync::lock(&self.live_text).push_str(&delta);
                        }
                        StreamEvent::Audio(samples) => {
                            self.heard_audio = true;
                            if audio.is_empty() {
                                self.first_audio_latency.observe_duration(started);
                            }
                            if audio.len() + samples.len() > cap {
                                return Err(S2sError::Overflow(format!(
                                    "{} samples exceeds the {}-sample bound",
                                    audio.len() + samples.len(),
                                    self.config.max_audio_samples
                                )));
                            }
                            audio.extend_from_slice(&samples);
                        }
                        StreamEvent::Done => {
                            turn_done = true;
                        }
                    }
                }
            }
            if turn_done {
                break;
            }
        }
        let voiced = !audio.is_empty();
        if !voiced {
            self.no_audio_turns += 1;
        }
        Ok(ChatTurn { text, audio, voiced })
    }
}

impl DuplexEngine for AudioSession {
    fn step(&mut self, mic: &Frame) -> Result<DuplexOut, DuplexError> {
        // Per-frame duplex over a request/response endpoint: every 20 ms
        // frame is VAD-scored immediately; hot frames open the in-flight
        // turn, cold past hangover closes it, and the window caps the
        // maximum length (force-commit). Thresholds stay exactly as
        // configured.
        fn empty() -> DuplexOut {
            DuplexOut {
                audio: AudioChunk {
                    samples: Arc::new(Vec::new()),
                    sample_rate: AUDIO_OUT_RATE,
                    marks: Vec::new(),
                },
                end_of_turn: false,
            }
        }
        fn voiced(audio: Vec<f32>) -> DuplexOut {
            DuplexOut {
                audio: AudioChunk {
                    samples: Arc::new(audio),
                    sample_rate: AUDIO_OUT_RATE,
                    marks: Vec::new(),
                },
                end_of_turn: true,
            }
        }
        // A fired barge-in retires the in-flight turn before any new
        // audio accumulates: report it, re-arm, and drop the buffer.
        if self.abort.is_cancelled() {
            self.mic_buffer.clear();
            self.in_turn = false;
            self.cold_frames = 0;
            self.abort = StreamAbort::new();
            return Err(DuplexError::Aborted);
        }
        // Echo reference: server audio never opens a user turn and never
        // enters the transcript. Attribution stays by channel.
        if mic.channel == orator_core::Channel::Reply {
            return Ok(empty());
        }
        let speech = self.vad.push_frame(mic).speech;
        if speech {
            self.mic_buffer.extend_from_slice(&mic.samples);
            self.in_turn = true;
            self.cold_frames = 0;
            let max =
                self.config.turn_window_ms as usize * AUDIO_IN_RATE as usize / 1000;
            if self.mic_buffer.len() < max.max(1) {
                return Ok(empty());
            }
        } else if self.in_turn {
            self.cold_frames += 1;
            if self.cold_frames < self.vad.hangover_frames.max(1) {
                return Ok(empty());
            }
        } else {
            return Ok(empty());
        }
        let buffered = std::mem::take(&mut self.mic_buffer);
        self.in_turn = false;
        self.cold_frames = 0;
        // Turn against the session's abort (not a fresh one), so
        // barge-in cancels the in-flight stream. One cancel retires
        // exactly one turn: an aborted turn re-arms the signal.
        let abort = self.abort.clone();
        match self.chat_turn(None, Some(&buffered), &abort) {
            Ok(turn) => {
                // The mic stays open while the server voices: `voicing`
                // marks the reply so a hot mic frame can barge the next
                // turn without waiting for silence.
                self.voicing = !turn.audio.is_empty();
                Ok(voiced(turn.audio))
            }
            Err(S2sError::Aborted) => {
                self.abort = StreamAbort::new();
                Err(DuplexError::Aborted)
            }
            Err(S2sError::Overflow(detail)) => Err(DuplexError::Backend(detail)),
            Err(S2sError::NoAudio) => Err(DuplexError::Unavailable("no audio".to_string())),
            Err(S2sError::Backend(message)) => Err(DuplexError::Backend(message)),
        }
    }
}

impl WorkUnit for AudioSession {
    fn name(&self) -> &str {
        "s2s.session"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if self.config.model.is_empty() {
            Ok(WorkOutput::fail("s2s.session: no model"))
        } else {
            Ok(WorkOutput::ok(format!(
                "model={} epoch={}",
                self.config.model, self.epoch.0
            )))
        }
    }
}

impl FieldAccess for AudioSession {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        match name {
            "mode" => {
                self.config.mode = value.to_string();
                Ok(())
            }
            "voice_prompt" => {
                self.config.voice_prompt = value.to_string();
                Ok(())
            }
            "turn_window_ms" => {
                self.config.turn_window_ms = value.parse().map_err(|_| {
                    FieldError::Parse("turn_window_ms must be a number".to_string())
                })?;
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "mode" => Ok(self.config.mode.clone()),
            "voice_prompt" => Ok(self.config.voice_prompt.clone()),
            "turn_window_ms" => Ok(self.config.turn_window_ms.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        &["mode", "voice_prompt", "turn_window_ms"]
    }
}

impl Describable for AudioSession {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "description": "Turn mode" },
                "voice_prompt": { "type": "string", "description": "TTS voice prompt" },
                "turn_window_ms": { "type": "integer", "description": "Duplex turn window" }
            },
            "required": ["mode", "voice_prompt", "turn_window_ms"]
        })
    }
}

impl_component!(AudioSession);

#[cfg(test)]
#[path = "../tests/session.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/calibration.rs"]
mod calibration_tests;

#[cfg(test)]
#[path = "../tests/duplex_calibration.rs"]
mod duplex_calibration_tests;
