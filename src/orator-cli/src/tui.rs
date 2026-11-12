//! Terminal duplex client: mic level, server-audio indicator,
//! scrolling transcript, barge key, session commands.
//!
//! The headless core (`TuiApp` plus `render` over any ratatui backend)
//! is fully scriptable: keystrokes in, transcript lines out, no TTY.
//! The live loop (`run_live`: raw mode, capture, playback) runs only
//! from the `tui` command on a real terminal and is never exercised
//! by tests.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use fluent_concurrency::stream::StreamAbort;
use orator_voice::vad::EnergyVad;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Terminal;

use crate::socket::{
    call_transport, refused_text, unavailable_socket_text, unavailable_ws_text, Transport,
};
use crate::ExitCode;

/// Bounded scrollback: the transcript never grows with session length.
pub const SCROLLBACK: usize = 200;

/// One mic frame at the capture gate (16 kHz mono).
const FRAME_SAMPLES: usize = 320;

/// One transcript row.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One transcript row.
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
/// Voice-turn progress feeding the transcript.
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
/// Keystroke meaning. Ctrl-modified keys drive session control so
/// typing never collides with commands.
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
    /// Cycle the mic mode (auto VAD ↔ push-to-talk).
    Mode,
    /// Toggle the PTT mic (push-to-talk mode only).
    Talk,
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
/// Map one key event to its action. Pure: headless tests drive it
/// without a terminal.
pub fn map_key(key: KeyEvent) -> TuiAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('b') => TuiAction::Barge,
            KeyCode::Char('c' | 'q') => TuiAction::Quit,
            KeyCode::Char('s') => TuiAction::Start,
            KeyCode::Char('e') => TuiAction::Stop,
            KeyCode::Char('l') => TuiAction::List,
            KeyCode::Char('g') => TuiAction::Mode,
            KeyCode::Char('t') => TuiAction::Talk,
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
/// Slash commands mirroring the chat loop vocabulary.
pub enum SlashCommand {
    /// Open a session.
    Start,
    /// Close the session.
    Stop,
    /// Show sessions.
    List,
    /// Release the endpoint and exit.
    Quit,
    /// Switch the mic to push-to-talk.
    Ptt,
    /// Switch the mic to auto VAD.
    Auto,
    /// Toggle the PTT mic.
    Talk,
}

/// Parse a slash command. Anything else (including unknown slashes)
/// is spoken text, never a dispatch — the chat loop rule.
#[must_use]
/// Parse a slash command. Anything else (including unknown slashes)
/// is spoken text, never a dispatch — the chat loop rule.
pub fn parse_command(input: &str) -> Option<SlashCommand> {
    match input {
        "/start" => Some(SlashCommand::Start),
        "/stop" => Some(SlashCommand::Stop),
        "/list" => Some(SlashCommand::List),
        "/quit" | "/exit" => Some(SlashCommand::Quit),
        "/ptt" => Some(SlashCommand::Ptt),
        "/auto" => Some(SlashCommand::Auto),
        "/talk" => Some(SlashCommand::Talk),
        _ => None,
    }
}

