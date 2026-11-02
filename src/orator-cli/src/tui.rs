//! Terminal duplex client: mic level, server-audio indicator,
//! scrolling transcript, barge key, session commands.
//!
//! The headless core (`TuiApp` plus `render` over any ratatui backend)
//! is fully scriptable: keystrokes in, transcript lines out, no TTY.
//! The live loop (`run_live`: raw mode, capture, playback) runs only
//! from the `tui` command on a real terminal and is never exercised
//! by tests.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fluent_concurrency::stream::StreamAbort;
use orator_vad::EnergyVad;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::socket::{
    Transport, call_transport, refused_text, unavailable_socket_text, unavailable_ws_text,
};
use crate::ExitCode;

/// Bounded scrollback: the transcript never grows with session length.
pub const SCROLLBACK: usize = 200;

/// One mic frame at the capture gate (16 kHz mono).
const FRAME_SAMPLES: usize = 320;

/// One transcript row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiLine {
    /// User turn text.
    Said(String),
    /// Assistant reply text.
    Heard(String),
    /// Server voiced `usize` samples.
    Voiced(usize),
    /// Turn completed.
    Done,
    /// State change worth keeping on screen.
    Status(String),
    /// Control failure, worded exactly like the plain CLI.
    Failure(String),
}

/// Voice-turn progress feeding the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    /// Transcript text arrived.
    Text(String),
    /// Server voiced `usize` samples.
    Audio(usize),
    /// Turn completed.
    Done,
}

/// Keystroke meaning. Ctrl-modified keys drive session control so
/// typing never collides with commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiAction {
    /// Cut the in-flight turn.
    Barge,
    /// Release the endpoint and exit.
    Quit,
    /// Open a session.
    Start,
    /// Close the session.
    Stop,
    /// Show sessions.
    List,
    /// Type into the input line.
    Type(char),
    /// Delete one input char.
    Backspace,
    /// Send the input line.
    Send,
    /// No binding.
    Ignore,
}

/// Map one key event to its action. Pure: headless tests drive it
/// without a terminal.
#[must_use]
pub fn map_key(key: KeyEvent) -> TuiAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('b') => TuiAction::Barge,
            KeyCode::Char('c' | 'q') => TuiAction::Quit,
            KeyCode::Char('s') => TuiAction::Start,
            KeyCode::Char('e') => TuiAction::Stop,
            KeyCode::Char('l') => TuiAction::List,
            _ => TuiAction::Ignore,
        };
    }
    match key.code {
        KeyCode::Esc => TuiAction::Quit,
        KeyCode::Enter => TuiAction::Send,
        KeyCode::Backspace => TuiAction::Backspace,
        KeyCode::Char(char) => TuiAction::Type(char),
        _ => TuiAction::Ignore,
    }
}

/// Slash commands mirroring the chat loop vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommand {
    /// Open a session.
    Start,
    /// Close the session.
    Stop,
    /// Show sessions.
    List,
    /// Release the endpoint and exit.
    Quit,
}

/// Parse a slash command. Anything else (including unknown slashes)
/// is spoken text, never a dispatch — the chat loop rule.
#[must_use]
pub fn parse_command(input: &str) -> Option<SlashCommand> {
    match input {
        "/start" => Some(SlashCommand::Start),
        "/stop" => Some(SlashCommand::Stop),
        "/list" => Some(SlashCommand::List),
        "/quit" | "/exit" => Some(SlashCommand::Quit),
        _ => None,
    }
}

/// Control failure: exit code plus the exact line shown. Built from
/// the shared constructors the plain CLI prints from — one wording,
/// two renderers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlError {
    /// Process exit for the failure.
    pub code: ExitCode,
    /// Exact line shown.
    pub text: String,
}

/// Session-control outcome.
pub type ControlOutcome = Result<serde_json::Value, ControlError>;

