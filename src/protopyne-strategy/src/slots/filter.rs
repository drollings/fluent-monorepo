//! Filter slots: admission of an entry thesis.
//!
//! A filter sees the same bounded [`Tape`] as a trigger and answers pass or
//! block with a reason. It never arms a side, sizes a position, or exits: a
//! block is task-value text for the skip line, and the decision stays the
//! caller's.
//!
//! Two scopes exist. `All` filters apply to every admission (regime, relative
//! volume, delta alignment); `TrendOnly` filters are the continuation
//! thesis's own guards and are skipped when the counter-trend arm is admitted
//! through its own set.

#![allow(clippy::unnecessary_wraps)] // uniform factory signature: a row may grow a refusal later

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use protopyne_kernels::bars::estimate_delta_volume_estimated;
use protopyne_kernels::market_context::{detect_regime_stable, MarketRegime};

use crate::proposal::Direction;
use crate::slots::{FnFactory, SlotError, SlotRegistry};
use crate::tape::Tape;
use serde::Serialize;

/// Which admissions a filter applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which admissions a filter applies to.
pub enum FilterScope {
    /// Every admission.
    All,
    /// The continuation thesis only; the counter-trend set skips it.
    TrendOnly,
}

/// The verdict of one filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
/// The verdict of one filter.
pub enum Admission {
    /// The thesis is admissible.
    Pass,
    /// It is blocked, with a task-value reason for the skip line.
    Block(&'static str),
}

impl Admission {
    /// Whether this is [`Admission::Pass`].
    #[must_use]
    pub const fn is_pass(self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// One admission guard.
pub trait Filter: Send + Sync {
    /// Stable catalog id (or a research id for [`FilterFn`]).
    fn id(&self) -> &'static str;
    /// Which admissions this guard applies to.
    fn scope(&self) -> FilterScope {
        FilterScope::All
    }
    /// Admit or block the thesis at the tape's cursor.
    fn admit(&self, tape: &Tape<'_>, direction: Direction) -> Admission;
}

fn at(series: &[Option<f64>], index: usize) -> Option<f64> {
    series.get(index).copied().flatten()
}

fn side(value: f64, direction: Direction) -> bool {
    match direction {
        Direction::Long => value > 0.0,
        Direction::Short => value < 0.0,
    }
}

/// RIBBON alignment: adaptive above signal for long, below for short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// RIBBON alignment: adaptive above signal for long, below for short.
pub struct RibbonFilter;

impl Filter for RibbonFilter {
    fn id(&self) -> &'static str {
        "ribbon"
    }

    fn scope(&self) -> FilterScope {
        FilterScope::TrendOnly
    }

    fn admit(&self, tape: &Tape<'_>, direction: Direction) -> Admission {
        let index = tape.at();
        let legs = tape.legs();
        match (at(&legs.adaptive, index), at(&legs.signal, index)) {
            (Some(adaptive), Some(signal))
                if (direction == Direction::Long && adaptive > signal)
                    || (direction == Direction::Short && adaptive < signal) =>
            {
                Admission::Pass
            }
            _ => Admission::Block("ribbon is not aligned with the side"),
        }
    }
}

/// MA-health alignment with a minimum magnitude.
#[derive(Debug, Clone, Copy, PartialEq)]
/// MA-health alignment with a minimum magnitude.
pub struct MaHealthFilter {
    /// Minimum absolute health reading.
    pub minimum: f64,
}

impl Filter for MaHealthFilter {
    fn id(&self) -> &'static str {
        "ma_health"
    }

    fn scope(&self) -> FilterScope {
        FilterScope::TrendOnly
    }

    fn admit(&self, tape: &Tape<'_>, direction: Direction) -> Admission {
        let value = at(&tape.legs().ma_health, tape.at());
        match value {
            Some(value) if side(value, direction) && value.abs() >= self.minimum => Admission::Pass,
            _ => Admission::Block("MA health is not aligned or is too weak"),
        }
    }
}

/// Ribbon-health alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Ribbon-health alignment.
pub struct RibbonHealthFilter;

impl Filter for RibbonHealthFilter {
    fn id(&self) -> &'static str {
        "ribbon_health"
    }