/// Control failure: exit code plus the exact line shown. Built from
/// the shared constructors the plain CLI prints from — one wording,
/// two renderers.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Control failure: exit code plus the exact line shown. Built from
/// the shared constructors the plain CLI prints from — one wording,
/// two renderers.
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
                        unavailable_socket_text(orator::socket_path().as_deref())
                    }
                    Transport::Ws => {
                        let (config, _) = orator::config::load_hierarchical();
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
/// Mic gate: scores local capture frames with the default detector
/// (the calibrated constants live there, never restated here) and
/// passes only hot spans upward. Level always tracks the frame.
pub struct MicGate {
    vad: EnergyVad,
    level: f32,
}

impl MicGate {
    /// Gate with the default detector constants.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(&orator::VadConfig::default())
    }

    /// Gate with every tunable taken from `VadConfig`: the single
    /// tunable surface for live capture, file turns, and the TUI.
    #[must_use]
    pub fn with_config(config: &orator::VadConfig) -> Self {
        Self {
            vad: EnergyVad::with_config(config),
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
        self.level = common_core::pcm::root_mean_square_f32(samples).clamp(0.0, 1.0);
        self.vad.push(samples);
        self.vad.speaking()
    }

    /// Drop the detector state (open spans close): the echo tail shed
    /// at voice end must not commit as a phantom utterance.
    pub fn reset(&mut self) {
        self.vad.reset();
    }
}

impl Default for MicGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Meter bar of `width` cells, clamped — never overflows, never
/// negative.
#[must_use]
/// Meter bar of `width` cells, clamped — never overflows, never
/// negative.
pub fn level_bar(level: f32, width: usize) -> String {
    let filled = (level.clamp(0.0, 1.0) * width as f32).round() as usize;
    let filled = filled.min(width);
    "█".repeat(filled) + &"░".repeat(width - filled)
}

/// Mic capture mode: continuous voice activity detection, or
/// explicit push-to-talk for speaker setups where the assistant's
/// own voice would otherwise loop back through the mic as input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Mic capture mode: continuous voice activity detection, or
/// explicit push-to-talk for speaker setups where the assistant's
/// own voice would otherwise loop back through the mic as input.
pub enum MicMode {
    /// VAD-gated capture (commits suppressed while voicing, so the
    /// reply never becomes the next question).
    #[default]
    Auto,
    /// Mic hot only while toggled on; toggle-off endpoints the turn.
    PushToTalk,
}

impl MicMode {
    /// Short label for the status header.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::PushToTalk => "ptt",
        }
    }
}

/// Live TUI state: transcript, meter, voicing, session, input line.
#[derive(Debug, Clone, PartialEq)]
/// Live TUI state: transcript, meter, voicing, session, input line.
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
    /// Mic capture mode (header readout).
    pub mic_mode: MicMode,
    /// PTT mic latched hot (header readout).
    pub talking: bool,
}

