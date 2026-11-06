//! Turn driver: epoch discipline, barge-in with duck/confirm/cut, DAC
//! history, speculative prefill, and per-stage degrade policy.
//!
//! The driver owns no audio and performs no inference. It turns driver
//! observations (VAD sustain, STT words, stage failures) into epoch
//! decisions the pipeline hops check: post-STT, pre-LLM, pre-speak. A
//! check against a fired or superseded epoch drops the item; the abort
//! is sticky, so late arrivals can never resurrect a cut turn.

use std::collections::HashMap;

use fluent_concurrency::stream::StreamAbort;
use orator_core::{Epoch, Word};

/// Barge-in detect: mic VAD must sustain this long while speaking.
pub const BARGE_IN_MS: u16 = 150;
/// Confirm waits this long for STT evidence before restoring gain.
pub const CONFIRM_MS: u16 = 250;
/// Utterances below this with only lexicon words are backchannels.
pub const BACKCHANNEL_MS: u64 = 600;
/// Playout fade on cut.
/// Canonical home: `common_core::envelope` (same calibrated value, shared).
pub use common_core::envelope::CUT_FADE_MS;
/// Playout duck depth on detect.
/// Canonical home: `common_core::envelope::DUCK_DEPTH_DB` — aliased
/// because the driver historically names it `DUCK_DB`.
pub use common_core::envelope::DUCK_DEPTH_DB as DUCK_DB;
/// Duck ramp length.
/// Canonical home: `common_core::envelope` (same calibrated value, shared).
pub use common_core::envelope::DUCK_RAMP_MS;

/// Pipeline hops that check the epoch. Every hop drops items from a
/// fired or superseded epoch; drops count per hop for metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hop {
    /// After STT commit, before the turn commits.
    PostStt,
    /// Before the LLM request submits.
    PreLlm,
    /// Before a TTS chunk plays.
    PreSpeak,
}

/// The epoch gate: one live epoch plus its abort. Bumping the epoch
/// retires the old abort (left fired) and opens a fresh one.
pub struct HopGate {
    live: Epoch,
    abort: StreamAbort,
    drops: HashMap<Hop, u64>,
}

impl HopGate {
    /// Gate opening on `epoch` with a live abort.
    #[must_use]
    pub fn new(epoch: Epoch) -> Self {
        Self {
            live: epoch,
            abort: StreamAbort::new(),
            drops: HashMap::new(),
        }
    }

    /// Whether `epoch` may pass `hop`. Records every drop per hop.
    pub fn pass(&mut self, epoch: Epoch, hop: Hop) -> bool {
        if epoch == self.live && !self.abort.is_cancelled() {
            true
        } else {
            *self.drops.entry(hop).or_insert(0) += 1;
            false
        }
    }

    /// Cut: fire the live abort (sticky) and open the next epoch with a
    /// fresh abort. Returns the new live epoch.
    pub fn cut(&mut self) -> Epoch {
        self.abort.cancel();
        self.live = Epoch(self.live.0 + 1);
        self.abort = StreamAbort::new();
        self.live
    }

    /// The live epoch.
    #[must_use]
    pub const fn live(&self) -> Epoch {
        self.live
    }

    /// The live epoch's abort handle for pipeline tasks.
    #[must_use]
    pub fn abort(&self) -> StreamAbort {
        self.abort.clone()
    }

    /// Drops recorded at `hop`.
    #[must_use]
    pub fn drops(&self, hop: Hop) -> u64 {
        self.drops.get(&hop).copied().unwrap_or(0)
    }
}

/// What the microphone brought while the assistant spoke.
#[derive(Debug, Clone, PartialEq)]
pub struct BargeInEvidence {
    /// Sustained VAD in ms.
    pub sustained_ms: u16,
    /// Utterance audio length in ms (for the backchannel rule).
    pub audio_ms: u64,
    /// STT words so far (empty while confirming).
    pub words: Vec<String>,
    /// Speaker-verification match when configured; `None` means no
    /// verification is enrolled and the speech counts as the user.
    pub speaker_match: Option<bool>,
}

/// Barge-in decision after detect or confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BargeInOutcome {
    /// Below threshold or filtered: keep speaking, no gain change.
    Ignored,
    /// Detected but unconfirmed: duck the playout, await STT evidence.
    Ducking,
    /// Confirmed with words: cut now.
    Cut,
}

/// Backchannel filter: short lexicon-only utterances never interrupt and
/// never reach the LLM. The lexicon is data (`lang` + `extra`), never a
/// hard-coded list at the call site.
#[derive(Debug, Clone)]
pub struct BackchannelFilter {
    lang: String,
    extra: Vec<String>,
}

impl BackchannelFilter {
    /// Filter for `lang` (`en`, `de`) with no custom entries.
    #[must_use]
    pub fn new(lang: &str) -> Self {
        Self {
            lang: lang.to_string(),
            extra: Vec::new(),
        }
    }

