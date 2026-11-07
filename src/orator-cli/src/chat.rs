//! `chat` turn helpers: slash parsing, voice/text mode selection,
//! and role serving labels. Pure and hermetic; the blocking loop in
//! the binary composes these with the fallback dials.

use crate::tui::MicMode;

/// Voice output mode for one assistant reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatVoiceMode {
    /// No synthesis: text only.
    Off,
    /// Voice the concise form (shared `concise_for_speech` home).
    #[default]
    Short,
    /// Voice the complete reply, chunk-streamed.
    Full,
}

/// Parse `/voice off|short|full`. Anything else refuses with the three
/// accepted spellings named.
pub fn parse_voice_mode(raw: &str) -> Result<ChatVoiceMode, String> {
    match raw {
        "off" => Ok(ChatVoiceMode::Off),
        "short" => Ok(ChatVoiceMode::Short),
        "full" => Ok(ChatVoiceMode::Full),
        _ => Err(format!(
            "orator: unknown voice mode: {raw:?} (expected off, short, or full)"
        )),
    }
}

/// Text output mode for one assistant reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatTextMode {
    /// Concise summary plus the full answer printed once.
    Short,
    /// Verbatim reply only (previous `chat` behavior).
    #[default]
    Full,
}

/// Parse `/text short|full`. Anything else refuses with both
/// spellings named.
pub fn parse_text_mode(raw: &str) -> Result<ChatTextMode, String> {
    match raw {
        "short" => Ok(ChatTextMode::Short),
        "full" => Ok(ChatTextMode::Full),
        _ => Err(format!(
            "orator: unknown text mode: {raw:?} (expected short or full)"
        )),
    }
}

/// One parsed chat slash. Unknown slashes are not commands: like the
/// TUI rule, they are spoken text, never a dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatSlash {
    /// Release and exit.
    Quit,
    /// Clear pending audio and continue.
    Reset,
    /// Voice a WAV file through the transcribe path (`/wav <path>`).
    Wav(String),
    /// Read a WAV file through the transcribe path (`/input <path>`).
    Input(String),
    /// Switch the mic (`/mic auto|ptt`).
    Mic(MicMode),
    /// Toggle the PTT mic (`/talk`).
    Talk,
    /// Switch voice output (`/voice off|short|full`).
    Voice(ChatVoiceMode),
    /// Switch text output (`/text short|full`).
    Text(ChatTextMode),
    /// Switch voice barge-in (`/barge on|off`, default on): talking
    /// over a reply cuts the live turn once the VAD span sustains
    /// past the configured bound.
    Barge(bool),
}

/// One parsed chat line: a slash command or spoken text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatLine {
    /// Slash command.
    Slash(ChatSlash),
    /// Spoken text.
    Say(String),
}

/// Parse one chat line. Empty lines are `None` (skip); unknown
/// slashes are `Some(Say)` — spoken, never dispatched.
pub fn parse_chat_line(line: &str) -> Option<ChatLine> {
    let input = line.trim();
    if input.is_empty() {
        return None;
    }
    if input == "/quit" || input == "/exit" {
        return Some(ChatLine::Slash(ChatSlash::Quit));
    }
    if input == "/reset" {
        return Some(ChatLine::Slash(ChatSlash::Reset));
    }
    if input == "/talk" {
        return Some(ChatLine::Slash(ChatSlash::Talk));
    }
    if let Some(path) = input.strip_prefix("/wav ") {
        return Some(ChatLine::Slash(ChatSlash::Wav(path.trim().to_string())));
    }
    if input == "/wav" {
        return Some(ChatLine::Slash(ChatSlash::Wav(String::new())));
    }
    if let Some(path) = input.strip_prefix("/input ") {
        return Some(ChatLine::Slash(ChatSlash::Input(path.trim().to_string())));
    }
    if input == "/input" {
        return Some(ChatLine::Slash(ChatSlash::Input(String::new())));
    }
    if let Some(mode) = input.strip_prefix("/mic ") {
        return match mode.trim() {
            "auto" => Some(ChatLine::Slash(ChatSlash::Mic(MicMode::Auto))),
            "ptt" => Some(ChatLine::Slash(ChatSlash::Mic(MicMode::PushToTalk))),
            _ => Some(ChatLine::Say(input.to_string())),
        };
    }
    if let Some(mode) = input.strip_prefix("/voice ") {
        return match parse_voice_mode(mode.trim()) {
            Ok(voice) => Some(ChatLine::Slash(ChatSlash::Voice(voice))),
            Err(_) => Some(ChatLine::Say(input.to_string())),
        };
    }
    if let Some(mode) = input.strip_prefix("/text ") {
        return match parse_text_mode(mode.trim()) {
            Ok(text) => Some(ChatLine::Slash(ChatSlash::Text(text))),
            Err(_) => Some(ChatLine::Say(input.to_string())),
        };
    }
    if let Some(mode) = input.strip_prefix("/barge ") {
        return match mode.trim() {
            "on" => Some(ChatLine::Slash(ChatSlash::Barge(true))),
            "off" => Some(ChatLine::Slash(ChatSlash::Barge(false))),
            _ => Some(ChatLine::Say(input.to_string())),
        };
    }
    Some(ChatLine::Say(input.to_string()))
}