impl TuiState {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            mic_level: 0.0,
            voicing: false,
            session: None,
            input: String::new(),
            mic_mode: MicMode::Auto,
            talking: false,
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
    /// PTT toggle-off endpoints the buffered turn even when the VAD
    /// still holds speech open (consumed once by the loop).
    pub ptt_commit: bool,
    /// Echo suppression: while set, hot spans move the meter but never
    /// buffer — the reply playing on the speakers must not become the
    /// next question. The loop sets this from the voicing flag.
    pub suppress_utterance: bool,
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
        Self::with_config(control, &orator::VadConfig::default())
    }

    /// App over `control` with the gate tuned from `VadConfig`.
    #[must_use]
    pub fn with_config(control: C, vad: &orator::VadConfig) -> Self {
        Self {
            state: TuiState::new(),
            control,
            pending_utterance: Vec::new(),
            pending_dropped: 0,
            quit: false,
            ptt_commit: false,
            suppress_utterance: false,
            gate: MicGate::with_config(vad),
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
                // A cut drops everything buffered (it may hold reply
                // echo) and unlatches the PTT mic: nothing survives.
                self.shed_utterance();
                self.state.talking = false;
                self.ptt_commit = false;
                self.state
                    .push_line(TuiLine::Status("barge: cut the in-flight turn".to_string()));
            }
            TuiAction::Quit => self.quit(),
            TuiAction::Start => self.start_session(),
            TuiAction::Stop => self.stop_session(),
            TuiAction::List => self.list_sessions(),
            TuiAction::Mode => self.cycle_mic_mode(),
            TuiAction::Talk => self.toggle_talk(),
            TuiAction::Type(char) => self.state.input.push(char),
            TuiAction::Backspace => {
                self.state.input.pop();
            }
            TuiAction::Send => self.send_input(),
            TuiAction::Ignore => {}
        }
    }

    /// Cycle the mic mode. Leaving PTT unlatches the talk mic and
    /// drops its buffer; the mode readout rides the shared state.
    pub fn cycle_mic_mode(&mut self) {
        match self.state.mic_mode {
            MicMode::Auto => {
                self.state.mic_mode = MicMode::PushToTalk;
                self.state.talking = false;
                self.ptt_commit = false;
                self.state.push_line(TuiLine::Status(
                    "mic: push-to-talk (Ctrl+T talks, toggle off sends)".to_string(),
                ));
            }
            MicMode::PushToTalk => {
                self.state.mic_mode = MicMode::Auto;
                self.state.talking = false;
                self.ptt_commit = false;
                self.state.push_line(TuiLine::Status(
                    "mic: auto VAD (commits pause while voicing)".to_string(),
                ));
            }
        }
    }

    /// Toggle the PTT talk mic. Toggle-off endpoints the buffered
    /// turn through `ptt_commit` even when the VAD still holds speech
    /// open. Outside PTT mode this names the remedy instead of acting.
    pub fn toggle_talk(&mut self) {
        if self.state.mic_mode != MicMode::PushToTalk {
            self.state.push_line(TuiLine::Status(
                "talk is for push-to-talk mode (Ctrl+G to switch)".to_string(),
            ));
            return;
        }
        self.state.talking = !self.state.talking;
        if self.state.talking {
            self.state.push_line(TuiLine::Status(
                "talk: mic hot — toggle off to send".to_string(),
            ));
        } else {
            self.ptt_commit = true;
            self.state
                .push_line(TuiLine::Status("talk: mic off — sending".to_string()));
        }
    }

    /// One captured mic frame: meter always follows; hot spans buffer
    /// unless suppressed (voicing echo) or the PTT mic is cold. The
    /// meter stays live in every mode so silence-vs-signal is visible
    /// even while buffering pauses.
    pub fn mic(&mut self, samples: &[f32]) {
        let hot = self.gate.push(samples);
        self.state.mic_level = self.gate.level();
        let ptt_hot = self.state.mic_mode == MicMode::PushToTalk && !self.state.talking;
        if !hot || self.suppress_utterance || ptt_hot {
            return;
        }
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

    /// Shed the buffered utterance (echo tail, barge cut): counted,
    /// never transcribed. Returns the shed sample count.
    pub fn shed_utterance(&mut self) -> usize {
        let shed = self.pending_utterance.len();
        self.pending_utterance.clear();
        self.pending_dropped += shed as u64;
        self.gate.reset();
        shed
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
                SlashCommand::Ptt => {
                    if self.state.mic_mode != MicMode::PushToTalk {
                        self.cycle_mic_mode();
                    }
                }
                SlashCommand::Auto => {
                    if self.state.mic_mode != MicMode::Auto {
                        self.cycle_mic_mode();
                    }
                }
                SlashCommand::Talk => self.toggle_talk(),
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
                // A saturated daemon refused the turn: keep the typed
                // message on the input line instead of dropping it, so
                // Enter retries. The refusal wording stays exactly the
                // shared text; the status names the remedy.
                if error.text.to_ascii_lowercase().contains("saturated") {
                    self.state.input = input;
                    self.state.push_line(TuiLine::Failure(error.text));
                    self.state.push_line(TuiLine::Status(
                        "message kept — press Enter to retry".to_string(),
                    ));
                } else {
                    self.state.push_line(TuiLine::Said(input));
                    self.state.push_line(TuiLine::Failure(error.text));
                }
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
            let mic = match (state.mic_mode, state.talking) {
                (MicMode::Auto, _) => "mic:auto".to_string(),
                (MicMode::PushToTalk, true) => "mic:ptt TALK".to_string(),
                (MicMode::PushToTalk, false) => "mic:ptt".to_string(),
            };
            let header =
                Paragraph::new(format!("orator tui | session: {session} | {voice} | {mic}"))
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
                    .title("say (Ctrl+B barge · Ctrl+Q quit · Ctrl+G mode · Ctrl+T talk)"),
            );
            frame.render_widget(prompt, rows[3]);
        })
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    Ok(())
}

