//! Trigger and classifier registry for the bytecode track.
//!
//! Two kinds of gate decide things in this engine, and they must never be
//! confused:
//!
//! * **Confidence** gates choose a *representation*. A miss, a veto, or a
//!   deopt only changes which code path serves the same values, so a
//!   confidence gate can never make an outcome wrong — at worst it makes it
//!   slower.
//! * **Correctness** gates choose *whether an engine may run at all*. They
//!   are static (compile/link classification) or structural bounds, their
//!   corpora are must-fire/must-NOT-fire, and none of them consults a
//!   measurement.
//!
//! A new trigger must be classified in [`TRIGGERS`]; the calibration suite
//! asserts the table and the pinned constants agree, so an unclassified gate
//! fails the build's tests instead of drifting into the wrong category.

/// What one trigger or classifier is allowed to decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// Chooses a representation; never decides correctness.
    Confidence,
    /// Decides whether an engine may run; never measured, never tuned.
    Correctness,
}

/// Every trigger and classifier in the track, with its kind and the constant
/// or entry point that carries it.
pub const TRIGGERS: &[(&str, TriggerKind)] = &[
    ("tier_stable_bars", TriggerKind::Confidence),
    ("split_brain_coverage", TriggerKind::Correctness),
    ("max_call_depth", TriggerKind::Correctness),
    ("max_loop_iterations", TriggerKind::Correctness),
    ("headless_effect", TriggerKind::Correctness),
];

/// The kind of one named trigger, when it is registered.
#[must_use]
pub fn trigger_kind(name: &str) -> Option<TriggerKind> {
    TRIGGERS
        .iter()
        .find(|(registered, _)| *registered == name)
        .map(|(_, kind)| *kind)
}
