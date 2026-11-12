//! Trigger slots: the six Pine entry branches as data-driven components.
//!
//! A trigger reads a bounded [`Tape`] and says which side, if either, it
//! arms. It never reads a filter, a size, or an exit: a trigger is a thesis,
//! and the admission of that thesis is the filter pass's job.
//!
//! The catalog ids are the configured uses: `momentum`, `ribbon_flip`,
//! `osc_cross`, `exhaustion`, `dual_exhaustion`. `TriggerFn` is the escape
//! hatch for a research composition, and is not a catalog row.

#![allow(clippy::unnecessary_wraps)] // uniform factory signature: a row may grow a refusal later

use std::sync::{Arc, OnceLock};

use protopyne_kernels::lite::LiteLegs;
use protopyne_kernels::util::{crossed, crossed_between, Crossing};
use serde::Serialize;

use crate::slots::{FnFactory, SlotError, SlotRegistry};
use crate::tape::Tape;

/// Which thesis a trigger belongs to. The counter-trend arm is admitted
/// through its own filter set; the trend arm borrows the full one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
/// Which thesis a trigger belongs to. The counter-trend arm is admitted
/// through its own filter set; the trend arm borrows the full one.
pub enum TriggerFamily {
    /// Continuation: momentum, ribbon flip, oscillator cross, exhaustion
    /// reversal.
    Trend,
    /// Counter-trend exhaustion confirmed by both oscillators.
    CounterTrend,
}

/// What one trigger concluded at the tape's cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
/// What one trigger concluded at the tape's cursor.
pub struct TriggerVerdict {
    /// The trigger armed a long entry.
    pub long: bool,
    /// The trigger armed a short entry.
    pub short: bool,
    /// The thesis family, for filter-set selection.
    pub family: TriggerFamily,
    /// Stable reason text for a skip line.
    pub reason: &'static str,
}

impl TriggerVerdict {
    /// A verdict that armed nothing.
    #[must_use]
    pub const fn idle(family: TriggerFamily, reason: &'static str) -> Self {
        Self {
            long: false,
            short: false,
            family,
            reason,
        }
    }

    /// A verdict for the armed sides.
    #[must_use]
    pub const fn armed(
        long: bool,
        short: bool,
        family: TriggerFamily,
        reason: &'static str,
    ) -> Self {
        Self {
            long,
            short,
            family,
            reason,
        }
    }
}

/// One entry thesis.
pub trait Trigger: Send + Sync {
    /// Stable catalog id (or a research id for [`TriggerFn`]).
    fn id(&self) -> &'static str;
    /// Evaluate at the tape's cursor.
    fn eval(&self, tape: &Tape<'_>) -> TriggerVerdict;
}

/// Which leg a series trigger reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which leg a series trigger reads.
pub enum Series {
    /// Fast oscillator.
    Fast,
    /// Slow oscillator.
    Slow,
    /// EIT adaptive relay.
    Adaptive,
    /// Ribbon signal leg.
    Signal,
}

impl Series {
    /// The series behind this selector.
    #[must_use]
    pub fn values(self, legs: &LiteLegs) -> &[Option<f64>] {
        match self {
            Self::Fast => &legs.fast,
            Self::Slow => &legs.slow,
            Self::Adaptive => &legs.adaptive,
            Self::Signal => &legs.signal,
        }
    }
}

fn at(series: &[Option<f64>], index: usize) -> Option<f64> {
    series.get(index).copied().flatten()
}

fn crossed_up(series: &[Option<f64>], level: f64, index: usize) -> bool {
    let Some(prev) = index.checked_sub(1) else {
        return false;
    };
    crossed(at(series, prev), at(series, index), level) == Some(Crossing::Up)
}

fn crossed_down(series: &[Option<f64>], level: f64, index: usize) -> bool {
    let Some(prev) = index.checked_sub(1) else {
        return false;
    };
    crossed(at(series, prev), at(series, index), level) == Some(Crossing::Down)
}

fn crossed_series(fast: &[Option<f64>], slow: &[Option<f64>], index: usize) -> Option<Crossing> {
    let prev = index.checked_sub(1)?;
    crossed_between(
        at(fast, prev),
        at(fast, index),
        at(slow, prev),
        at(slow, index),
    )
}

/// The adaptive threshold the reference uses for the momentum arm: 100 while
/// the two oscillators agree in sign, else the slow magnitude floored at 50.
#[must_use]
/// The adaptive threshold the reference uses for the momentum arm: 100 while
/// the two oscillators agree in sign, else the slow magnitude floored at 50.
pub fn adaptive_threshold(fast: f64, slow: f64) -> f64 {
    if (fast >= 0.0) == (slow >= 0.0) {
        100.0
    } else {
        slow.abs().max(50.0)
    }
}

/// A level crossing on one series.
#[derive(Debug, Clone, Copy, PartialEq)]
/// A level crossing on one series.
pub struct CrossLevel {
    /// Level the long side crosses upward.
    pub long_level: f64,
    /// Level the short side crosses downward.
    pub short_level: f64,
    /// Use the adaptive threshold instead of the fixed levels.
    pub adaptive: bool,
    /// Thesis family.
    pub family: TriggerFamily,
    /// Reason text.
    pub reason: &'static str,
}

impl Trigger for CrossLevel {
    fn id(&self) -> &'static str {
        "cross_level"
    }