/// Session control seam: daemon calls, abort, endpoint release. The
/// live loop wires the transport; tests wire the stub.
pub trait SessionControl {
    /// One session-control call through the shared envelopes.
    fn call(&mut self, method: &str, params: serde_json::Value) -> ControlOutcome;
    /// Cut the in-flight turn.
    fn abort(&mut self);
    /// Release the endpoint (idempotent: exactly once).
    fn release(&mut self);
    /// Adopt a fresh abort after a barge cut the last one.
    /// No-op for controls without abort state.
    fn rearm(&mut self, _abort: StreamAbort) {}
}

/// Live control over the daemon transport: quiet cores in, transcript
/// text out. Abort fires the shared signal; quit releases through the
/// session stop on the loop path, so no releaser handle is held here.
pub struct LiveControl {
    transport: Transport,
    abort: StreamAbort,
}

impl LiveControl {
    /// Control over `transport`, cutting `abort` on barge.
    #[must_use]
    pub fn new(transport: Transport, abort: StreamAbort) -> Self {
        Self { transport, abort }
    }
}

impl SessionControl for LiveControl {
    fn call(&mut self, method: &str, params: serde_json::Value) -> ControlOutcome {
        call_transport(self.transport, method, params).map_err(|failure| ControlError {
            code: failure.code,
            text: failure.text.unwrap_or_else(|| match failure.code {
                ExitCode::DaemonUnavailable => match self.transport {
                    Transport::Socket => {
                        unavailable_socket_text(orator_core::socket_path().as_deref())
                    }
                    Transport::Ws => {
                        let (config, _) = orator_core::config::load_hierarchical();
                        unavailable_ws_text(&config.daemon.ws_listen)
                    }
                },
                _ => refused_text(method, &serde_json::Value::Null),
            }),
        })
    }

    fn abort(&mut self) {
        self.abort.cancel();
    }

    fn release(&mut self) {
        // Nothing held: the loop stops the session explicitly on quit,
        // so release stays a tested no-op on the live path.
    }

    fn rearm(&mut self, abort: StreamAbort) {
        self.abort = abort;
    }
}

/// Mic gate: scores local capture frames with the default detector
/// (the calibrated constants live there, never restated here) and
/// passes only hot spans upward. Level always tracks the frame.
#[derive(Debug)]
pub struct MicGate {
    vad: EnergyVad,
    level: f32,
}

impl MicGate {
    /// Gate with the default detector constants.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vad: EnergyVad::default(),
            level: 0.0,
        }
    }

    /// Current meter level in `[0.0, 1.0]`.
    #[must_use]
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Whether the detector currently holds speech open.
    #[must_use]
    pub fn speaking(&self) -> bool {
        self.vad.speaking()
    }

    /// Score one frame: level always updates; `true` means the frame
    /// belongs to a hot span and goes upward.
    pub fn push(&mut self, samples: &[f32]) -> bool {
        self.level = rms(samples).clamp(0.0, 1.0);
        self.vad.push(samples);
        self.vad.speaking()
    }
}

impl Default for MicGate {
    fn default() -> Self {
        Self::new()
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f32 = samples.iter().map(|sample| sample * sample).sum();
    (energy / samples.len() as f32).sqrt()
}

/// Meter bar of `width` cells, clamped — never overflows, never
/// negative.
#[must_use]
pub fn level_bar(level: f32, width: usize) -> String {
    let filled = (level.clamp(0.0, 1.0) * width as f32).round() as usize;
    let filled = filled.min(width);
    "█".repeat(filled) + &"░".repeat(width - filled)
}

/// Live TUI state: transcript, meter, voicing, session, input line.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiState {
    /// Scrollback-capped transcript rows.
    pub lines: Vec<TuiLine>,
    /// Mic meter level in `[0.0, 1.0]`.
    pub mic_level: f32,
    /// Server-audio indicator: reply audio is playing.
    pub voicing: bool,
    /// Active session id, if any.
    pub session: Option<String>,
    /// Pending input line.
    pub input: String,
}

impl TuiState {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            mic_level: 0.0,
            voicing: false,
            session: None,
            input: String::new(),
        }
    }

    fn push_line(&mut self, line: TuiLine) {
        self.lines.push(line);
        if self.lines.len() > SCROLLBACK {
            let overflow = self.lines.len() - SCROLLBACK;
            self.lines.drain(..overflow);
        }
    }
}

