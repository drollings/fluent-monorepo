//! The turn-taking state machine: a pure function of (state, event).
//!
//! The machine owns no clock and performs no I/O. Drivers translate audio
//! and inference activity into events carrying durations; the machine
//! answers with the next state. Purity makes it deterministic under a
//! simulated clock and exhaustively testable: every (state, event) pair
//! either advances per the transition table or is absorbed.

use fluent_wvr::{
    Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit,
    impl_component,
};
use internment::ArcIntern;
use orator_core::TurnConfig;

/// Turn states. Session-level modes reuse the same machine with the
/// `Thinking`, `ToolWait`, and `Speaking` states disabled by configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnState {
    /// Session open, nobody speaking.
    Idle,
    /// User utterance in progress.
    UserSpeaking,
    /// Deciding whether the turn is finished.
    Endpointing,
    /// LLM request in flight, no audio yet.
    Thinking,
    /// Tool running, optional preamble spoken.
    ToolWait,
    /// Assistant audio is playing.
    Speaking,
    /// Barge-in confirmed; cutting over.
    Yielding,
}

/// Driver-reported events. Durations ride along so the machine never needs
/// a clock.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    /// Voice activity started.
    VadStart,
    /// Voice activity ended.
    VadEnd,
    /// Accumulated silence reached `ms` with a semantic hint attached.
    SilenceElapsed {
        /// Silence so far in milliseconds.
        ms: u16,
        /// Whether the stable text looks like a complete turn.
        looks_complete: bool,
    },
    /// The driver committed the turn (STT final + semantic hint agree).
    TurnCommitted,
    /// First TTS chunk is playing.
    LlmFirstChunk,
    /// A tool call started.
    ToolCallStarted,
    /// A tool result is ready.
    ToolResultReady,
    /// Playout finished naturally.
    PlayoutDone,
    /// Confirmed user interruption while speaking.
    BargeInDetected,
    /// Cutover finished; the new utterance owns the floor.
    AbortComplete,
}

/// The pure transition function. Total: every pair yields a state, either
/// by advancing or by absorbing the event.
#[must_use]
pub const fn transition(
    state: TurnState,
    event: &TurnEvent,
    config: &TurnConfig,
) -> TurnState {
    match (state, event) {
        // Voice activity opens the floor from any listening state, and a
        // finished cutover hands the new utterance the floor.
        (
            TurnState::Idle
            | TurnState::Endpointing
            | TurnState::Thinking
            | TurnState::ToolWait,
            TurnEvent::VadStart,
        )
        | (TurnState::Yielding, TurnEvent::AbortComplete) => TurnState::UserSpeaking,
        (
            TurnState::UserSpeaking,
            TurnEvent::SilenceElapsed { ms, looks_complete },
        ) => {
            if *ms >= effective_hold_ms(config, *looks_complete) {
                TurnState::Endpointing
            } else {
                TurnState::UserSpeaking
            }
        }
        // A committed turn resumes thinking, wherever it waited.
        (
            TurnState::Endpointing,
            TurnEvent::TurnCommitted,
        )
        | (
            TurnState::ToolWait,
            TurnEvent::ToolResultReady,
        ) => TurnState::Thinking,
        (TurnState::Thinking, TurnEvent::LlmFirstChunk) => TurnState::Speaking,
        (TurnState::Thinking, TurnEvent::ToolCallStarted) => TurnState::ToolWait,
        (TurnState::Speaking, TurnEvent::PlayoutDone) => TurnState::Idle,
        (TurnState::Speaking, TurnEvent::BargeInDetected) => TurnState::Yielding,
        (state, _) => state,
    }
}

/// Effective hold for one silence report: the configured floor when the
/// text looks complete, the ceiling otherwise. Both bounds stay inside the
/// calibrated band; the machine never invents a third value.
#[must_use]
pub const fn effective_hold_ms(config: &TurnConfig, looks_complete: bool) -> u16 {
    if looks_complete {
        config.hold_min_ms
    } else {
        config.hold_max_ms
    }
}

