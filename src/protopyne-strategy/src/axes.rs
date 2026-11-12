//! The two measurement axes, kept apart by the type system.
//!
//! Every trigger, threshold, and gate in this platform answers one of two
//! different questions, and the answers are not interchangeable:
//!
//! - **Producer confidence** — *is the thing that spoke healthy enough to be
//!   believed?* A producer's own doubt about its reading. It may silence the
//!   producer (disarm entries, drop a stale label, withhold a cache
//!   revision). It must never deny a trade on merit, size a position, or
//!   approve one.
//! - **Task-value** — *does this outcome serve the operator's stated intent?*
//!   Merit of the trade and the fit of the order. It selects, sizes, and
//!   ranks. It must never claim anything about a producer's health.
//!
//! A third axis rides beside them as a hard bound rather than a judgement:
//! **correctness** — *must this never be violated?* Risk caps, staleness and
//! clock limits, reconciliation, authority. Configuration may tighten a
//! correctness bound and can never widen it.
//!
//! The conflation this module exists to prevent is specific and already in the
//! tree: a signal's [`conviction`](crate::elements::SignalOutput::conviction)
//! is its *confidence* in its own read, and the grant's `min_conviction` is a
//! *task-value* policy. A confident producer can be wrong for the task and a
//! hesitant one can be right, so the two are separate types here: mixing them
//! is a compile error, not a review note.

use crate::error::StrategyError;

/// Which axis a field, gate, or threshold measures.
///
/// Carried in a gate's schema as `x-axis` so a consumer — a terminal badge, an
/// audit, a test — can tell a confidence reading from a trade decision without
/// knowing the field by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Which axis a field, gate, or threshold measures.
/// 
/// Carried in a gate's schema as `x-axis` so a consumer — a terminal badge, an
/// audit, a test — can tell a confidence reading from a trade decision without
/// knowing the field by name.
pub enum GateAxis {
    /// A hard bound that must never be violated and may only be tightened.
    Correctness,
    /// Merit of the outcome: selects, sizes, ranks.
    TaskValue,
    /// The producer's own doubt; may silence a producer, never judge a trade.
    ProducerConfidence,
}

impl GateAxis {
    /// The schema spelling (the closed vocabulary `#[field(axis = "…")]`
    /// accepts).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Correctness => "correctness",
            Self::TaskValue => "task-value",
            Self::ProducerConfidence => "producer-confidence",
        }
    }

    /// Parse the schema spelling; `None` for an unlisted word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "correctness" => Some(Self::Correctness),
            "task-value" => Some(Self::TaskValue),
            "producer-confidence" => Some(Self::ProducerConfidence),
            _ => None,
        }
    }
}

impl std::fmt::Display for GateAxis {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A producer's confidence in its own reading, bounded to `[0, 1]`.
///
/// Constructors are the only ways in, and neither one starts from another
/// axis: there is deliberately no `From<f64>` (a bare number has no axis) and
/// no conversion from [`Merit`]. A confidence value is a *producer's*
/// statement; code that needs a trade judgement must ask for [`Merit`].
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
/// A producer's confidence in its own reading, bounded to `[0, 1]`.
/// 
/// Constructors are the only ways in, and neither one starts from another
/// axis: there is deliberately no `From<f64>` (a bare number has no axis) and
/// no conversion from [`Merit`]. A confidence value is a *producer's*
/// statement; code that needs a trade judgement must ask for [`Merit`].
pub struct Confidence(f64);

impl Confidence {
    /// Saturating construction for a producer score.
    ///
    /// Saturation is deliberate and stated: a score is bounded by definition,
    /// so a producer reporting `1.2` reads as full confidence rather than
    /// failing the order path over its own scale error. A non-finite score
    /// reads as no confidence (`0.0`) — the same guard the volume and
    /// volatility readings use for unmeasurable input. Nothing is hidden: the
    /// bound is this type's contract.
    #[must_use]
    pub fn saturating(score: f64) -> Self {
        if !score.is_finite() {
            return Self(0.0);
        }
        Self(score.clamp(0.0, 1.0))
    }

    /// Strict construction: refuses a non-finite or out-of-band score instead
    /// of saturating. Callers that must not proceed on a malformed producer
    /// reading use this one.
    pub fn try_new(score: f64) -> Result<Self, StrategyError> {
        if !score.is_finite() || !(0.0..=1.0).contains(&score) {
            return Err(StrategyError::InvalidElement(format!(
                "confidence {score} sits outside [0, 1]"
            )));
        }
        Ok(Self(score))
    }

    /// The bounded score.
    #[must_use]
    pub const fn value(self) -> f64 {
        self.0
    }
}

/// A task-value reading: the merit of the outcome, in the axes a policy may
/// threshold.
///
/// `activity` is the observed opportunity (options participation today);
/// `edge` is an estimated per-unit edge when a signal measures one. Absence is
/// not zero: `edge: None` means "not measured", which a policy that needs it
/// must refuse rather than read as no edge.
#[derive(Debug, Clone, Copy, PartialEq)]
/// A task-value reading: the merit of the outcome, in the axes a policy may
/// threshold.
/// 
/// `activity` is the observed opportunity (options participation today);
/// `edge` is an estimated per-unit edge when a signal measures one. Absence is
/// not zero: `edge: None` means "not measured", which a policy that needs it
/// must refuse rather than read as no edge.
pub struct Merit {
    /// Bounded activity/opportunity score in `[0, 1]`.
    pub activity: f64,
    /// Estimated per-unit edge, when a signal measured one.
    pub edge: Option<f64>,
}

impl Merit {
    /// Build a merit reading fail-closed: a non-finite or out-of-band activity
    /// score refuses, because this value feeds task-value thresholds whose
    /// verdicts the operator acts on. An unmeasured edge stays `None`.
    pub fn new(activity: f64, edge: Option<f64>) -> Result<Self, StrategyError> {
        if !activity.is_finite() || !(0.0..=1.0).contains(&activity) {
            return Err(StrategyError::InvalidElement(format!(
                "activity score {activity} sits outside [0, 1]"
            )));
        }
        if edge.is_some_and(|value| !value.is_finite()) {
            return Err(StrategyError::InvalidElement(
                "edge must be finite when present".to_owned(),
            ));
        }
        Ok(Self { activity, edge })
    }
}

#[cfg(test)]
#[path = "../tests/axes.rs"]
mod tests;

/// Heuristics whose control/treatment calibration is frozen in-tree.
///
/// A heuristic id lands here only with a frozen corpus (treatment
/// must-fire, controls must-not-fire) and measured precision/recall. Cache,
/// persist, and sizing paths must consult [`require_calibrated`] before
/// trusting a heuristic id: an unlisted id refuses instead of silently
/// running uncalibrated.
#[must_use]
/// Heuristics whose control/treatment calibration is frozen in-tree.
/// 
/// A heuristic id lands here only with a frozen corpus (treatment
/// must-fire, controls must-not-fire) and measured precision/recall. Cache,
/// persist, and sizing paths must consult [`require_calibrated`] before
/// trusting a heuristic id: an unlisted id refuses instead of silently
/// running uncalibrated.
pub fn calibrated_heuristic_ids() -> &'static [&'static str] {
    &["trail_on_exhaustion"]
}

/// Refuse an uncalibrated heuristic id before it can back caching,
/// persistence, or sizing.
pub fn require_calibrated(id: &str) -> Result<(), String> {
    if calibrated_heuristic_ids().contains(&id) {
        Ok(())
    } else {
        Err(format!(
            "heuristic {id:?} has no frozen calibration; calibrate it before trusting it"
        ))
    }
}