/// Headless duplex client: keystrokes and mic frames in, transcript
/// lines out. The live loop owns one and renders its state.
pub struct TuiApp<C: SessionControl> {
    /// Rendered state.
    pub state: TuiState,
    /// Session control (daemon transport live, stub in tests).
    pub control: C,
    /// Buffered hot-span samples awaiting endpointing. Bounded and
    /// lossy while a slow transcribe holds the queue (see
    /// [`MAX_PENDING_UTTERANCE`]).
    pub pending_utterance: Vec<f32>,
    /// Over-cap utterance samples dropped so far.
    pub pending_dropped: u64,
    /// Quit latched: the loop exits after releasing.
    pub quit: bool,
    gate: MicGate,
}

/// Hot-span buffer cap in samples (120 s at 16 kHz). The queue holds
/// at most one in-flight transcribe plus this buffer; over-cap input
/// drops the oldest first with a counter, never OOMs.
pub const MAX_PENDING_UTTERANCE: usize = 1_920_000;

impl<C: SessionControl> TuiApp<C> {
    /// App over `control`.
    #[must_use]
    pub fn new(control: C) -> Self {
        Self {
            state: TuiState::new(),
            control,
            pending_utterance: Vec::new(),
            pending_dropped: 0,
            quit: false,
            gate: MicGate::new(),
        }
    }

    /// Whether the mic gate currently holds speech open.
    #[must_use]
    pub fn gate_speaking(&self) -> bool {
        self.gate.speaking()
    }

    /// One keystroke: map, then dispatch against the control.
    pub fn key(&mut self, key: KeyEvent) {
        match map_key(key) {
            TuiAction::Barge => {
                self.control.abort();
                self.state
                    .push_line(TuiLine::Status("barge: cut the in-flight turn".to_string()));
            }
            TuiAction::Quit => self.quit(),
            TuiAction::Start => self.start_session(),
            TuiAction::Stop => self.stop_session(),
            TuiAction::List => self.list_sessions(),
            TuiAction::Type(char) => self.state.input.push(char),
            TuiAction::Backspace => {
                self.state.input.pop();
            }
            TuiAction::Send => self.send_input(),
            TuiAction::Ignore => {}
        }
    }

    /// One captured mic frame: meter always follows; hot spans buffer.
    /// Over-cap spans drop the oldest first (realtime lossy) and count
    /// them, so a slow transcribe never grows the queue without bound.
    pub fn mic(&mut self, samples: &[f32]) {
        if self.gate.push(samples) {
            self.pending_utterance.extend_from_slice(samples);
            let excess = self
                .pending_utterance
                .len()
                .saturating_sub(MAX_PENDING_UTTERANCE);
            if excess > 0 {
                self.pending_utterance.drain(..excess);
                self.pending_dropped += excess as u64;
            }
        }
        self.state.mic_level = self.gate.level();
    }

    /// One voice-turn event into the transcript.
    pub fn event(&mut self, event: TurnEvent, from_user: bool) {
        let line = match (event, from_user) {
            (TurnEvent::Text(text), true) => TuiLine::Said(text),
            (TurnEvent::Text(text), false) => TuiLine::Heard(text),
            (TurnEvent::Audio(samples), _) => TuiLine::Voiced(samples),
            (TurnEvent::Done, _) => TuiLine::Done,
        };
        self.state.push_line(line);
    }