/// Spoken form of `reply` under `mode`: `None` for off, the shared
/// concise form for short, the complete reply for full (voiced
/// chunk-streamed by the caller).
#[must_use]
pub fn spoken_for_voice(reply: &str, mode: ChatVoiceMode) -> Option<String> {
    match mode {
        ChatVoiceMode::Off => None,
        ChatVoiceMode::Short => Some(crate::ask::concise_for_speech(reply)),
        ChatVoiceMode::Full => Some(reply.to_string()),
    }
}

/// Printed lines for `reply` under `mode`: full prints the reply
/// verbatim (previous behavior); short prints the concise summary
/// plus the full answer once (a single line when they agree).
#[must_use]
pub fn display_for_text(reply: &str, mode: ChatTextMode) -> Vec<String> {
    match mode {
        ChatTextMode::Full => vec![reply.to_string()],
        ChatTextMode::Short => {
            let concise = crate::ask::concise_for_speech(reply);
            if concise == reply {
                vec![reply.to_string()]
            } else {
                vec![concise, reply.to_string()]
            }
        }
    }
}

/// Chat session starts from the `chat.*` leaves: valid spellings map
/// to their modes, anything else falls back to the previous hardcoded
/// starts (short voice, full text, auto mic, barge on). The loop names
/// the bad value when it falls back, so config garbage is loud at the
/// session start instead of refusing the session or voicing silently.
#[must_use]
pub fn initial_voice_mode(config: &orator_core::OratorConfig) -> ChatVoiceMode {
    parse_voice_mode(config.chat.voice.trim()).unwrap_or_default()
}

/// See [`initial_voice_mode`]: text starts, same fallback rule.
#[must_use]
pub fn initial_text_mode(config: &orator_core::OratorConfig) -> ChatTextMode {
    parse_text_mode(config.chat.text.trim()).unwrap_or_default()
}

/// See [`initial_voice_mode`]: mic starts, same fallback rule.
#[must_use]
pub fn initial_mic_mode(config: &orator_core::OratorConfig) -> MicMode {
    match config.chat.mic.trim() {
        "auto" => MicMode::Auto,
        "ptt" => MicMode::PushToTalk,
        _ => MicMode::default(),
    }
}

/// See [`initial_voice_mode`]: barge-in starts straight off the leaf.
#[must_use]
pub fn initial_barge_on(config: &orator_core::OratorConfig) -> bool {
    config.chat.barge
}

/// Model+backend label logged for one LLM turn: the chat role head
/// over the daemon-first endpoint (head wins, `llm.model` leaf as
/// fallback — defaults seed them equal).
#[must_use]
pub fn llm_serving(config: &orator_core::OratorConfig) -> (String, String) {
    (
        crate::ask::resolve_ask_model(config),
        "daemon-or-direct".to_string(),
    )
}

/// Model+backend label logged for one ASR turn: the locally served
/// weights when present (whisper or moonshine through the unified
/// role walk), else the first listed asr role entry with its
/// declared backend. This names the LOCAL leg only — daemon-served
/// turns log [`daemon_asr_serving`] instead (see `transcribe_leg`).
#[must_use]
pub fn asr_serving(config: &orator_core::OratorConfig) -> (String, String) {
    if let Ok(found) = crate::fallback::resolve_local_asr(config) {
        return (found.tag, found.backend.name().to_string());
    }
    let model = orator_core::resolve_role_model(&config.roles, "asr")
        .unwrap_or_else(|| config.stt.model.clone());
    let backend = config
        .models
        .models
        .get(&model)
        .and_then(|spec| spec.primary_backend())
        .map_or_else(|| config.stt.backend.clone(), str::to_string);
    (model, backend)
}

/// Model+backend label logged for a daemon-served ASR turn: the
/// configured voice model over the audio-server protocol. The daemon
/// assures this tag before serving it, so the loop names the served
/// model — never the local leg's fallback pick.
#[must_use]
pub fn daemon_asr_serving(config: &orator_core::OratorConfig) -> (String, String) {
    (config.s2s.model.clone(), "audio-server".to_string())
}