    /// Whether `words` spoken over `audio_ms` is a backchannel: short and
    /// every word in the lexicon.
    #[must_use]
    pub fn is_backchannel(&self, words: &[String], audio_ms: u64) -> bool {
        if audio_ms >= BACKCHANNEL_MS || words.is_empty() {
            return false;
        }
        words.iter().all(|word| self.in_lexicon(word))
    }

    fn in_lexicon(&self, word: &str) -> bool {
        let clean = word
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        lexicon(self.lang.as_str()).contains(&clean.as_str())
            || self.extra.iter().any(|entry| entry == &clean)
    }
}

/// Per-language backchannel lexicon.
#[must_use]
pub fn lexicon(lang: &str) -> &'static [&'static str] {
    match lang {
        "de" => &[
            "ja", "mhm", "mmhm", "aha", "oh", "ah", "hm", "genau", "richtig", "okay", "ok",
            "stimmt",
        ],
        _ => &[
            "yeah", "yep", "yup", "mhm", "mmhm", "mm-hm", "uh-huh", "uhhuh", "right", "okay", "ok",
            "sure", "wow", "oh", "ah", "hmm", "hm",
        ],
    }
}

/// One conversation turn as the user heard it: DAC-confirmed words only,
/// plus whether barge-in cut it short.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryTurn {
    /// Reply epoch.
    pub epoch: Epoch,
    /// Words whose speech marks reached the DAC.
    pub words: Vec<Word>,
    /// Whether the user cut the reply short.
    pub interrupted: bool,
}

/// DAC history: only spoken words persist, followed by an interruption
/// marker the model can see. The unspoken remainder is dropped here —
/// never summarized, never stored.
#[derive(Debug, Clone, Default)]
pub struct History {
    turns: Vec<HistoryTurn>,
}

impl History {
    /// Empty history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Commit one reply as heard.
    pub fn push_reply(&mut self, epoch: Epoch, words: Vec<Word>, interrupted: bool) {
        self.turns.push(HistoryTurn {
            epoch,
            words,
            interrupted,
        });
    }

    /// The durable reply text: spoken words, then the interruption marker
    /// when the user cut in.
    #[must_use]
    pub fn reply_final(&self) -> String {
        let mut out = String::new();
        for turn in &self.turns {
            for word in &turn.words {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&word.text);
            }
            if turn.interrupted {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str("[interrupted by user]");
            }
        }
        out
    }

    /// Committed turns.
    #[must_use]
    pub fn turns(&self) -> &[HistoryTurn] {
        &self.turns
    }
}

/// Speculative prefill state: armed at the short threshold on stable text,
/// discarded when the user resumes. The resume returns the armed prefix
/// so the next request re-sends exactly it — the server reuses its cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SpecState {
    /// Nothing speculated.
    #[default]
    Idle,
    /// Prefill in flight on this prefix.
    Speculating {
        /// Stable text sent speculatively.
        prefix: String,
    },
}

impl SpecState {
    /// Whether a speculative request is in flight.
    #[must_use]
    pub const fn is_speculating(&self) -> bool {
        matches!(self, Self::Speculating { .. })
    }

    /// Arm a speculative prefill on the stable text.
    pub fn arm(&mut self, prefix: String) {
        *self = Self::Speculating { prefix };
    }

    /// The user resumed: drop the speculation and hand back the armed
    /// prefix for the re-request. `None` when nothing was armed.
    pub fn on_resume(&mut self) -> Option<String> {
        match std::mem::replace(self, Self::Idle) {
            Self::Speculating { prefix } => Some(prefix),
            Self::Idle => None,
        }
    }
}

/// Session kinds for the degrade table: re-exported from the core
/// vocabulary (single home).
pub use orator_core::types::SessionKind;

/// Degraded serving shape after a stage failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// Everything serves.
    Full,
    /// Assistant without replies: dictation plus an earcon.
    DictationOnly,
    /// Replies as text only.
    TextOnly,
    /// Keep recording, mark the gap, re-transcribe offline later.
    RecordOnly,
    /// Session pauses (device lost, disk full).
    Paused,
}

/// Stage failure → serving shape, per the failure table. Archive failure
/// is fatal and loud; the record path never degrades silently. Degraded
/// (non-fatal) rows that stay `Full` — `aec` (half-duplex gate), `vad`
/// (energy detector), background stages — fall through to the default.
#[must_use]
pub fn degrade_on(failed: &str, session: SessionKind) -> SessionMode {
    match failed {
        "archive" | "capture.mic" => SessionMode::Paused,
        "capture.sys" => SessionMode::RecordOnly,
        "stt" => match session {
            SessionKind::Dictation => SessionMode::Paused,
            _ => SessionMode::RecordOnly,
        },
        "llm" => match session {
            SessionKind::Assistant => SessionMode::DictationOnly,
            _ => SessionMode::Full,
        },
        "tts" => match session {
            SessionKind::Assistant => SessionMode::TextOnly,
            _ => SessionMode::Full,
        },
        _ => SessionMode::Full,
    }
}