    /// Take the buffered utterance, leaving an empty buffer.
    #[must_use]
    pub fn take_utterance(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.pending_utterance)
    }

    fn quit(&mut self) {
        if !self.quit {
            self.control.release();
            self.quit = true;
        }
    }

    fn start_session(&mut self) {
        match self
            .control
            .call("StartSession", serde_json::json!({"kind": "assistant"}))
        {
            Ok(body) => {
                let id = body
                    .get("session")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                self.state.session = Some(id.clone());
                self.state
                    .push_line(TuiLine::Status(format!("session {id} started")));
            }
            Err(error) => self.state.push_line(TuiLine::Failure(error.text)),
        }
    }

    fn stop_session(&mut self) {
        match self.control.call("StopSession", serde_json::json!({})) {
            Ok(_) => {
                self.state.session = None;
                self.state
                    .push_line(TuiLine::Status("session stopped".to_string()));
            }
            Err(error) => self.state.push_line(TuiLine::Failure(error.text)),
        }
    }

    fn list_sessions(&mut self) {
        match self.control.call("ListSessions", serde_json::json!({})) {
            Ok(body) => {
                let sessions = body
                    .get("sessions")
                    .and_then(|value| value.as_array())
                    .map_or_else(Vec::new, |sessions| {
                        sessions
                            .iter()
                            .filter_map(|value| value.as_str())
                            .collect::<Vec<_>>()
                    });
                let text = if sessions.is_empty() {
                    "sessions: none".to_string()
                } else {
                    format!("sessions: {}", sessions.join(", "))
                };
                self.state.push_line(TuiLine::Status(text));
            }
            Err(error) => self.state.push_line(TuiLine::Failure(error.text)),
        }
    }

    fn send_input(&mut self) {
        let input = std::mem::take(&mut self.state.input);
        if input.is_empty() {
            return;
        }
        if let Some(command) = parse_command(&input) {
            match command {
                SlashCommand::Start => self.start_session(),
                SlashCommand::Stop => self.stop_session(),
                SlashCommand::List => self.list_sessions(),
                SlashCommand::Quit => self.quit(),
            }
            return;
        }
        let mut params = serde_json::json!({"question": input});
        if let Some(session) = self.state.session.as_deref() {
            params["session"] = serde_json::Value::String(session.to_string());
        }
        match self.control.call("Ask", params) {
            Ok(body) => {
                let reply = body
                    .get("reply")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                self.state.push_line(TuiLine::Said(input));
                self.state.push_line(TuiLine::Heard(reply));
            }
            Err(error) => {
                self.state.push_line(TuiLine::Said(input));
                self.state.push_line(TuiLine::Failure(error.text));
            }
        }
    }
}

fn line_text(line: &TuiLine) -> String {
    match line {
        TuiLine::Said(text) => format!("you: {text}"),
        TuiLine::Heard(text) => format!("orator: {text}"),
        TuiLine::Voiced(samples) => format!("♪ {samples}"),
        TuiLine::Done => "— done —".to_string(),
        TuiLine::Status(text) => format!("· {text}"),
        TuiLine::Failure(text) => format!("error: {text}"),
    }
}

/// Draw the state on any backend. Tests draw on `TestBackend` and
/// assert on the buffer — no TTY involved.
pub fn render<B: Backend>(terminal: &mut Terminal<B>, state: &TuiState) -> io::Result<()>
where
    B::Error: std::fmt::Debug,
{
    terminal
        .draw(|frame| {
        let area = frame.area();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(area);
        let session = state.session.as_deref().unwrap_or("none");
        let voice = if state.voicing { "VOICING" } else { "idle" };
        let header = Paragraph::new(format!("orator tui | session: {session} | {voice}"))
            .block(Block::default().borders(Borders::ALL).title("status"));
        frame.render_widget(header, rows[0]);
        let meter = Paragraph::new(format!("mic [{}]", level_bar(state.mic_level, 20)))
            .block(Block::default().borders(Borders::ALL).title("input"));
        frame.render_widget(meter, rows[1]);
        let items: Vec<ListItem> = state
            .lines
            .iter()
            .map(|line| ListItem::new(line_text(line)))
            .collect();
        let transcript =
            List::new(items).block(Block::default().borders(Borders::ALL).title("transcript"));
        frame.render_widget(transcript, rows[2]);
        let prompt = Paragraph::new(format!("> {}", state.input)).block(
            Block::default()
                .borders(Borders::ALL)
                .title("say (Ctrl+B barge · Ctrl+Q quit)"),
        );
        frame.render_widget(prompt, rows[3]);
        })
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    Ok(())
}