/// Model+backend label logged for one TTS turn: the first listed tts
/// role entry with its declared backend (kokoro default).
#[must_use]
pub fn tts_serving(config: &orator_core::OratorConfig) -> (String, String) {
    let model = orator_core::resolve_role_model(&config.roles, "tts")
        .unwrap_or_else(|| config.tts.voice.clone());
    let backend = config
        .models
        .models
        .get(&model)
        .and_then(|spec| spec.primary_backend())
        .map_or_else(|| config.tts.backend.clone(), str::to_string);
    (model, backend)
}

/// Signal stats for an empty decode: peak and RMS over the exact
/// buffer the STT backend heard. A silent mic reports zeros here
/// instead of a bare mystery — the operator sees capture failure,
/// not a model failure.
#[must_use]
pub fn decode_signal_stats(samples: &[f32]) -> (f32, f32) {
    let mut peak = 0.0f32;
    let mut sum = 0.0f64;
    for sample in samples {
        let abs = sample.abs();
        if abs > peak {
            peak = abs;
        }
        sum += f64::from(*sample) * f64::from(*sample);
    }
    let rms = if samples.is_empty() {
        0.0
    } else {
        (sum / samples.len() as f64).sqrt() as f32
    };
    (peak, rms)
}

/// Session state the prompt-playout decision reads: anything live
/// holds the reply for overlap, a fully idle session voices at once.
#[allow(clippy::struct_excessive_bools)] // five independent idle gates; an enum would hide which leg blocks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceNow {
    /// A turn is currently running.
    pub turn_running: bool,
    /// Input waits (stashed lines).
    pub queued: bool,
    /// The VAD gate is hot.
    pub mic_speaking: bool,
    /// Buffered speech awaits endpointing.
    pub utterance_buffered: bool,
    /// A playout worker is live.
    pub playout_live: bool,
}

/// Playout tail hold: how long capture stays suppressed after any
/// playout ends, so the room tail decays instead of endpointing as
/// a phantom turn. Covers reverb plus VAD hangover plus drain
/// granularity; the AEC opt-in bypasses suppression, so only
/// AEC-off turns pay this hold.
pub const ECHO_TAIL_HOLD_MS: u64 = 1000;

/// Whether capture stays suppressed for the playout tail: a hold
/// deadline in the future mutes endpointing (the VAD still runs, so
/// hot speech is seen but not committed until the room is quiet).
#[must_use]
pub fn tail_hold_active(quiet_until: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    quiet_until.is_some_and(|until| now < until)
}

/// Whether a completed short reply voices at once instead of
/// holding for overlap: the session is idle (nothing in `state` is
/// live). A held reply voices during the next turn; an idle session
/// voices immediately, so a single exchange talks back instead of
/// waiting for a next turn that may never come.
#[must_use]
pub fn should_voice_now(state: VoiceNow) -> bool {
    !state.turn_running
        && !state.queued
        && !state.mic_speaking
        && !state.utterance_buffered
        && !state.playout_live
}

/// One voice-barge decision, already resolved to plain terms: the
/// loop computes each arm per poll (liveness, leaf, gate, utterance
/// length vs the `turn.barge_in_ms` leaf) and the gate below stays a
/// pure conjunction — no timers, no I/O, hermetic.
#[allow(clippy::struct_excessive_bools)] // four independent gates; an enum would hide which leg blocks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BargeCutInput {
    /// A turn is currently running (only a live turn can be cut).
    pub turn_live: bool,
    /// The `chat.barge` leaf (off sheds mic input instead of cutting).
    pub barge_on: bool,
    /// The VAD gate closed over buffered speech (endpointed).
    pub gate_quiet: bool,
    /// Buffered speech sustained past the `turn.barge_in_ms` leaf.
    pub sustained_past_bound: bool,
}

/// Voice-barge cut: speech present (confidence, VAD endpoint) AND
/// sustained past the temporal bound (task-value, the same
/// `turn.barge_in_ms` leaf the turn driver reads — the loop's
/// endpoint model has no hops, so only the bound is shared, not the
/// machine). Echo control lives in the OS audio path, so the gate
/// trusts the VAD span: a brief blip is present but not sustained
/// and holds; only continuous speech cuts.
#[must_use]
pub fn should_cut_for_barge(input: &BargeCutInput) -> bool {
    input.turn_live && input.barge_on && input.gate_quiet && input.sustained_past_bound
}