    fn scope(&self) -> FilterScope {
        FilterScope::TrendOnly
    }

    fn admit(&self, tape: &Tape<'_>, direction: Direction) -> Admission {
        match at(&tape.legs().ribbon_health, tape.at()) {
            Some(value) if side(value, direction) => Admission::Pass,
            _ => Admission::Block("ribbon health is not aligned with the side"),
        }
    }
}

/// Slow-oscillator side with a possibly negative floor.
#[derive(Debug, Clone, Copy, PartialEq)]
/// Slow-oscillator side with a possibly negative floor.
pub struct SlowSideFilter {
    /// Floor: long needs the slow leg above `+minimum`, short below
    /// `-minimum`.
    pub minimum: f64,
}

impl Filter for SlowSideFilter {
    fn id(&self) -> &'static str {
        "slow_side"
    }

    fn scope(&self) -> FilterScope {
        FilterScope::TrendOnly
    }

    fn admit(&self, tape: &Tape<'_>, direction: Direction) -> Admission {
        let value = at(&tape.legs().slow, tape.at());
        let ok = match value {
            Some(value) => match direction {
                Direction::Long => value > self.minimum,
                Direction::Short => value < -self.minimum,
            },
            None => false,
        };
        if ok {
            Admission::Pass
        } else {
            Admission::Block("slow side is not on the trade's side")
        }
    }
}

/// Shape-estimated delta alignment over a trailing window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Shape-estimated delta alignment over a trailing window.
pub struct DeltaSideFilter {
    /// Bars in the trailing window (at least one).
    pub length: usize,
}

impl Filter for DeltaSideFilter {
    fn id(&self) -> &'static str {
        "delta_side"
    }

    fn admit(&self, tape: &Tape<'_>, direction: Direction) -> Admission {
        let index = tape.at();
        let len = self.length.max(1).min(index + 1);
        let mut sum = 0.0;
        let mut count = 0_usize;
        for back in 0..len {
            let Some(bar) = tape.bar(index - back) else {
                continue;
            };
            use rust_decimal::prelude::ToPrimitive as _;
            let est = estimate_delta_volume_estimated(
                bar.o.to_f64().unwrap_or(f64::NAN),
                bar.h.to_f64().unwrap_or(f64::NAN),
                bar.l.to_f64().unwrap_or(f64::NAN),
                bar.c.to_f64().unwrap_or(f64::NAN),
                bar.volume as f64,
            );
            let sell = bar.volume as f64 - est.buy_volume;
            if est.buy_volume.is_finite() && sell.is_finite() {
                sum += est.buy_volume - sell;
                count += 1;
            }
        }
        if count == 0 {
            return Admission::Block("delta is unmeasurable");
        }
        let mean = sum / count as f64;
        let ok = match direction {
            Direction::Long => mean > 0.0,
            Direction::Short => mean < 0.0,
        };
        if ok {
            Admission::Pass
        } else {
            Admission::Block("delta is on the other side")
        }
    }
}

/// Relative-volume floor.
#[derive(Debug, Clone, Copy, PartialEq)]
/// Relative-volume floor.
pub struct VolumeMinFilter {
    /// Minimum volume/volume-SMA ratio.
    pub minimum: f64,
}

impl Filter for VolumeMinFilter {
    fn id(&self) -> &'static str {
        "volume_min"
    }

    fn admit(&self, tape: &Tape<'_>, _direction: Direction) -> Admission {
        let index = tape.at();
        let ratio = tape.legs().vol_ratio.get(index).copied().unwrap_or(0.0);
        if ratio < self.minimum {
            Admission::Block("relative volume is below the floor")
        } else {
            Admission::Pass
        }
    }
}