/// Run the live duplex client: capture, gate, transcribe, ask, voice.
/// Raw mode and the alternate screen require a terminal; anything
/// else refuses with a named usage error before touching the display.
pub fn run_live(transport: Transport, config: &orator::OratorConfig) -> crate::ExitCode {
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
    if ratatui::crossterm::execute!(stdout, ratatui::crossterm::terminal::EnterAlternateScreen)
        .is_err()
    {
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
        eprintln!("orator: tui needs a terminal (alternate screen refused)");
        return crate::ExitCode::Usage;
    }
    let outcome = run_loop(transport, backend, config);
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
    let _ =
        ratatui::crossterm::execute!(stdout, ratatui::crossterm::terminal::LeaveAlternateScreen);
    outcome
}

struct TranscribeJob {
    receiver: std::sync::mpsc::Receiver<Result<String, String>>,
}

/// One TUI transcription request: samples plus the transport the
/// worker dials, answered once over `reply`. The worker owns no
/// inference — transcription runs on the daemon.
struct SttRequest {
    samples: Vec<f32>,
    transport: Transport,
    abort: StreamAbort,
    reply: std::sync::mpsc::Sender<Result<String, String>>,
}

/// Long-lived TUI speech worker. One thread forwards utterances to the
/// daemon `Transcribe` method; the loop never blocks on the network
/// and barged turns abort before dialing (or drop on delivery when
/// the abort lands mid-call — the loop already dropped the job).
struct SttWorker {
    tx: std::sync::mpsc::Sender<SttRequest>,
}

impl SttWorker {
    /// Spawn the worker; it parks until the first utterance.
    /// Transcription runs daemon-first, falling back to local
    /// specialized ASR when the daemon has no voice serving.
    fn spawn(config: &orator::OratorConfig) -> Self {
        let config = config.clone();
        Self::spawn_with(move |samples, transport, abort| {
            crate::fallback::transcribe_dial(samples, transport, abort, &config)
        })
    }

