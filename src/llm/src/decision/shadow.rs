//! Shadow guidance consumers (logs only, never alters).
//!
//! `ShadowLog` accumulates `ShadowOutcome`s (label, distribution summary,
//! latency, would-have-acted) alongside the authoritative path.
//! `shadow_adapter` composes `ComponentAdapter::with_execute_override`: it
//! runs the inner component, records the outcome, and returns the inner
//! output byte-identical. `instrumented_shadow` adds
//! `Instrumented::with_metrics` timing — no second timing path.

use std::sync::{Arc, Mutex};

use common_core::sync::lock;
use fluent_wvr::wrapper::{ComponentAdapter, ExecuteFn, Instrumented};
use fluent_wvr::{Component, WorkContext, WorkOutput};

use crate::decision::{decide_verdict, Decision, ThresholdSet, Verdict};

/// What the shadow decision would have done, had it been authoritative.
#[derive(Debug, Clone, PartialEq)]
/// What the shadow decision would have done, had it been authoritative.
pub struct ShadowOutcome {
    pub label: String,
    pub confidence: f64,
    pub margin: f64,
    pub latency_ms: u64,
    pub cached: bool,
    pub verdict: Verdict,
    pub would_have_acted: bool,
}

impl ShadowOutcome {
    /// Record a decision at its own thresholds. `would_have_acted` is true
    /// only for `Act` — `Review`/`Escalate` need a human or a rule.
    ///
    /// ```rust
    /// use fluent_llm::decision::{Decision, ShadowOutcome, ThresholdSet};
    ///
    /// let d = Decision::from_distribution(
    ///     "B",
    ///     vec![("B".to_string(), 0.97), ("A".to_string(), 0.03)],
    ///     "m".to_string(),
    ///     1,
    ///     0,
    ///     false,
    /// )
    /// .expect("decision");
    /// let outcome = ShadowOutcome::record(&d, &ThresholdSet { act: 0.9, review: 0.7 }, true);
    /// assert!(outcome.would_have_acted);
    /// ```
    #[must_use]
    pub fn record(decision: &Decision, thresholds: &ThresholdSet, acted: bool) -> Self {
        let verdict = decide_verdict(decision, thresholds);
        Self {
            label: decision.label.clone(),
            confidence: decision.confidence,
            margin: decision.margin,
            latency_ms: decision.latency_ms,
            cached: decision.cached,
            verdict,
            would_have_acted: acted && verdict == Verdict::Act,
        }
    }
}

/// Shared append-only shadow log.
///
/// ```rust
/// use fluent_llm::decision::{Decision, ShadowLog, ShadowOutcome, ThresholdSet};
///
/// let log = ShadowLog::new();
/// assert!(log.is_empty());
/// ```
#[derive(Default)]
/// Shared append-only shadow log.
/// 
/// ```rust
/// use fluent_llm::decision::{Decision, ShadowLog, ShadowOutcome, ThresholdSet};
/// 
/// let log = ShadowLog::new();
/// assert!(log.is_empty());
/// ```
pub struct ShadowLog {
    outcomes: Mutex<Vec<ShadowOutcome>>,
}

impl ShadowLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one outcome.
    pub fn record(&self, outcome: ShadowOutcome) {
        lock(&self.outcomes).push(outcome);
    }

    /// Snapshot of recorded outcomes, in order.
    #[must_use]
    pub fn outcomes(&self) -> Vec<ShadowOutcome> {
        lock(&self.outcomes).clone()
    }

    /// Number of recorded outcomes.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.outcomes).len()
    }

    /// `true` when nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Wrap `inner` so `execute` runs it, records `outcome`, and returns the
/// inner output unchanged.
///
/// ```rust
/// use std::sync::Arc;
/// use fluent_llm::decision::{Decision, ShadowLog, ShadowOutcome, ThresholdSet, shadow_adapter};
/// use fluent_llm::testutil::StubBackend;
/// use fluent_wvr::{Component, WorkContext, WorkUnit};
///
/// let inner: Arc<dyn Component> = Arc::new(StubBackend::new("stub", vec!["m"], "hi"));
/// let d = Decision::from_distribution(
///     "B",
///     vec![("B".to_string(), 0.97), ("A".to_string(), 0.03)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("decision");
/// let outcome = ShadowOutcome::record(&d, &ThresholdSet { act: 0.9, review: 0.7 }, true);
/// let shadowed = shadow_adapter(inner, Arc::new(ShadowLog::new()), outcome);
/// assert!(shadowed.execute(&WorkContext::default()).is_ok());
/// ```
#[must_use]
/// Wrap `inner` so `execute` runs it, records `outcome`, and returns the
/// inner output unchanged.
/// 
/// ```rust
/// use std::sync::Arc;
/// use fluent_llm::decision::{Decision, ShadowLog, ShadowOutcome, ThresholdSet, shadow_adapter};
/// use fluent_llm::testutil::StubBackend;
/// use fluent_wvr::{Component, WorkContext, WorkUnit};
/// 
/// let inner: Arc<dyn Component> = Arc::new(StubBackend::new("stub", vec!["m"], "hi"));
/// let d = Decision::from_distribution(
/// "B",
/// vec![("B".to_string(), 0.97), ("A".to_string(), 0.03)],
/// "m".to_string(),
/// 1,
/// 0,
/// false,
/// )
/// .expect("decision");
/// let outcome = ShadowOutcome::record(&d, &ThresholdSet { act: 0.9, review: 0.7 }, true);
/// let shadowed = shadow_adapter(inner, Arc::new(ShadowLog::new()), outcome);
/// assert!(shadowed.execute(&WorkContext::default()).is_ok());
/// ```
pub fn shadow_adapter(
    inner: Arc<dyn Component>,
    log: Arc<ShadowLog>,
    outcome: ShadowOutcome,
) -> ComponentAdapter {
    let run_inner = Arc::clone(&inner);
    let run: ExecuteFn = Arc::new(move |ctx: &WorkContext| {
        let output: WorkOutput = run_inner.execute(ctx)?;
        log.record(outcome.clone());
        Ok(output)
    });
    ComponentAdapter::new(inner).with_execute_override(run)
}

/// `instrumented_shadow`: shadow adapter plus histogram timing in one registration-site wrapper.
///
/// Wraps [`shadow_adapter`] in `Instrumented::with_metrics`; see
/// `tests/decision_shadow.rs` for the byte-identical contract.
pub fn instrumented_shadow(
    inner: Arc<dyn Component>,
    label: impl Into<String>,
    histogram: Arc<common_core::metrics::LatencyHistogram>,
    log: Arc<ShadowLog>,
    outcome: ShadowOutcome,
) -> Instrumented<ComponentAdapter> {
    Instrumented::with_metrics(shadow_adapter(inner, log, outcome), label, histogram)
}