/// Distance-from-mean cap in ATRs.
#[derive(Debug, Clone, Copy, PartialEq)]
/// Distance-from-mean cap in ATRs.
pub struct AtrDistanceMaxFilter {
    /// Maximum `|close - mean| / atr`.
    pub maximum: f64,
}

impl Filter for AtrDistanceMaxFilter {
    fn id(&self) -> &'static str {
        "atr_distance_max"
    }

    fn scope(&self) -> FilterScope {
        FilterScope::TrendOnly
    }

    fn admit(&self, tape: &Tape<'_>, _direction: Direction) -> Admission {
        let index = tape.at();
        let legs = tape.legs();
        match (at(&legs.mean, index), at(&legs.atr, index)) {
            (Some(mean), Some(atr)) if atr > 0.0 => {
                let close = legs.close.get(index).copied().unwrap_or(f64::NAN);
                if (close - mean).abs() > self.maximum * atr {
                    Admission::Block("price is too extended from the mean")
                } else {
                    Admission::Pass
                }
            }
            _ => Admission::Block("extension is unmeasurable"),
        }
    }
}

/// Regime skip with hysteresis state held per filter instance.
///
/// The label is written back on every evaluation, exactly as the caller-held
/// hysteresis did, so a skipped regime remains a state transition rather than
/// a stateless veto.
#[derive(Debug)]
/// Regime skip with hysteresis state held per filter instance.
/// 
/// The label is written back on every evaluation, exactly as the caller-held
/// hysteresis did, so a skipped regime remains a state transition rather than
/// a stateless veto.
pub struct RegimeSkip {
    /// Skip the quiet regime.
    pub low_volume: bool,
    /// Skip the volatile regime.
    pub volatile: bool,
    previous: AtomicU8,
}

impl RegimeSkip {
    /// Build a regime skip from the two arm switches.
    #[must_use]
    pub fn new(low_volume: bool, volatile: bool) -> Self {
        Self {
            low_volume,
            volatile,
            previous: AtomicU8::new(0),
        }
    }

    /// The held label (the last evaluation's result).
    #[must_use]
    pub fn previous(&self) -> MarketRegime {
        regime_from_code(self.previous.load(Ordering::Relaxed))
    }
}

impl Filter for RegimeSkip {
    fn id(&self) -> &'static str {
        "regime_skip"
    }

    fn admit(&self, tape: &Tape<'_>, _direction: Direction) -> Admission {
        let index = tape.at();
        let legs = tape.legs();
        let volatility_ratio = match (at(&legs.stdev, index), at(&legs.atr, index)) {
            (Some(dev), Some(atr)) if atr > 0.0 => Some(dev / atr),
            _ => None,
        };
        let vol_ratio = legs.vol_ratio.get(index).copied();
        let previous = regime_from_code(self.previous.load(Ordering::Relaxed));
        let regime = detect_regime_stable(
            previous,
            at(&legs.ma_health, index),
            volatility_ratio,
            vol_ratio,
        );
        self.previous.store(regime_code(regime), Ordering::Relaxed);
        if self.low_volume && regime == MarketRegime::LowVolume {
            return Admission::Block("regime is too quiet");
        }
        if self.volatile && regime == MarketRegime::Volatile {
            return Admission::Block("regime is too volatile");
        }
        Admission::Pass
    }
}

fn regime_code(regime: MarketRegime) -> u8 {
    match regime {
        MarketRegime::Ranging => 0,
        MarketRegime::TrendingBull => 1,
        MarketRegime::TrendingBear => 2,
        MarketRegime::Volatile => 3,
        MarketRegime::LowVolume => 4,
    }
}

fn regime_from_code(code: u8) -> MarketRegime {
    match code {
        1 => MarketRegime::TrendingBull,
        2 => MarketRegime::TrendingBear,
        3 => MarketRegime::Volatile,
        4 => MarketRegime::LowVolume,
        _ => MarketRegime::Ranging,
    }
}