    fn eval(&self, tape: &Tape<'_>) -> TriggerVerdict {
        let index = tape.at();
        let legs = tape.legs();
        let (long_level, short_level) = if self.adaptive {
            let Some(fast) = at(&legs.fast, index) else {
                return TriggerVerdict::idle(self.family, self.reason);
            };
            let Some(slow) = at(&legs.slow, index) else {
                return TriggerVerdict::idle(self.family, self.reason);
            };
            let threshold = adaptive_threshold(fast, slow);
            (threshold, -threshold)
        } else {
            (self.long_level, self.short_level)
        };
        let long = crossed_up(&legs.fast, long_level, index);
        let short = crossed_down(&legs.fast, short_level, index);
        TriggerVerdict::armed(long, short, self.family, self.reason)
    }
}

/// A crossing between two series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A crossing between two series.
pub struct CrossSeries {
    /// Series the long side crosses above.
    pub fast: Series,
    /// Series it must cross.
    pub slow: Series,
    /// Thesis family.
    pub family: TriggerFamily,
    /// Reason text.
    pub reason: &'static str,
}

impl Trigger for CrossSeries {
    fn id(&self) -> &'static str {
        "cross_series"
    }

    fn eval(&self, tape: &Tape<'_>) -> TriggerVerdict {
        let index = tape.at();
        let legs = tape.legs();
        let crossing = crossed_series(self.fast.values(legs), self.slow.values(legs), index);
        TriggerVerdict::armed(
            crossing == Some(Crossing::Up),
            crossing == Some(Crossing::Down),
            self.family,
            self.reason,
        )
    }
}

/// An extreme-band re-entry: the fast leg crosses back through `±band`.
#[derive(Debug, Clone, Copy, PartialEq)]
/// An extreme-band re-entry: the fast leg crosses back through `±band`.
pub struct BandExtreme {
    /// Band half-width.
    pub band: f64,
    /// Thesis family.
    pub family: TriggerFamily,
    /// Reason text.
    pub reason: &'static str,
}

impl Trigger for BandExtreme {
    fn id(&self) -> &'static str {
        "band_extreme"
    }

    fn eval(&self, tape: &Tape<'_>) -> TriggerVerdict {
        let index = tape.at();
        let legs = tape.legs();
        let long = crossed_up(&legs.fast, -self.band, index);
        let short = crossed_down(&legs.fast, self.band, index);
        TriggerVerdict::armed(long, short, self.family, self.reason)
    }
}

/// The counter-trend exhaustion that needs both oscillators: the slow leg
/// crosses its level while the fast leg sits beyond its own within the
/// lookback window.
#[derive(Debug, Clone, Copy, PartialEq)]
/// The counter-trend exhaustion that needs both oscillators: the slow leg
/// crosses its level while the fast leg sits beyond its own within the
/// lookback window.
pub struct DualExhaustion {
    /// Slow-leg level.
    pub slow_level: f64,
    /// Fast-leg distance.
    pub fast_level: f64,
    /// Bars back the fast leg may sit beyond its level.
    pub lookback: usize,
    /// Reason text.
    pub reason: &'static str,
}

impl Trigger for DualExhaustion {
    fn id(&self) -> &'static str {
        "dual_exhaustion"
    }

    fn eval(&self, tape: &Tape<'_>) -> TriggerVerdict {
        let index = tape.at();
        let legs = tape.legs();
        let fast_beyond = |level: f64, below: bool| {
            (0..=self.lookback.min(index)).any(|back| {
                at(&legs.fast, index - back).is_some_and(|value| {
                    if below {
                        value < level
                    } else {
                        value > level
                    }
                })
            })
        };
        let short =
            crossed_down(&legs.slow, self.slow_level, index) && fast_beyond(self.fast_level, false);
        let long =
            crossed_up(&legs.slow, -self.slow_level, index) && fast_beyond(-self.fast_level, true);
        TriggerVerdict::armed(long, short, TriggerFamily::CounterTrend, self.reason)
    }
}