    /// Spawn the worker with a scripted dial (tests answer without a
    /// daemon; production passes [`transcribe_samples`]).
    fn spawn_with(
        dial: impl Fn(&[f32], Transport, &StreamAbort) -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<SttRequest>();
        std::thread::spawn(move || {
            for request in rx {
                if request.abort.is_cancelled() {
                    let _ = request
                        .reply
                        .send(Err("orator: transcribe aborted".to_string()));
                    continue;
                }
                let outcome = dial(&request.samples, request.transport, &request.abort);
                // Barge landed mid-call: the transcript is stale by
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
    /// cancels the call instead of orphaning it.
    fn transcribe(
        &self,
        samples: Vec<f32>,
        transport: Transport,
        abort: StreamAbort,
    ) -> TranscribeJob {
        let (sender, receiver) = std::sync::mpsc::channel();
        let request = SttRequest {
            samples,
            transport,
            abort,
            reply: sender,
        };
        let _ = self.tx.send(request);
        TranscribeJob { receiver }
    }
}

/// One completed voice turn: the reply text plus its spoken audio.
/// `voice` carries the `Say` outcome separately so a failed voicing
/// still shows its text: the transcript never loses words to a
/// playback problem.
#[derive(Debug)]
/// One completed voice turn: the reply text plus its spoken audio.
/// `voice` carries the `Say` outcome separately so a failed voicing
/// still shows its text: the transcript never loses words to a
/// playback problem.
struct VoiceTurn {
    /// Reply text (always shown, even when voicing failed).
    reply: String,
    /// Spoken audio at its server rate, or the voicing failure.
    /// `None` (no error) means the reply was empty: nothing to voice.
    voice: Result<Option<(Vec<f32>, u32)>, String>,
}

/// One TUI voice request: text plus the transport the worker dials,
/// answered once over `reply`. The worker owns no inference — the
/// daemon answers `Ask`, then `Say`.
struct VoiceRequest {
    text: String,
    session: Option<String>,
    transport: Transport,
    abort: StreamAbort,
    reply: std::sync::mpsc::Sender<Result<VoiceTurn, String>>,
}

struct VoiceJob {
    receiver: std::sync::mpsc::Receiver<Result<VoiceTurn, String>>,
}

/// Long-lived TUI voice worker. One thread runs `Ask` then `Say`
/// against the daemon; the loop never blocks on the network, so keys
/// (barge, quit, typing) stay live through slow turns and a cut
/// drops the job before delivery — exactly the `SttWorker` shape one
/// layer up the turn.
struct VoiceWorker {
    tx: std::sync::mpsc::Sender<VoiceRequest>,
}

impl VoiceWorker {
    /// Spawn the worker; it parks until the first turn. Voice runs
    /// daemon-first through `ask_then_voice`, falling back to local
    /// specialized inference when the daemon has no voice serving.
    fn spawn(config: &orator::OratorConfig) -> Self {
        let config = config.clone();
        Self::spawn_with(move |text, session, transport, abort| {
            ask_then_voice(text, session, transport, abort, &config)
        })
    }

    /// Spawn the worker with a scripted dial (tests answer without a
    /// daemon; production passes [`ask_then_voice`]).
    fn spawn_with(
        dial: impl Fn(&str, Option<&str>, Transport, &StreamAbort) -> Result<VoiceTurn, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<VoiceRequest>();
        std::thread::spawn(move || {
            for request in rx {
                if request.abort.is_cancelled() {
                    let _ = request
                        .reply
                        .send(Err("orator: voice turn aborted".to_string()));
                    continue;
                }
                let outcome = dial(
                    &request.text,
                    request.session.as_deref(),
                    request.transport,
                    &request.abort,
                );
                // Barge landed mid-turn: the answer is stale by
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

    /// Run one voice turn on the worker; the answer arrives over the
    /// job receiver. The request carries the turn abort so a barge
    /// cuts the turn instead of orphaning it.
    fn turn(
        &self,
        text: String,
        session: Option<String>,
        transport: Transport,
        abort: StreamAbort,
    ) -> VoiceJob {
        let (sender, receiver) = std::sync::mpsc::channel();
        let request = VoiceRequest {
            text,
            session,
            transport,
            abort,
            reply: sender,
        };
        let _ = self.tx.send(request);
        VoiceJob { receiver }
    }
}

/// One voice turn: daemon Ask then daemon Say, falling back to local
/// inference (direct endpoint ask, kokoro synth) when the daemon has
/// no voice serving. The transcript shows the full reply; the voice
/// carries the concise form (short replies pass through, long ones
/// voice a summary — the single [`crate::ask::concise_for_speech`]
/// home shared with `ask --speak`). A failed `Say` still returns its
/// text with the voicing failure attached, so words are never lost to
/// a playback problem.
fn ask_then_voice(
    text: &str,
    session: Option<&str>,
    transport: Transport,
    abort: &StreamAbort,
    config: &orator::OratorConfig,
) -> Result<VoiceTurn, String> {
    if abort.is_cancelled() {
        return Err("orator: voice turn aborted".to_string());
    }
    ask_then_voice_with(
        text,
        session,
        &|text, session| crate::fallback::ask_dial(text, session, transport, abort, config),
        &|text| {
            // Cut between the answer and its voicing skips a
            // pointless synthesis; the worker drops the turn anyway.
            if abort.is_cancelled() {
                return Err("orator: voice turn aborted".to_string());
            }
            crate::fallback::say_dial(text, transport, abort, config)
        },
    )
}

/// Ask one question through the daemon transport. Pure dial seam for
/// [`ask_then_voice_with`]: production passes this, tests script it.
pub(crate) fn live_ask(
    text: &str,
    session: Option<&str>,
    transport: Transport,
    abort: &StreamAbort,
) -> Result<String, String> {
    // Thin client: the daemon owns the LLM. This side ships text —
    // never inference.
    let mut control = LiveControl::new(transport, abort.clone());
    let mut params = serde_json::json!({"question": text});
    if let Some(session) = session {
        params["session"] = serde_json::Value::String(session.to_string());
    }
    match control.call("Ask", params) {
        Ok(body) => Ok(body
            .get("reply")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string()),
        Err(error) => Err(error.text),
    }
}

/// Voice one reply through the daemon transport. Pure dial seam for
/// [`ask_then_voice_with`]: production passes this, tests script it.
pub(crate) fn live_say(
    text: &str,
    transport: Transport,
    abort: &StreamAbort,
) -> Result<(Vec<f32>, u32), String> {
    // Thin client: the daemon owns TTS. This side plays the daemon's
    // audio bytes locally — playout of provided audio, never inference.
    let mut control = LiveControl::new(transport, abort.clone());
    match control.call("Say", serde_json::json!({"text": text})) {
        Ok(body) => {
            crate::say_chain::decode_say_audio(&body).map_err(|error| format!("orator: {error}"))
        }
        Err(error) => Err(error.text),
    }
}

/// Ask dial for [`ask_then_voice_with`]: question plus session in,
/// reply text or the failure line out.
type AskDial<'a> = &'a dyn Fn(&str, Option<&str>) -> Result<String, String>;

/// Say dial for [`ask_then_voice_with`]: spoken text in, PCM samples
/// plus rate, or the failure line, out.
type SayDial<'a> = &'a dyn Fn(&str) -> Result<(Vec<f32>, u32), String>;

/// One voice turn from its dials: `ask` for the reply text, then
/// `say` for its audio. The transcript shows the full reply; the
/// voice carries the concise form (short replies pass through, long
/// ones voice a summary — the single [`crate::ask::concise_for_speech`]
/// home shared with `ask --speak`). A failed `say` still returns its
/// text with the voicing failure attached, so words are never lost to
/// a playback problem. An empty reply voices nothing (no dial).
fn ask_then_voice_with(
    text: &str,
    session: Option<&str>,
    ask: AskDial<'_>,
    say: SayDial<'_>,
) -> Result<VoiceTurn, String> {
    let reply = ask(text, session)?;
    if reply.trim().is_empty() {
        return Ok(VoiceTurn {
            reply,
            voice: Ok(None),
        });
    }
    let spoken = crate::ask::concise_for_speech(&reply);
    match say(&spoken) {
        Ok((samples, rate)) => Ok(VoiceTurn {
            reply,
            voice: Ok(Some((samples, rate))),
        }),
        Err(error) => Ok(VoiceTurn {
            reply,
            voice: Err(error),
        }),
    }
}

fn run_loop(
    transport: Transport,
    backend: orator_audio::CpalBackend,
    config: &orator::OratorConfig,
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
    let mut app = TuiApp::with_config(LiveControl::new(transport, turn_abort.clone()), &config.vad);
    let mut turn_abort = turn_abort;
    let device_rate = backend.sample_rate;
    let mut consumer = backend.consumer;
    let mut carry: Vec<f32> = Vec::new();
    let mut remainder_16k: Vec<f32> = Vec::new();
    // One thin worker forwards utterances to the daemon; the TUI
    // owns no inference backends.
    let stt_worker = SttWorker::spawn(config);
    let mut transcribing: Option<TranscribeJob> = None;
    // Voice turns ride their own worker for the same reason: `Ask`
    // plus `Say` block on the network, and keys (barge, quit,
    // typing) must stay live through slow turns. A cut drops the job
    // before delivery — never stale speech after a barge.
    let voice_worker = VoiceWorker::spawn(config);
    let mut voicing_turn: Option<VoiceJob> = None;
    let mut player: Option<orator_audio::StreamPlayer> = None;
    // Saturated-daemon cooldown: a refused transcribe sheds mic input
    // instead of hot-looping the bound. Realtime stays lossy with a
    // counter; the shed line itself is throttled so the transcript
    // shows load, not spam.
    let mut cooldown_until: Option<std::time::Instant> = None;
    let mut last_shed_line: Option<std::time::Instant> = None;
    loop {
        if tui_loop_should_quit(&app, &shutdown) {
            tui_loop_quit_cleanup(&mut app);
            let _ = render(&mut terminal, &app.state);
            return crate::ExitCode::Success;
        }
        // Drain capture into 16 kHz frames; the gate decides upward.
        // Echo suppression rides the voicing flag: while the reply
        // plays, hot spans move the meter but never buffer, so the
        // speakers cannot become the next question.
        // The 16 kHz remainder keeps its own domain across polls (see
        // `drain_capture`): resampling never re-tags it as device-rate.
        app.suppress_utterance = app.state.voicing;
        // PTT-cold mics move the meter but buffer nothing (the TUI
        // rule, now an explicit argument instead of a method check).
        let ptt_cold = app.state.mic_mode == MicMode::PushToTalk && !app.state.talking;
        drain_capture(
            &mut || consumer.pop().ok(),
            &mut carry,
            &mut remainder_16k,
            device_rate,
            &mut app.gate,
            &mut app.pending_utterance,
            &mut app.pending_dropped,
            !app.suppress_utterance && !ptt_cold,
        );
        app.state.mic_level = app.gate.level();
        // Utterance complete: the gate closed over buffered hot spans,
        // or PTT toggled off (which endpoints even a hot VAD span).
        // Past a saturated refusal the loop sheds instead of
        // submitting: drop the buffer with a counter and tell the
        // transcript (throttled), so one overloaded daemon is one
        // line, not a hot retry loop.
        let cooling = cooldown_until.is_some_and(|until| std::time::Instant::now() < until);
        if cooling && !app.pending_utterance.is_empty() && !app.gate_speaking() {
            let shed = app.take_utterance().len();
            app.pending_dropped += shed as u64;
            let due = last_shed_line.is_none_or(|at| {
                at.elapsed() > std::time::Duration::from_secs(config.turn.shed_notice_secs)
            });
            if due {
                last_shed_line = Some(std::time::Instant::now());
                app.state.push_line(TuiLine::Status(format!(
                    "daemon saturated; shedding mic input until it drains ({} sample(s) shed)",
                    app.pending_dropped
                )));
            }
        }
        // PTT toggle-off endpoints the turn: consume the flag every
        // poll (never stale), commit when audio actually buffered.
        let ptt_end = std::mem::take(&mut app.ptt_commit);
        if transcribing.is_none()
            && !cooling
            && !app.pending_utterance.is_empty()
            && (!app.gate_speaking() || ptt_end)
        {
            let samples = app.take_utterance();
            transcribing = Some(stt_worker.transcribe(samples, transport, turn_abort.clone()));
        }
        if let Some(job) = transcribing.as_mut() {
            if let Ok(result) = job.receiver.try_recv() {
                transcribing = None;
                match result {
                    Ok(text) if !text.trim().is_empty() => {
                        cooldown_until = None;
                        // The user turn shows at once; the assistant
                        // turn arrives over the voice job below, so a
                        // slow answer never freezes the transcript.
                        app.event(TurnEvent::Text(text.clone()), true);
                        let session = app.state.session.clone();
                        voicing_turn =
                            Some(voice_worker.turn(text, session, transport, turn_abort.clone()));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if error.to_ascii_lowercase().contains("saturated") {
                            cooldown_until = Some(
                                std::time::Instant::now()
                                    + std::time::Duration::from_secs(
                                        config.turn.saturated_cooldown_secs,
                                    ),
                            );
                        }
                        app.state.push_line(TuiLine::Failure(error));
                    }
                }
            }
        }
        // Voice turn complete: transcript first, then playout. A cut
        // already dropped the job, so anything delivered here is fresh.
        if let Some(job) = voicing_turn.as_mut() {
            if let Ok(result) = job.receiver.try_recv() {
                voicing_turn = None;
                match result {
                    Ok(turn) => {
                        app.event(TurnEvent::Text(turn.reply.clone()), false);
                        match turn.voice {
                            Ok(Some((samples, rate))) => {
                                match orator_audio::StreamPlayer::open(rate) {
                                    // No tap: the TUI capture path runs
                                    // without a filter, so reference
                                    // would have no reader.
                                    Ok(opened) => {
                                        player = Some(opened);
                                        app.state.voicing = true;
                                        if let Some(active) = player.as_ref() {
                                            active.push(&samples);
                                        }
                                        app.event(TurnEvent::Audio(samples.len()), false);
                                    }
                                    Err(error) => {
                                        app.state.push_line(TuiLine::Failure(format!(
                                            "orator: {error}"
                                        )));
                                    }
                                }
                            }
                            Ok(None) => {}
                            Err(voice_error) => {
                                app.state.push_line(TuiLine::Failure(voice_error));
                            }
                        }
                    }
                    Err(error) => {
                        app.state.push_line(TuiLine::Failure(error));
                    }
                }
            }
        }
        // Playback drained: the reply finished voicing. The mic
        // buffer sheds here (echo tail) with the gate reset, so the
        // speakers' last syllables never commit as a phantom turn.
        if app.state.voicing {
            if let Some(active) = player.as_ref() {
                if active.played() >= active.pushed() {
                    if let Some(finished) = player.take() {
                        let _ = finished.finish();
                    }
                    app.state.voicing = false;
                    app.shed_utterance();
                    app.event(TurnEvent::Done, false);
                }
            } else {
                app.state.voicing = false;
            }
        }
        if event::poll(std::time::Duration::from_millis(u64::from(
            config.turn.poll_ms,
        )))
        .unwrap_or(false)
        {
            if let Ok(Event::Key(key)) = event::read() {
                let barged = map_key(key) == TuiAction::Barge;
                if barged {
                    // Barge abandons queued playout, the pending
                    // transcript, and the in-flight voice turn: the
                    // device stops, nothing buffered survives the cut,
                    // and a late answer never delivers stale speech.
                    if let Some(abandoned) = player.take() {
                        drop(abandoned);
                    }
                    app.state.voicing = false;
                    transcribing = None;
                    voicing_turn = None;
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

/// Drain device-rate capture into 16 kHz frames; hot spans buffer
/// into `utterance` unless `accept` is false (voicing echo or a cold
/// PTT mic: the meter still moves, nothing buffers). Over-cap input
/// drops the oldest first with a counter. Echo control lives in the
/// OS audio path, so capture here is gate-then-buffer only. Single
/// home for the TUI and `chat` capture loops (same resampling, same
/// remainder domain, same cap).
pub fn drain_capture(
    pop: &mut impl FnMut() -> Option<f32>,
    carry: &mut Vec<f32>,
    remainder_16k: &mut Vec<f32>,
    device_rate: u32,
    gate: &mut MicGate,
    utterance: &mut Vec<f32>,
    dropped: &mut u64,
    accept: bool,
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
    let mut frames = window.chunks_exact_mut(FRAME_SAMPLES);
    for frame in &mut frames {
        // The gate scores every frame (meter alive, barge timing
        // unchanged); hot, accepted frames buffer verbatim.
        if !gate.push(frame) || !accept {
            continue;
        }
        utterance.extend_from_slice(frame);
        let excess = utterance.len().saturating_sub(MAX_PENDING_UTTERANCE);
        if excess > 0 {
            utterance.drain(..excess);
            *dropped += excess as u64;
        }
    }
    remainder_16k.extend_from_slice(frames.into_remainder());
}

pub(crate) fn transcribe_samples(
    samples: &[f32],
    transport: Transport,
    abort: &StreamAbort,
) -> Result<String, String> {
    if abort.is_cancelled() {
        return Err("orator: transcribe aborted".to_string());
    }
    // Thin client: the daemon owns STT (whisper, audio-server, or
    // supervised voice server). The TUI ships 16 kHz mono and reads
    // text back — never inference.
    let mut control = LiveControl::new(transport, abort.clone());
    match control.call(
        "Transcribe",
        crate::transcribe::encode_transcribe_audio(samples),
    ) {
        Ok(body) => crate::transcribe::decode_transcribe_text(&body),
        Err(error) => Err(error.text),
    }
}

#[cfg(test)]
#[path = "../tests/tui.rs"]
mod tests;