/// Run the live duplex client: capture, gate, transcribe, ask, voice.
/// Raw mode and the alternate screen require a terminal; anything
/// else refuses with a named usage error before touching the display.
pub fn run_live(
    transport: Transport,
    config: &orator_core::OratorConfig,
) -> crate::ExitCode {
    use ratatui::crossterm::tty::IsTty as _;

    if !std::io::stdin().is_tty() {
        eprintln!("orator: tui needs a terminal (stdin is not a TTY)");
        return crate::ExitCode::Usage;
    }
    let backend = match orator_audio::CpalBackend::open_default() {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("orator: {error}");
            return crate::ExitCode::Error;
        }
    };
    if ratatui::crossterm::terminal::enable_raw_mode().is_err() {
        eprintln!("orator: tui needs a terminal (raw mode refused)");
        return crate::ExitCode::Usage;
    }
    let mut stdout = std::io::stdout();
    if ratatui::crossterm::execute!(
        stdout,
        ratatui::crossterm::terminal::EnterAlternateScreen
    )
    .is_err()
    {
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
        eprintln!("orator: tui needs a terminal (alternate screen refused)");
        return crate::ExitCode::Usage;
    }
    let outcome = run_loop(transport, config, backend);
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
    let _ = ratatui::crossterm::execute!(
        stdout,
        ratatui::crossterm::terminal::LeaveAlternateScreen
    );
    outcome
}

struct TranscribeJob {
    receiver: std::sync::mpsc::Receiver<Result<String, String>>,
}

/// Cached whisper backend keyed by (model, language). The struct owns
/// the lazily loaded ONNX context, so reuse across utterances never
/// pays a second load; a key change rebuilds once and drops the old
/// context. Construction is lazy (no I/O), loads happen on decode.
#[derive(Default)]
struct WhisperCache {
    key: Option<(String, String)>,
    backend: Option<orator_stt::WhisperBackend>,
    builds: u64,
}

impl WhisperCache {
    /// Backend for (`model`, `language`), rebuilding only on key
    /// change. `model_path` must already name the weights file.
    fn get_or_build(
        &mut self,
        model: &str,
        language: &str,
        model_path: std::path::PathBuf,
    ) -> &orator_stt::WhisperBackend {
        let hit = self
            .key
            .as_ref()
            .is_some_and(|(cached_model, cached_language)| {
                cached_model == model && cached_language == language
            });
        if !hit {
            self.builds += 1;
            self.key = Some((model.to_string(), language.to_string()));
            self.backend = Some(orator_stt::WhisperBackend::new(
                model_path,
                language.to_string(),
            ));
        }
        self.backend.as_ref().expect("built above")
    }
}

/// Cached Kokoro backend keyed by (model path, voices dir, voice,
/// gpu). Same single-build discipline as [`WhisperCache`]: the TUI
/// reply path reuses the session instead of reloading ONNX per reply.
#[derive(Default)]
struct KokoroCache {
    key: Option<(std::path::PathBuf, std::path::PathBuf, String, bool)>,
    backend: Option<orator_tts::KokoroBackend>,
    builds: u64,
}

impl KokoroCache {
    /// Backend for the reply config, rebuilding only on key change.
    fn get_or_build(
        &mut self,
        model: std::path::PathBuf,
        voices: std::path::PathBuf,
        voice: &str,
        gpu: bool,
    ) -> &orator_tts::KokoroBackend {
        let hit = self.key.as_ref().is_some_and(|key| {
            key.0 == model && key.1 == voices && key.2 == voice && key.3 == gpu
        });
        if !hit {
            self.builds += 1;
            self.key = Some((model.clone(), voices.clone(), voice.to_string(), gpu));
            let backend =
                orator_tts::KokoroBackend::new(model, voices, voice.to_string());
            self.backend = Some(if gpu { backend.with_gpu() } else { backend });
        }
        self.backend.as_ref().expect("built above")
    }
}

/// One TUI transcription request: samples plus everything the worker
/// thread needs, answered once over `reply`.
struct SttRequest {
    samples: Vec<f32>,
    backend_name: String,
    language: String,
    model: String,
    endpoint: String,
    abort: StreamAbort,
    reply: std::sync::mpsc::Sender<Result<String, String>>,
}