/// Merge the cut turn's preserved words with the interrupting
/// utterance's fresh transcription: verbatim prepend, one space
/// between. Either side empty leaves the other standing alone
/// (never a leading or doubled space); nothing is reworded, so
/// the cut turn's words survive exactly as transcribed.
#[must_use]
pub fn merge_preserved_question(preserved: Option<&str>, transcribed: &str) -> String {
    let Some(kept) = preserved.map(str::trim).filter(|kept| !kept.is_empty()) else {
        return transcribed.to_string();
    };
    if transcribed.trim().is_empty() {
        return kept.to_string();
    }
    format!("{kept} {}", transcribed.trim())
}

/// Per-poll sustained tracker: continuity composed from the shared
/// turn driver — the loop's only temporal home (no chat-local
/// cumulative counter). Each poll votes hot only when the gate is
/// hot AND the buffer grew (buffered speech advances the run;
/// refused capture and pauses never do). The latch holds
/// a reached bound through the endpoint so the cut can fire on it,
/// and clears when the buffer empties (take/shed ends the span).
/// The driver owns the run and the `turn.barge_in_ms` bound; the
/// loop owns only the latch, which carries no audio.
pub struct BargeSustain {
    driver: orator_turn::TurnDriver,
    prev_len: usize,
    latched: bool,
}

impl BargeSustain {
    /// Tracker over the session config: the bound comes from the
    /// `turn.barge_in_ms` leaf via the driver. Epoch and lexicon
    /// are unused by the continuity path — the loop has no hops and
    /// no STT words at this stage, so only the bound is shared.
    #[must_use]
    pub fn new(config: &orator_core::OratorConfig) -> Self {
        Self {
            driver: orator_turn::TurnDriver::with_config(
                orator_core::Epoch(1),
                "en",
                &config.turn,
            ),
            prev_len: 0,
            latched: false,
        }
    }

    /// Feed one poll; returns whether sustained speech reached the
    /// bound. `frame_ms` is the nominal analysis quantum (the loop
    /// polls on roughly the 20 ms cadence).
    pub fn poll(
        &mut self,
        speaking: bool,
        utterance_len: usize,
        ptt_cold: bool,
        frame_ms: u64,
    ) -> bool {
        if utterance_len == 0 {
            self.latched = false;
        }
        let grew = utterance_len > self.prev_len;
        self.prev_len = utterance_len;
        let outcome = self
            .driver
            .push_frame(speaking && grew, ptt_cold, frame_ms);
        if outcome != orator_turn::BargeInOutcome::Ignored {
            self.latched = true;
        }
        self.latched
    }
}

/// One turn's inference path: the native interleaved session when it
/// was assured, else the daemon-first cascade. Pure so the dispatch
/// decision pins without hardware.
#[must_use]
pub fn resolve_turn_path(prefer_s2s: bool, s2s_assured: bool) -> TurnPath {
    if prefer_s2s && s2s_assured {
        TurnPath::Native
    } else {
        TurnPath::Cascade
    }
}

/// One turn's inference path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnPath {
    /// Native interleaved speech-to-speech session.
    Native,
    /// Daemon-first cascade (ask, then say).
    Cascade,
}

/// Map a lone control-character line to its TUI action through the
/// single chord table (`tui::map_key` — never a second table).
/// Returns `None` for ordinary lines. Line discipline means a chord
/// arrives as its control character followed by Enter; Ctrl+C aborts
/// through the signal watch and Ctrl+D ends input, so neither
/// reaches this path.
#[must_use]
pub fn parse_control_line(line: &str) -> Option<crate::tui::TuiAction> {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut chars = line.chars();
    match (chars.next(), chars.next()) {
        (Some(control), None) if control.is_control() => {
            let byte = control as u32;
            if !(0x01..=0x1Au32).contains(&byte) {
                return None;
            }
            let base = (byte as u8 | 0x60) as char;
            Some(crate::tui::map_key(KeyEvent::new(
                KeyCode::Char(base),
                KeyModifiers::CONTROL,
            )))
        }
        _ => None,
    }
}

/// Why a file turn sheds before any inference runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileGate {
    /// PTT latch off: names the shed sample count.
    PttOff {
        /// Shed samples.
        samples: usize,
    },
    /// No speech present: nothing worth transcribing.
    NoSpeech,
}

/// Gate one file turn before any inference: the PTT latch sheds
/// first (mode + intent), then speech presence sheds silence (never
/// spend a decode on non-speech). `Ok(())` commits the turn.
pub fn check_file_gate(
    mic_mode: crate::tui::MicMode,
    talking: bool,
    samples: &[f32],
) -> Result<(), FileGate> {
    use crate::tui::MicMode;
    if mic_mode == MicMode::PushToTalk && !talking {
        return Err(FileGate::PttOff {
            samples: samples.len(),
        });
    }
    if !orator_vad::gate::speech_present_16k(samples) {
        return Err(FileGate::NoSpeech);
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/chat.rs"]
mod tests;