/// A closure-backed trigger for a research composition.
#[derive(Clone)]
/// A closure-backed trigger for a research composition.
pub struct TriggerFn {
    id: &'static str,
    eval: Arc<dyn Fn(&Tape<'_>) -> TriggerVerdict + Send + Sync>,
}

impl TriggerFn {
    /// Build a closure trigger.
    #[must_use]
    pub fn new(
        id: &'static str,
        eval: impl Fn(&Tape<'_>) -> TriggerVerdict + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            eval: Arc::new(eval),
        }
    }
}

impl Trigger for TriggerFn {
    fn id(&self) -> &'static str {
        self.id
    }

    fn eval(&self, tape: &Tape<'_>) -> TriggerVerdict {
        (self.eval)(tape)
    }
}

fn required_f64(params: &serde_json::Value, key: &str, id: &'static str) -> Result<f64, SlotError> {
    params
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| SlotError::invalid(id, format!("{key} must be a number")))
}

fn optional_f64(params: &serde_json::Value, key: &str) -> f64 {
    params
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
}

fn family_of(params: &serde_json::Value) -> TriggerFamily {
    match params.get("family").and_then(serde_json::Value::as_str) {
        Some("counter_trend" | "counter-trend") => TriggerFamily::CounterTrend,
        _ => TriggerFamily::Trend,
    }
}

fn series_of(params: &serde_json::Value, key: &str, id: &'static str) -> Result<Series, SlotError> {
    match params.get(key).and_then(serde_json::Value::as_str) {
        Some("fast") => Ok(Series::Fast),
        Some("slow") => Ok(Series::Slow),
        Some("adaptive") => Ok(Series::Adaptive),
        Some("signal") => Ok(Series::Signal),
        other => Err(SlotError::invalid(
            id,
            format!("{key} must be fast/slow/adaptive/signal, got {other:?}"),
        )),
    }
}

fn trigger_registry() -> &'static SlotRegistry<Arc<dyn Trigger>> {
    static REGISTRY: OnceLock<SlotRegistry<Arc<dyn Trigger>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = SlotRegistry::new();
        for (id, build) in [
            (
                "momentum",
                momentum_factory as fn(&serde_json::Value) -> Result<Arc<dyn Trigger>, SlotError>,
            ),
            ("ribbon_flip", ribbon_flip_factory),
            ("osc_cross", osc_cross_factory),
            ("exhaustion", exhaustion_factory),
            ("dual_exhaustion", dual_exhaustion_factory),
        ] {
            registry
                .register(Arc::new(FnFactory::new(id, build)))
                .expect("unique trigger id");
        }
        registry
    })
}

fn momentum_factory(params: &serde_json::Value) -> Result<Arc<dyn Trigger>, SlotError> {
    Ok(Arc::new(CrossLevel {
        long_level: optional_f64(params, "long_level"),
        short_level: optional_f64(params, "short_level"),
        adaptive: params
            .get("adaptive")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        family: family_of(params),
        reason: "momentum",
    }))
}

fn ribbon_flip_factory(params: &serde_json::Value) -> Result<Arc<dyn Trigger>, SlotError> {
    Ok(Arc::new(CrossSeries {
        fast: Series::Adaptive,
        slow: Series::Signal,
        family: family_of(params),
        reason: "ribbon-flip",
    }))
}

fn osc_cross_factory(params: &serde_json::Value) -> Result<Arc<dyn Trigger>, SlotError> {
    Ok(Arc::new(CrossSeries {
        fast: series_of(params, "fast", "osc_cross")?,
        slow: series_of(params, "slow", "osc_cross")?,
        family: family_of(params),
        reason: "oscillator-cross",
    }))
}

fn exhaustion_factory(params: &serde_json::Value) -> Result<Arc<dyn Trigger>, SlotError> {
    Ok(Arc::new(BandExtreme {
        band: required_f64(params, "band", "exhaustion")?,
        family: family_of(params),
        reason: "exhaustion",
    }))
}

fn dual_exhaustion_factory(params: &serde_json::Value) -> Result<Arc<dyn Trigger>, SlotError> {
    Ok(Arc::new(DualExhaustion {
        slow_level: required_f64(params, "slow_level", "dual_exhaustion")?,
        fast_level: required_f64(params, "fast_level", "dual_exhaustion")?,
        lookback: params
            .get("lookback")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize,
        reason: "counter-trend-exhaustion",
    }))
}

/// The trigger catalog.
#[must_use]
/// The trigger catalog.
pub fn triggers() -> &'static SlotRegistry<Arc<dyn Trigger>> {
    trigger_registry()
}

/// The catalog ids, in schema order.
#[must_use]
/// The catalog ids, in schema order.
pub fn trigger_ids() -> Vec<&'static str> {
    triggers().ids()
}

#[cfg(test)]
#[path = "../../tests/trigger_pins.rs"]
mod trigger_pins_tests;