/// The stateful machine: current state plus tunables. Wraps [`transition`]
/// so drivers step events without tracking states themselves. Illegal
/// events drop with a counter, never an error.
#[derive(Debug, Clone)]
pub struct TurnMachine {
    state: TurnState,
    config: TurnConfig,
    absorbed: u64,
    illegal: u64,
}

impl TurnMachine {
    /// Start in `Idle` with the given tunables.
    #[must_use]
    pub fn new(config: TurnConfig) -> Self {
        Self {
            state: TurnState::Idle,
            config,
            absorbed: 0,
            illegal: 0,
        }
    }

    /// Current state.
    #[must_use]
    pub const fn state(&self) -> TurnState {
        self.state
    }

    /// Dropped (absorbed + illegal) events so far. Kept as the total for
    /// existing callers; prefer [`absorbed_drops`](Self::absorbed_drops)
    /// and [`illegal_drops`](Self::illegal_drops) for the split.
    #[must_use]
    pub const fn drops(&self) -> u64 {
        self.absorbed.saturating_add(self.illegal)
    }

    /// Absorbed-by-design events so far (`VadEnd` in any state: VAD end
    /// with no speech to endpoint is normal, never a violation).
    #[must_use]
    pub const fn absorbed_drops(&self) -> u64 {
        self.absorbed
    }

    /// Illegal events so far (same-state absorbs outside the legality
    /// table, excluding absorbed-by-design).
    #[must_use]
    pub const fn illegal_drops(&self) -> u64 {
        self.illegal
    }

    /// Whether `(state, event)` is absorbed by design rather than illegal.
    /// `VadEnd` in any state is normal telemetry (the floor never held
    /// speech), and `VadStart` while `Speaking` is barge-in evidence the
    /// driver confirms separately — both absorb without counting as a
    /// violation.
    #[must_use]
    pub const fn is_absorbed(state: TurnState, event: &TurnEvent) -> bool {
        matches!(event, TurnEvent::VadEnd)
            || matches!(
                (state, event),
                (TurnState::Speaking, TurnEvent::VadStart)
            )
    }

    /// Whether `(state, event)` is a legal policy step. Silence hold
    /// reports that stay `UserSpeaking` count as legal (endpointing
    /// timing, not a violation); every other same-state absorb is a
    /// drop.
    #[must_use]
    pub fn is_legal(state: TurnState, event: &TurnEvent) -> bool {
        matches!(
            (state, event),
            (
                TurnState::Idle | TurnState::Endpointing | TurnState::Thinking | TurnState::ToolWait,
                TurnEvent::VadStart,
            ) | (TurnState::Yielding, TurnEvent::AbortComplete)
                | (
                    TurnState::UserSpeaking,
                    TurnEvent::SilenceElapsed { .. },
                )
                | (TurnState::Endpointing, TurnEvent::TurnCommitted)
                | (TurnState::ToolWait, TurnEvent::ToolResultReady)
                | (
                    TurnState::Thinking,
                    TurnEvent::LlmFirstChunk | TurnEvent::ToolCallStarted,
                )
                | (
                    TurnState::Speaking,
                    TurnEvent::PlayoutDone | TurnEvent::BargeInDetected,
                )
        )
    }

    /// Step one event, returning the new state.
    pub fn step(&mut self, event: &TurnEvent) -> TurnState {
        if !Self::is_legal(self.state, event) {
            if Self::is_absorbed(self.state, event) {
                self.absorbed = self.absorbed.saturating_add(1);
            } else {
                self.illegal = self.illegal.saturating_add(1);
            }
        }
        self.state = transition(self.state, event, &self.config);
        self.state
    }
}

impl WorkUnit for TurnMachine {
    fn name(&self) -> &str {
        "turn"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok(format!("state={:?}", self.state)))
    }
}

impl FieldAccess for TurnMachine {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        self.config.set_field(name, value)
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        self.config.get_field(name)
    }
    fn field_names(&self) -> &'static [&'static str] {
        self.config.field_names()
    }
}

impl Describable for TurnMachine {
    fn describe(&self) -> serde_json::Value {
        self.config.describe()
    }
}

impl_component!(TurnMachine);

#[cfg(test)]
#[path = "../tests/machine.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/hold_calibration.rs"]
mod calibration_tests;
