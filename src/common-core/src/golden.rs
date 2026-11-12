//! Golden comparison and per-stage timing for calibrated pipelines.
//!
//! [`compare_series`] is the tolerance gate every golden suite funnels
//! through: actual vs expected `Option<f64>` series compared elementwise at
//! an absolute `eps`, with a [`CompareReport`] that names each mismatch
//! (index, both sides, max drift) instead of failing on the first one.
//! Threshold *selection* stays in [`crate::calibration`]; this module
//! answers "does this output match the pinned fixture", not "where should
//! the threshold sit".
//!
//! [`StageTimer`] is the per-stage timer shorthand: hold it for a stage's
//! duration and it observes once into the shared [`LatencyHistogram`] on
//! drop. `Instrumented::with_metrics` in `fluent-wvr` stays the `WorkUnit`
//! path — this is the free-function path for stages that are not units
//! (kernel batch eval in tests, fixture regeneration scripts).

use std::fmt;
use std::time::Instant;

use crate::metrics::LatencyHistogram;

/// How one position mismatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// How one position mismatched.
pub enum MismatchKind {
    /// Both sides present but farther apart than `eps` (or either side
    /// non-finite).
    Value,
    /// One side `None` (Pine `na`) where the other has a value.
    Missing,
    /// Position exists on only one side (different lengths).
    Length,
}

/// One mismatched position.
#[derive(Debug, Clone, PartialEq)]
/// One mismatched position.
pub struct Mismatch {
    /// Element index.
    pub index: usize,
    /// Actual value (`None` when missing/non-finite-or-absent).
    pub actual: Option<f64>,
    /// Expected value (`None` when the fixture has no value here).
    pub expected: Option<f64>,
    /// Absolute difference when both sides are finite numbers.
    pub abs_diff: Option<f64>,
    /// How the position mismatched.
    pub kind: MismatchKind,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "position {}: actual {:?} vs expected {:?} ({:?})",
            self.index, self.actual, self.expected, self.kind
        )
    }
}

/// Outcome of [`compare_series`].
#[derive(Debug, Clone, PartialEq)]
/// Outcome of [`compare_series`].
pub struct CompareReport {
    /// `true` when every compared position matched within `eps`.
    pub matches: bool,
    /// Positions compared (the longer side's length).
    pub compared: usize,
    /// Every mismatch, in index order.
    pub mismatches: Vec<Mismatch>,
    /// Largest finite absolute difference observed (0.0 when none).
    pub max_abs_diff: f64,
}

/// Compare `actual` against a pinned `expected` series at absolute tolerance
/// `eps`.
///
/// `None` equals `None` (warmup/`na` agreement matters as much as values);
/// anything else unequal — value drift beyond `eps`, `None` vs `Some`,
/// length skew, or a non-finite actual — is one [`Mismatch`]. Empty series
/// match empty series.
#[must_use]
/// Compare `actual` against a pinned `expected` series at absolute tolerance
/// `eps`.
/// 
/// `None` equals `None` (warmup/`na` agreement matters as much as values);
/// anything else unequal — value drift beyond `eps`, `None` vs `Some`,
/// length skew, or a non-finite actual — is one [`Mismatch`]. Empty series
/// match empty series.
pub fn compare_series(actual: &[Option<f64>], expected: &[Option<f64>], eps: f64) -> CompareReport {
    let compared = actual.len().max(expected.len());
    let mut mismatches = Vec::new();
    let mut max_abs_diff = 0.0f64;
    for index in 0..compared {
        let a = actual.get(index).copied().flatten();
        let e = expected.get(index).copied().flatten();
        match (a, e) {
            (None, None) => {}
            (Some(a), Some(e)) if a.is_finite() && e.is_finite() && (a - e).abs() <= eps => {
                max_abs_diff = max_abs_diff.max((a - e).abs());
            }
            (Some(a), Some(e)) => {
                if a.is_finite() && e.is_finite() {
                    max_abs_diff = max_abs_diff.max((a - e).abs());
                }
                mismatches.push(Mismatch {
                    index,
                    actual: Some(a),
                    expected: Some(e),
                    abs_diff: if a.is_finite() && e.is_finite() {
                        Some((a - e).abs())
                    } else {
                        None
                    },
                    kind: MismatchKind::Value,
                });
            }
            _ => {
                let kind = if actual.len() != expected.len()
                    && index >= actual.len().min(expected.len())
                {
                    MismatchKind::Length
                } else {
                    MismatchKind::Missing
                };
                mismatches.push(Mismatch {
                    index,
                    actual: a,
                    expected: e,
                    abs_diff: None,
                    kind,
                });
            }
        }
    }
    CompareReport {
        matches: mismatches.is_empty(),
        compared,
        mismatches,
        max_abs_diff,
    }
}

/// RAII per-stage timer: observes its lifetime into `hist` exactly once, on
/// drop. Hold it for a stage's duration; never reuse it.
pub struct StageTimer<'h> {
    hist: &'h LatencyHistogram,
    start: Instant,
}

impl<'h> StageTimer<'h> {
    /// Start timing a stage into `hist`.
    #[must_use]
    pub fn start(hist: &'h LatencyHistogram) -> Self {
        Self {
            hist,
            start: Instant::now(),
        }
    }
}

impl Drop for StageTimer<'_> {
    fn drop(&mut self) {
        self.hist.observe_duration(self.start);
    }
}

/// Run `stage`, observe its duration into `hist`, return its value. The
/// closure shorthand for a one-shot stage; hold a [`StageTimer`] instead
/// when the stage spans awaits or early returns.
pub fn measure<T>(hist: &LatencyHistogram, stage: impl FnOnce() -> T) -> T {
    let _timer = StageTimer::start(hist);
    stage()
}