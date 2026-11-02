#![forbid(unsafe_code)]

//! orator-turn: the turn-taking state machine: endpointing, barge-in, and backchannel filtering.

pub mod driver;
pub mod machine;

pub use driver::{
    degrade_on, lexicon, BackchannelFilter, BargeInEvidence, BargeInOutcome, History, HistoryTurn,
    Hop, HopGate, SessionKind, SessionMode, SpecState, TurnDriver, BACKCHANNEL_MS, BARGE_IN_MS,
    CONFIRM_MS, CUT_FADE_MS, DUCK_DB, DUCK_RAMP_MS,
};
pub use machine::{TurnEvent, TurnMachine, TurnState};