/// A closure-backed filter for a research composition.
#[derive(Clone)]
/// A closure-backed filter for a research composition.
pub struct FilterFn {
    id: &'static str,
    #[allow(clippy::type_complexity)]
    admit: Arc<dyn Fn(&Tape<'_>, Direction) -> Admission + Send + Sync>,
}

impl FilterFn {
    /// Build a closure filter.
    #[must_use]
    pub fn new(
        id: &'static str,
        admit: impl Fn(&Tape<'_>, Direction) -> Admission + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            admit: Arc::new(admit),
        }
    }
}

impl Filter for FilterFn {
    fn id(&self) -> &'static str {
        self.id
    }

    fn admit(&self, tape: &Tape<'_>, direction: Direction) -> Admission {
        (self.admit)(tape, direction)
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

fn bool_of(params: &serde_json::Value, key: &str) -> bool {
    params
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn filter_registry() -> &'static SlotRegistry<Arc<dyn Filter>> {
    static REGISTRY: OnceLock<SlotRegistry<Arc<dyn Filter>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = SlotRegistry::new();
        for (id, build) in [
            (
                "ribbon",
                ribbon_factory as fn(&serde_json::Value) -> Result<Arc<dyn Filter>, SlotError>,
            ),
            ("ma_health", ma_health_factory),
            ("ribbon_health", ribbon_health_factory),
            ("slow_side", slow_side_factory),
            ("delta_side", delta_side_factory),
            ("volume_min", volume_min_factory),
            ("atr_distance_max", atr_distance_factory),
            ("regime_skip", regime_factory),
        ] {
            registry
                .register(Arc::new(FnFactory::new(id, build)))
                .expect("unique filter id");
        }
        registry
    })
}

fn ribbon_factory(_: &serde_json::Value) -> Result<Arc<dyn Filter>, SlotError> {
    Ok(Arc::new(RibbonFilter))
}

fn ma_health_factory(params: &serde_json::Value) -> Result<Arc<dyn Filter>, SlotError> {
    Ok(Arc::new(MaHealthFilter {
        minimum: optional_f64(params, "minimum"),
    }))
}

fn ribbon_health_factory(_: &serde_json::Value) -> Result<Arc<dyn Filter>, SlotError> {
    Ok(Arc::new(RibbonHealthFilter))
}

fn slow_side_factory(params: &serde_json::Value) -> Result<Arc<dyn Filter>, SlotError> {
    Ok(Arc::new(SlowSideFilter {
        minimum: optional_f64(params, "minimum"),
    }))
}

fn delta_side_factory(params: &serde_json::Value) -> Result<Arc<dyn Filter>, SlotError> {
    Ok(Arc::new(DeltaSideFilter {
        length: params
            .get("length")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1) as usize,
    }))
}

fn volume_min_factory(params: &serde_json::Value) -> Result<Arc<dyn Filter>, SlotError> {
    Ok(Arc::new(VolumeMinFilter {
        minimum: required_f64(params, "minimum", "volume_min")?,
    }))
}

fn atr_distance_factory(params: &serde_json::Value) -> Result<Arc<dyn Filter>, SlotError> {
    Ok(Arc::new(AtrDistanceMaxFilter {
        maximum: required_f64(params, "maximum", "atr_distance_max")?,
    }))
}

fn regime_factory(params: &serde_json::Value) -> Result<Arc<dyn Filter>, SlotError> {
    Ok(Arc::new(RegimeSkip::new(
        bool_of(params, "low_volume"),
        bool_of(params, "volatile"),
    )))
}

/// The filter catalog.
#[must_use]
/// The filter catalog.
pub fn filters() -> &'static SlotRegistry<Arc<dyn Filter>> {
    filter_registry()
}

/// The catalog ids, in schema order.
#[must_use]
/// The catalog ids, in schema order.
pub fn filter_ids() -> Vec<&'static str> {
    filters().ids()
}

#[cfg(test)]
#[path = "../../tests/regime_calibration.rs"]
mod regime_calibration_tests;

#[cfg(test)]
#[path = "../../tests/filter_pins.rs"]
mod filter_pins_tests;