/// Long-lived TUI speech worker. One thread owns the whisper backend
/// for the whole `tui` run (the whisper context is not `Send`, so it
/// can never cross threads — reuse happens by thread affinity, not
/// sharing). Audio-server turns resolve the endpoint per request;
/// barged turns abort promptly (whisper) or drop on delivery.
struct SttWorker {
    tx: std::sync::mpsc::Sender<SttRequest>,
}

impl SttWorker {
    /// Spawn the worker; it parks until the first utterance.
    fn spawn() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<SttRequest>();
        std::thread::spawn(move || {
            let mut whisper = WhisperCache::default();
            for request in rx {
                if request.abort.is_cancelled() {
                    let _ = request
                        .reply
                        .send(Err("orator: transcribe aborted".to_string()));
                    continue;
                }
                let outcome = transcribe_samples(
                    &request.samples,
                    &request.backend_name,
                    &request.language,
                    &request.model,
                    &request.endpoint,
                    &request.abort,
                    &mut whisper,
                );
                // Barge landed mid-decode: the transcript is stale by
                // definition, so it is never delivered (the loop already
                // dropped the job on the cut).
                if request.abort.is_cancelled() {
                    continue;
                }
                let _ = request.reply.send(outcome);
            }
        });
        Self { tx }
    }

    /// Transcribe `samples` on the worker; the answer arrives over the
    /// job receiver. The request carries the turn abort so a barge
    /// cancels the decode instead of orphaning it.
    fn transcribe(
        &self,
        samples: Vec<f32>,
        config: &orator_core::OratorConfig,
        abort: StreamAbort,
    ) -> TranscribeJob {
        let (sender, receiver) = std::sync::mpsc::channel();
        let request = SttRequest {
            samples,
            backend_name: config.stt.backend.clone(),
            language: config.stt.language.clone(),
            model: config.stt.model.clone(),
            endpoint: crate::config::audio_base_url(&config.llm),
            abort,
            reply: sender,
        };
        let _ = self.tx.send(request);
        TranscribeJob { receiver }
    }
}

fn run_loop(
    transport: Transport,
    config: &orator_core::OratorConfig,
    backend: orator_audio::CpalBackend,
) -> crate::ExitCode {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, Event};

    let screen = CrosstermBackend::new(std::io::stdout());
    let mut terminal = match Terminal::new(screen) {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("orator: tui failed to start: {error}");
            return crate::ExitCode::Error;
        }
    };
    // A shutdown mid-session latches the flag; the loop below exits
    // through the quit path between polls.
    let shutdown = Arc::new(AtomicBool::new(false));
    let _watch = {
        let shutdown = Arc::clone(&shutdown);
        crate::SignalWatch::arm(move || shutdown.store(true, Ordering::SeqCst))
    };
    let turn_abort = StreamAbort::new();
    let mut app = TuiApp::new(LiveControl::new(transport, turn_abort.clone()));
    let mut turn_abort = turn_abort;
    let device_rate = backend.sample_rate;
    let mut consumer = backend.consumer;
    let mut carry: Vec<f32> = Vec::new();
    let mut remainder_16k: Vec<f32> = Vec::new();
    // Long-lived speech handles for the run: the STT worker owns its
    // whisper backend on its thread (no ONNX reload per utterance),
    // and the Kokoro session persists across replies.
    let stt_worker = SttWorker::spawn();
    let mut tts_cache = KokoroCache::default();
    let mut transcribing: Option<TranscribeJob> = None;
    let mut player: Option<orator_audio::StreamPlayer> = None;
    loop {
        if tui_loop_should_quit(&app, &shutdown) {
            tui_loop_quit_cleanup(&mut app);
            let _ = render(&mut terminal, &app.state);
            return crate::ExitCode::Success;
        }
        // Drain capture into 16 kHz frames; the gate decides upward.
        // The 16 kHz remainder keeps its own domain across polls (see
        // `drain_capture`): resampling never re-tags it as device-rate.
        drain_capture(
            &mut || consumer.pop().ok(),
            &mut carry,
            &mut remainder_16k,
            device_rate,
            &mut app,
        );
        // Utterance complete: the gate closed over buffered hot spans.
        if transcribing.is_none()
            && !app.pending_utterance.is_empty()
            && !app.gate_speaking()
        {
            let samples = app.take_utterance();
            transcribing = Some(stt_worker.transcribe(samples, config, turn_abort.clone()));
        }
        if let Some(job) = transcribing.as_mut() {
            if let Ok(result) = job.receiver.try_recv() {
                transcribing = None;
                match result {
                    Ok(text) if !text.trim().is_empty() => {
                        ask_and_voice(&mut app, &text, config, &mut player, &turn_abort, &mut tts_cache);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        app.state.push_line(TuiLine::Failure(error));
                    }
                }
            }
        }
        // Playback drained: the reply finished voicing.
        if app.state.voicing {
            if let Some(active) = player.as_ref() {
                if active.played() >= active.pushed() {
                    if let Some(finished) = player.take() {
                        let _ = finished.finish();
                    }
                    app.state.voicing = false;
                    app.event(TurnEvent::Done, false);
                }
            } else {
                app.state.voicing = false;
            }
        }
        if event::poll(std::time::Duration::from_millis(20)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                let barged = map_key(key) == TuiAction::Barge;
                if barged {
                    // Barge abandons queued playout and the pending
                    // transcript: the device stops, nothing buffered
                    // survives the cut.
                    if let Some(abandoned) = player.take() {
                        drop(abandoned);
                    }
                    app.state.voicing = false;
                    transcribing = None;
                }
                app.key(key);
                if barged {
                    // Fresh abort handles so the next turn starts uncut
                    // (the cut above fired the shared old signal).
                    turn_abort = StreamAbort::new();
                    app.control.rearm(turn_abort.clone());
                }
            }
        }
        if render(&mut terminal, &app.state).is_err() {
            return crate::ExitCode::Error;
        }
    }
}

