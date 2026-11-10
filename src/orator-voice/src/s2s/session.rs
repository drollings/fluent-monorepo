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
    impl_component, Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput,
    WorkUnit,
};
use internment::ArcIntern;
use orator::{AudioChunk, DuplexEngine, DuplexError, DuplexOut, Epoch, Frame};
use thiserror::Error;

use crate::s2s::config::{S2sConfig, AUDIO_IN_RATE, AUDIO_OUT_RATE};
use crate::s2s::protocol::{
    audio_msg, chat_body, encode_wav_pcm16, parse_sse_line, system_msg, text_msg, StreamEvent,
    SYSTEM_ASR, SYSTEM_INTERLEAVED,
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

/// How long a turn waits out a still-loading model before failing.
/// The readiness probe opens routes before weights finish streaming
/// in, so the first turn after a cold boot rides this instead of
/// failing a server that is seconds from serving.
const LOADING_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// Interval between loading polls. Short enough to start promptly
/// once the model answers, long enough to stay out of the way.
const LOADING_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether `body` names a still-loading model (the transient 503 the
/// server answers while weights stream in). Anything else — model
/// errors, auth, unknown routes — is terminal and never retried.
fn is_loading_body(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        && body.to_ascii_lowercase().contains("loading model")
}

/// Sleep `duration` unless `abort` fires first. Returns whether the
/// turn was cancelled during the wait.
///
/// Stays separate from `common_core::retry::run_with_budget` (bounded
/// sync execution with no abort signal): the 100 ms chunking here
/// races the abort signal for prompt cancellation, which the shared
/// helper deliberately does not do.
fn sleep_abort_aware(abort: &StreamAbort, duration: std::time::Duration) -> bool {
    let waited = std::time::Duration::from_millis(0);
    let mut waited = waited;
    let step = std::time::Duration::from_millis(100);
    while waited < duration {
        if abort.is_cancelled() {
            return true;
        }
        let chunk = step.min(duration.saturating_sub(waited));
        std::thread::sleep(chunk);
        waited += chunk;
    }
    abort.is_cancelled()
}

/// POST `body` as JSON until the server answers something other than
/// "still loading", the budget burns, or `abort` fires. Transport
/// failures report through `on_transport_error` (which owns any
/// cache invalidation); a burnt budget replays the last loading
/// reply so the caller names what it saw instead of a bare timeout.
///
/// M5-S3 audit: stays separate from `common_core::retry::PollWithBackoff`.
/// The poller is sync (trait methods are sync; each send bridges), sleeps
/// abort-aware in 100 ms chunks, enforces a wall-clock deadline, and
/// re-issues the POST exactly once after body inspection — none of which
/// the async unraced tick loop expresses. Pinned by
/// `post_until_loaded_returns_terminal_503_without_waiting`.
fn post_until_loaded(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    abort: &StreamAbort,
    on_transport_error: impl Fn(reqwest::Error) -> S2sError,
) -> Result<reqwest::Response, S2sError> {
    if abort.is_cancelled() {
        return Err(S2sError::Aborted);
    }
    let deadline = std::time::Instant::now() + LOADING_WAIT;
    loop {
        // The request builds inside the async block: `send()` arms
        // the client timeout eagerly at construction, which needs the
        // runtime context the bridge only enters on first poll.
        let response = common_core::runtime::block_on(async {
            client
                .post(url)
                .header("Authorization", "Bearer dummy")
                .json(body)
                .timeout(std::time::Duration::from_secs(120))
                .send()
                .await
        })
        .map_err(&on_transport_error)?;
        if abort.is_cancelled() {
            return Err(S2sError::Aborted);
        }
        if !is_loading_status(&response) {
            return Ok(response);
        }
        // Peek without consuming: only a loading body waits; any
        // other 503 shape falls through as a terminal reply below.
        let (status, bytes) = common_core::runtime::block_on(async {
            let status = response.status();
            let bytes = response.bytes().await.unwrap_or_default().to_vec();
            (status, bytes)
        });
        let text = String::from_utf8_lossy(&bytes);
        if !is_loading_body(status, &text) {
            return rebuild_reply(url, client, body, abort, &on_transport_error);
        }
        if std::time::Instant::now() >= deadline {
            return Err(S2sError::Backend(format!(
                "model still loading at {url} after {}s",
                LOADING_WAIT.as_secs()
            )));
        }
        if sleep_abort_aware(abort, LOADING_POLL) {
            return Err(S2sError::Aborted);
        }
    }
}

/// Whether the reply line is even eligible for a loading wait: only
/// 503 continues; every other status returns immediately.
fn is_loading_status(response: &reqwest::Response) -> bool {
    response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE
}

/// Re-issue the POST once after consuming a non-loading 503 body for
/// inspection. Loading retries need the reply to decide; a terminal
/// 503 must still reach the caller as a full response, so the turn
/// replays the request exactly once instead of fabricating one.
fn rebuild_reply(
    url: &str,
    client: &reqwest::Client,
    body: &serde_json::Value,
    abort: &StreamAbort,
    on_transport_error: &impl Fn(reqwest::Error) -> S2sError,
) -> Result<reqwest::Response, S2sError> {
    if abort.is_cancelled() {
        return Err(S2sError::Aborted);
    }
    common_core::runtime::block_on(async {
        client
            .post(url)
            .header("Authorization", "Bearer dummy")
            .json(body)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
    })
    .map_err(on_transport_error)
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

/// One completed turn: spoken audio plus its text.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatTurn {
    /// Reply text deltas joined.
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
    vad: crate::vad::EnergyVad,
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
    /// Session over `config` with a fresh transport. The client is a
    /// default per-session transport (never shared across runtimes);
    /// the 120 s turn bound is set per request, never via a builder.
    #[must_use]
    pub fn new(config: S2sConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            history: Vec::new(),
            first_turn: true,
            heard_audio: false,
            mic_buffer: Vec::new(),
            epoch: Epoch(0),
            abort: StreamAbort::new(),
            first_audio_latency: common_core::metrics::LatencyHistogram::new(),
            no_audio_turns: 0,
            vad: crate::vad::EnergyVad::default(),
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

    /// Transcribe 16 kHz mic samples (single-shot ASR). Input with
    /// no speech presence answers empty without inference: the model
    /// voices politeness priors ("Okay.", "Thank you.") for hiss and
    /// room tone, and no prompt steers that away (measured live), so
    /// the gate is signal-level, never a denylist. The verdict is the
    /// shared commit gate (energy fraction plus stationary-tone
    /// rejection): a loud pure tone is vowel-like to energy alone, so
    /// energy without the tonal verdict would commit it.
    pub fn transcribe_blocking(&mut self, samples: &[f32]) -> Result<String, S2sError> {
        if !crate::vad::speech_present_16k(samples) {
            return Ok(String::new());
        }
        let wav = encode_wav_pcm16(samples, AUDIO_IN_RATE);
        let messages = vec![system_msg(SYSTEM_ASR), audio_msg(&wav)];
        let abort = self.abort.clone();
        let turn = self.stream_turn(&messages, true, &abort)?;
        Ok(turn.text)
    }

    /// Speak `text` to 24 kHz samples (single-shot TTS). The custom
    /// voice server streams `audio_chunk` deltas inside the chat
    /// stream; stock `llama-server` voices through `POST /tts`
    /// instead — so a text-only chat turn falls through to `/tts`
    /// before reporting `NoAudio`. The caller still owns the cascade
    /// (Kokoro/daemon) past this.
    pub fn speak_blocking(&mut self, text: &str) -> Result<Vec<f32>, S2sError> {
        let messages = vec![
            system_msg(&self.config.voice_prompt.clone()),
            text_msg(text),
        ];
        let abort = self.abort.clone();
        let turn = self.stream_turn(&messages, true, &abort)?;
        if turn.voiced {
            return Ok(turn.audio);
        }
        // Stock server: no audio deltas in chat — voice via /tts.
        self.speak_via_tts(text, &abort)
    }

    /// Voice already-decided reply `text` through `POST {endpoint}/tts`
    /// (stock-server shape): the one-shot `say` fallback as a reusable
    /// seam for native turns whose chat stream carried no audio deltas.
    /// `NoAudio` when the route is absent or the payload is empty —
    /// the caller advances the voice chain, never synthesizes silence.
    pub fn speak_text_via_tts(
        &self,
        text: &str,
        abort: &StreamAbort,
    ) -> Result<Vec<f32>, S2sError> {
        self.speak_via_tts(text, abort)
    }

    /// Voice `text` through `POST {endpoint}/tts` (stock server
    /// shape): `{"model", "input": text}` in, WAV bytes out. The
    /// router-mode gateway requires `model` to pick the voice;
    /// single-model appliances ignore the extra field. Resamples to the
    /// 24 kHz speaker rate when the server answers otherwise. A 404
    /// (no `/tts` route: text-only gateway) reports `NoAudio`, never
    /// `Backend` — voiceless stays voiceless and never ejects.
    fn speak_via_tts(&self, text: &str, abort: &StreamAbort) -> Result<Vec<f32>, S2sError> {
        if abort.is_cancelled() {
            return Err(S2sError::Aborted);
        }
        let url = format!("{}/tts", self.config.endpoint.trim_end_matches('/'));
        let body = serde_json::json!({ "model": self.config.model, "input": text });
        // Still-loading 503s wait like chat turns (cold-boot race);
        // anything else returns as-is for the status handling below.
        let response = post_until_loaded(&self.client, &url, &body, abort, |error| {
            S2sError::Backend(error.to_string())
        })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(S2sError::NoAudio);
        }
        let bytes: Vec<u8> = common_core::runtime::block_on(async {
            response
                .error_for_status()
                .map_err(|error| S2sError::Backend(error.to_string()))?
                .bytes()
                .await
                .map_err(|error| S2sError::Backend(format!("tts read: {error}")))
                .map(|chunk| chunk.to_vec())
        })?;
        if abort.is_cancelled() {
            return Err(S2sError::Aborted);
        }
        let wav = orator_audio::decode_wav_bytes(&bytes)
            .map_err(|error| S2sError::Backend(error.to_string()))?;
        if wav.samples.is_empty() {
            return Err(S2sError::NoAudio);
        }
        let samples = if wav.sample_rate == AUDIO_OUT_RATE {
            wav.samples
        } else {
            orator_audio::resample_linear(&wav.samples, wav.sample_rate, AUDIO_OUT_RATE)
        };
        Ok(samples)
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
        crate::s2s::ensure::served_models_hint(&self.config.endpoint).await
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
        let url = crate::s2s::protocol::completions_url(&self.config.endpoint);
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
        // A 503 that names model loading retries with backoff: the
        // readiness probe opens routes before weights finish
        // streaming in, so the first turn after a cold boot would
        // otherwise fail a server that is seconds from serving.
        let mut response =
            post_until_loaded(&self.client, &url, &body, abort, |error: reqwest::Error| {
                crate::s2s::ensure::invalidate_hint_cache(&self.config.endpoint);
                S2sError::Backend(error.to_string())
            })?;
        if !response.status().is_success() {
            crate::s2s::ensure::invalidate_hint_cache(&self.config.endpoint);
            let status = response.status();
            let detail = common_core::runtime::block_on(response.text())
                .map(|body| orator::truncate_tool_output(&body))
                .unwrap_or_default();
            let hint = common_core::runtime::block_on(self.served_models_hint());
            return Err(S2sError::Backend(format!(
                "HTTP {status} from {url} for model '{}'{}{}",
                self.config.model,
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                },
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
        // Audio bound: `max_audio_samples` is a `u32` (schema-capped at
        // 96M), which fits every supported `usize` exactly — no
        // silent `usize::MAX` fallback that would uncap the turn.
        let cap = self.config.max_audio_samples as usize;
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
                    crate::s2s::ensure::invalidate_hint_cache(&self.config.endpoint);
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
        Ok(ChatTurn {
            text,
            audio,
            voiced,
        })
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
        if mic.channel == orator::Channel::Reply {
            return Ok(empty());
        }
        let speech = self.vad.push_frame(mic).speech;
        if speech {
            self.mic_buffer.extend_from_slice(&mic.samples);
            self.in_turn = true;
            self.cold_frames = 0;
            let max = self.config.turn_window_ms as usize * AUDIO_IN_RATE as usize / 1000;
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
        // M15-S3 keep-decision: flat names over the SHARED `S2sConfig`
        // with intentionally session-local validation (plain number
        // parse, no leaf min/max). Delegating to the derived leaf impl
        // would add range enforcement (turn_window_ms 500..10000) and
        // change the contract; a shadow config struct would duplicate
        // the shared section. So the hand impl stays — the schema it
        // mirrors is pinned by
        // `session_field_surface_pins_schema_and_table`.
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
#[path = "../../tests/s2s_session.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/s2s_duplex_calibration.rs"]
mod duplex_calibration_tests;