/// The turn driver: machine state plus epoch discipline, barge-in
/// detect/confirm, DAC history, and speculation.
pub struct TurnDriver {
    gate: HopGate,
    history: History,
    spec: SpecState,
    backchannel: BackchannelFilter,
    barge_in_ms: u16,
}

impl TurnDriver {
    /// Driver opening on `epoch` with the default English filter.
    #[must_use]
    pub fn new(epoch: Epoch) -> Self {
        Self::with_lang(epoch, "en")
    }

    /// Driver opening on `epoch` with the `lang` backchannel lexicon.
    #[must_use]
    pub fn with_lang(epoch: Epoch, lang: &str) -> Self {
        Self {
            gate: HopGate::new(epoch),
            history: History::new(),
            spec: SpecState::Idle,
            backchannel: BackchannelFilter::new(lang),
            barge_in_ms: BARGE_IN_MS,
        }
    }

    /// Driver with the barge-in threshold taken from `TurnConfig`.
    /// The config leaf is the single tunable; [`BARGE_IN_MS`] stays the
    /// default it carries.
    #[must_use]
    pub fn with_config(epoch: Epoch, lang: &str, config: &orator_core::TurnConfig) -> Self {
        Self {
            gate: HopGate::new(epoch),
            history: History::new(),
            spec: SpecState::Idle,
            backchannel: BackchannelFilter::new(lang),
            barge_in_ms: config.barge_in_ms,
        }
    }

    /// Override the barge-in threshold (tests and hot-edit paths).
    pub fn set_barge_in_ms(&mut self, ms: u16) {
        self.barge_in_ms = ms;
    }

    /// Current barge-in threshold in ms.
    #[must_use]
    pub const fn barge_in_ms(&self) -> u16 {
        self.barge_in_ms
    }

    /// Check `epoch` at `hop`. Late or fired arrivals drop.
    pub fn check(&mut self, epoch: Epoch, hop: Hop) -> bool {
        self.gate.pass(epoch, hop)
    }

    /// VAD sustained `ms` while speaking: duck past the threshold, else
    /// ignore. Ducking is the detect half; confirmation needs words.
    #[must_use]
    pub const fn detect(&self, ms: u16) -> BargeInOutcome {
        if ms >= self.barge_in_ms {
            BargeInOutcome::Ducking
        } else {
            BargeInOutcome::Ignored
        }
    }

    /// Confirm with STT evidence: backchannels, empty transcripts, and
    /// verification mismatches restore gain; sustained speech with words
    /// cuts and opens the next epoch.
    pub fn confirm(&mut self, evidence: &BargeInEvidence) -> BargeInOutcome {
        if evidence.sustained_ms < self.barge_in_ms {
            return BargeInOutcome::Ignored;
        }
        if evidence.words.is_empty() {
            return BargeInOutcome::Ignored;
        }
        if evidence.speaker_match == Some(false) {
            return BargeInOutcome::Ignored;
        }
        if self
            .backchannel
            .is_backchannel(&evidence.words, evidence.audio_ms)
        {
            return BargeInOutcome::Ignored;
        }
        self.gate.cut();
        self.spec = SpecState::Idle;
        BargeInOutcome::Cut
    }

    /// Live epoch and its abort for newly spawned pipeline tasks.
    #[must_use]
    pub const fn live(&self) -> Epoch {
        self.gate.live()
    }

    /// Abort handle for the live epoch.
    #[must_use]
    pub fn abort(&self) -> StreamAbort {
        self.gate.abort()
    }

    /// Drops recorded at `hop`.
    #[must_use]
    pub fn drops(&self, hop: Hop) -> u64 {
        self.gate.drops(hop)
    }

    /// Commit one reply as the user heard it.
    pub fn commit_reply(&mut self, words: Vec<Word>, interrupted: bool) {
        let epoch = self.gate.live();
        self.history.push_reply(epoch, words, interrupted);
    }

    /// DAC history.
    #[must_use]
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Arm speculation on the stable text at the short threshold.
    pub fn speculate(&mut self, prefix: String) {
        self.spec.arm(prefix);
    }

    /// The user resumed: drop the speculation, return the prefix to
    /// re-request. Also trips the in-flight request via epoch cut when
    /// armed, so the speculative tokens can never land.
    pub fn on_resume(&mut self) -> Option<String> {
        let prefix = self.spec.on_resume()?;
        self.gate.cut();
        Some(prefix)
    }
}

#[cfg(test)]
#[path = "../tests/driver.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/barge_in_calibration.rs"]
mod calibration_tests;