/// Stop the active session on the way out; failures are already
/// transcript lines, never a second exit path.
pub(crate) fn tui_loop_quit_cleanup<C: SessionControl>(app: &mut TuiApp<C>) {
    if app.state.session.is_some() {
        app.stop_session();
    }
}

/// Whether the loop exits this iteration: quit latched or the
/// shutdown flag landed (the watch runs the loop's releaser first).
pub(crate) fn tui_loop_should_quit<C: SessionControl>(
    app: &TuiApp<C>,
    shutdown: &AtomicBool,
) -> bool {
    app.quit || shutdown.load(Ordering::SeqCst)
}

fn drain_capture<C: SessionControl>(
    pop: &mut impl FnMut() -> Option<f32>,
    carry: &mut Vec<f32>,
    remainder_16k: &mut Vec<f32>,
    device_rate: u32,
    app: &mut TuiApp<C>,
) {
    let mut drained: Vec<f32> = Vec::new();
    for _ in 0..4096 {
        match pop() {
            Some(sample) => drained.push(sample),
            None => break,
        }
    }
    if drained.is_empty() {
        return;
    }
    carry.extend_from_slice(&drained);
    let resampled = orator_audio::resample_linear(carry, device_rate, 16_000);
    carry.clear();
    // The tail of the resample output is 16 kHz audio, not device-rate
    // input: it rides `remainder_16k` into the next poll's window
    // instead of re-entering `carry`, so mixed-rate capture frames
    // exactly and single-rate capture behaves byte-identically.
    let mut window = std::mem::take(remainder_16k);
    window.extend_from_slice(&resampled);
    let mut frames = window.chunks_exact(FRAME_SAMPLES);
    for frame in &mut frames {
        app.mic(frame);
    }
    remainder_16k.extend_from_slice(frames.remainder());
}



fn transcribe_samples(
    samples: &[f32],
    backend_name: &str,
    language: &str,
    model: &str,
    endpoint: &str,
    abort: &StreamAbort,
    whisper: &mut WhisperCache,
) -> Result<String, String> {
    if abort.is_cancelled() {
        return Err("orator: transcribe aborted".to_string());
    }
    if backend_name == "audio-server" {
        // No abort hook on the single-shot endpoint call: the HTTP
        // timeout bounds it, and barged results are never delivered
        // (the worker drops them, the loop dropped the job).
        let bundle = orator_core::default_bundle();
        let assured = common_core::runtime::block_on(orator_s2s::ensure_audio_server(
            endpoint, model, &bundle,
        ));
        let owned = match assured {
            Ok(owned) => owned,
            Err(error) => return Err(format!("orator: {error}")),
        };
        let mut session = orator_s2s::AudioSession::new(orator_s2s::S2sConfig {
            endpoint: owned.base_url().to_string(),
            ..orator_s2s::S2sConfig::default()
        });
        let outcome = session.transcribe_blocking(samples);
        owned.release();
        outcome.map_err(|error| format!("orator: {error}"))
    } else if backend_name == "whisper" {
        let Some(dir) = orator_core::models_dir() else {
            return Err(
                "orator: no managed model directory (HOME and XDG_DATA_HOME are unset)".to_string(),
            );
        };
        let path = dir.join(format!("ggml-{model}.bin"));
        if !path.exists() {
            return Err(format!(
                "orator: model missing at {} (fetch it with `orator models pull`)",
                path.display()
            ));
        }
        let backend = whisper.get_or_build(model, language, path);
        backend
            .decode_blocking(samples, abort)
            .map(|segments| {
                segments
                    .iter()
                    .map(|segment| segment.text.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .map_err(|error| format!("orator: {error}"))
    } else {
        Err(format!("orator: unknown STT backend: {backend_name}"))
    }
}

fn ask_and_voice<C: SessionControl>(
    app: &mut TuiApp<C>,
    text: &str,
    config: &orator_core::OratorConfig,
    player: &mut Option<orator_audio::StreamPlayer>,
    turn_abort: &StreamAbort,
    tts: &mut KokoroCache,
) {
    app.event(TurnEvent::Text(text.to_string()), true);
    let mut params = serde_json::json!({"question": text});
    if let Some(session) = app.state.session.as_deref() {
        params["session"] = serde_json::Value::String(session.to_string());
    }
    let reply = match app.control.call("Ask", params) {
        Ok(body) => body
            .get("reply")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        Err(error) => {
            app.state.push_line(TuiLine::Failure(error.text));
            return;
        }
    };
    app.event(TurnEvent::Text(reply.clone()), false);
    if reply.trim().is_empty() {
        return;
    }
    voice_reply(app, &reply, config, player, turn_abort, tts);
}

fn voice_reply<C: SessionControl>(
    app: &mut TuiApp<C>,
    reply: &str,
    config: &orator_core::OratorConfig,
    player: &mut Option<orator_audio::StreamPlayer>,
    turn_abort: &StreamAbort,
    tts: &mut KokoroCache,
) {
    let Some((model, voices)) = orator_tts::default_paths() else {
        app.state.push_line(TuiLine::Failure(
            "orator: no managed model directory (HOME and XDG_DATA_HOME are unset)".to_string(),
        ));
        return;
    };
    // Reused across replies: the session loads once, later replies
    // synthesize against the resident weights.
    let backend = tts.get_or_build(model, voices, &config.tts.voice, config.tts.gpu);
    let chunks: Vec<String> = orator_tts::split_all(reply)
        .into_iter()
        .map(|chunk| chunk.text)
        .collect();
    let opened = match orator_audio::StreamPlayer::open(orator_tts::KOKORO_SAMPLE_RATE) {
        Ok(opened) => opened,
        Err(error) => {
            app.state
                .push_line(TuiLine::Failure(format!("orator: {error}")));
            return;
        }
    };
    *player = Some(opened);
    app.state.voicing = true;
    for chunk in &chunks {
        if turn_abort.is_cancelled() {
            if let Some(abandoned) = player.take() {
                drop(abandoned);
            }
            app.state.voicing = false;
            return;
        }
        match backend.synth_blocking(chunk) {
            Ok(samples) => {
                if let Some(active) = player.as_ref() {
                    active.push(&samples);
                }
                app.event(TurnEvent::Audio(samples.len()), false);
            }
            Err(error) => {
                app.state
                    .push_line(TuiLine::Failure(format!("orator: local voice failed: {error}")));
                if let Some(abandoned) = player.take() {
                    drop(abandoned);
                }
                app.state.voicing = false;
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/tui.rs"]
mod tests;
