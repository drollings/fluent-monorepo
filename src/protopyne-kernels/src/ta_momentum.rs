//! Momentum remainder: price oscillator, rate-of-change family, stochastic
//! fast pair, triple-EMA rate, Aroon, and ADX.
//!
//! `MACD`, `RSI`, and `STOCH` already ship in `oscillators` and are untouched
//! here; this module adds only what is missing. Two reference behaviors shape
//! the confidence contract and are pinned by tests, not assumed:
//!
//! * `RSV`/`WILLR` define flat-range (and warmup) as `0`, the reference's
//!   `otherwise(0)` branch — so those kernels are confident from bar zero
//!   (`warmup` 1) with a `Some(0.0)` floor, unlike the `na`-on-flat `stoch`.
//! * Zero denominators elsewhere refuse via
//!   [`div_or_none`](crate::math::div_or_none): the reference yields `inf`
//!   there, so Expr-parity suites run on nonzero data and a dedicated degenerate
//!   test pins the `None` refusal.
//!
//! Multi-pass kernels compose the shared [`SmaIndicator`](crate::sma::SmaIndicator)
//! and [`EmaIndicator`](crate::ema::EmaIndicator) through child states,
//! exactly like the `dema`/`macd` compositions, so batch and incremental paths
//! share every recurrence.

use crate::averages::RmaIndicator;
use crate::ema::EmaIndicator;
use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, BarTick, FullTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState,
    NamedSeries, NamedValue, Ohlcv, ParamSet,
};
use crate::sma::SmaIndicator;

/// Resolve the reference `matype` switch (0 SMA, 1 EMA); anything else fails
/// closed instead of silently substituting. Exact comparison: only the exact
/// values 0/1 select a smoothing mode, mirroring the reference's
/// `matype == 0` branch — a near-1.0 float must not select EMA by accident.
#[allow(clippy::float_cmp)]
fn resolve_matype(id: &'static str, params: &ParamSet) -> Result<bool, KernelError> {
    let matype = params.float(id, "matype").unwrap_or(0.0);
    if matype == 0.0 {
        Ok(false)
    } else if matype == 1.0 {
        Ok(true)
    } else {
        Err(KernelError::InvalidParam(format!(
            "{id}: matype {matype} unsupported, only 0 (SMA) and 1 (EMA) are implemented"
        )))
    }
}

/// Absolute price oscillator (`apo`), parameterized by `fast_length`,
/// `slow_length`, and `matype` (0 SMA, 1 EMA — both first-class reference
/// paths, anything else refused).
pub struct ApoIndicator {
    spec: IndicatorSpec,
}

impl ApoIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "apo";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Absolute Price Oscillator".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn shape(params: &ParamSet) -> Result<(usize, usize, bool), KernelError> {
        let fast = crate::math::period_len(Self::ID, params, "fast_length")?;
        let slow = crate::math::period_len(Self::ID, params, "slow_length")?;
        // Both reference smoothing modes are implemented; anything else fails
        // closed instead of silently substituting.
        Ok((fast, slow, resolve_matype(Self::ID, params)?))
    }

    fn legs(period: (usize, usize)) -> (ParamSet, ParamSet) {
        (
            ParamSet::new().with_int("length", period.0 as i64),
            ParamSet::new().with_int("length", period.1 as i64),
        )
    }
}

impl Default for ApoIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for ApoIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let (fast, slow, _) = Self::shape(params)?;
        Ok(fast.max(slow))
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (fast, slow, use_ema) = Self::shape(params)?;
        let (fast_ps, slow_ps) = Self::legs((fast, slow));
        let (fast_leg, slow_leg) = if use_ema {
            let ema = EmaIndicator::new();
            (
                ema.batch(data, &fast_ps)?.values,
                ema.batch(data, &slow_ps)?.values,
            )
        } else {
            let sma = SmaIndicator::new();
            (
                sma.batch(data, &fast_ps)?.values,
                sma.batch(data, &slow_ps)?.values,
            )
        };
        let values = fast_leg
            .iter()
            .zip(slow_leg.iter())
            .map(|(f, s)| match (f, s) {
                (Some(f), Some(s)) => fin_or_none(f - s),
                _ => None,
            })
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let (fast, slow, use_ema) = Self::shape(params)?;
        let (fast_ps, slow_ps) = Self::legs((fast, slow));
        let (fast_leg, slow_leg) = if use_ema {
            let ema = EmaIndicator::new();
            (
                ema.update(state.child(0), bar, &fast_ps)?,
                ema.update(state.child(1), bar, &slow_ps)?,
            )
        } else {
            let sma = SmaIndicator::new();
            (
                sma.update(state.child(0), bar, &fast_ps)?,
                sma.update(state.child(1), bar, &slow_ps)?,
            )
        };
        let raw = match (fast_leg, slow_leg) {
            (Some(f), Some(s)) => f - s,
            _ => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Percentage price oscillator (`ppo`): the `apo` legs as a ratio minus one,
/// parameterized identically (`fast_length`, `slow_length`, `matype`).
pub struct PpoIndicator {
    spec: IndicatorSpec,
}

impl PpoIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "ppo";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Percentage Price Oscillator".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn shape(params: &ParamSet) -> Result<(usize, usize, bool), KernelError> {
        let fast = crate::math::period_len(Self::ID, params, "fast_length")?;
        let slow = crate::math::period_len(Self::ID, params, "slow_length")?;
        Ok((fast, slow, resolve_matype(Self::ID, params)?))
    }

    fn ratio(fast: f64, slow: f64) -> f64 {
        crate::math::div_or_none(fast, slow)
            .and_then(|r| fin_or_none(r - 1.0))
            .unwrap_or(f64::NAN)
    }
}

impl Default for PpoIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for PpoIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let (fast, slow, _) = Self::shape(params)?;
        Ok(fast.max(slow))
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (fast, slow, use_ema) = Self::shape(params)?;
        let (fast_ps, slow_ps) = ApoIndicator::legs((fast, slow));
        let (fast_leg, slow_leg) = if use_ema {
            let ema = EmaIndicator::new();
            (
                ema.batch(data, &fast_ps)?.values,
                ema.batch(data, &slow_ps)?.values,
            )
        } else {
            let sma = SmaIndicator::new();
            (
                sma.batch(data, &fast_ps)?.values,
                sma.batch(data, &slow_ps)?.values,
            )
        };
        let values = fast_leg
            .iter()
            .zip(slow_leg.iter())
            .map(|(f, s)| match (f, s) {
                (Some(f), Some(s)) => fin_or_none(Self::ratio(*f, *s)),
                _ => None,
            })
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let (fast, slow, use_ema) = Self::shape(params)?;
        let (fast_ps, slow_ps) = ApoIndicator::legs((fast, slow));
        let (fast_leg, slow_leg) = if use_ema {
            let ema = EmaIndicator::new();
            (
                ema.update(state.child(0), bar, &fast_ps)?,
                ema.update(state.child(1), bar, &slow_ps)?,
            )
        } else {
            let sma = SmaIndicator::new();
            (
                sma.update(state.child(0), bar, &fast_ps)?,
                sma.update(state.child(1), bar, &slow_ps)?,
            )
        };
        let raw = match (fast_leg, slow_leg) {
            (Some(f), Some(s)) => Self::ratio(f, s),
            _ => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Momentum (`mom`): close minus close `length` bars ago.
pub struct MomIndicator {
    spec: IndicatorSpec,
}

impl MomIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "mom";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Momentum".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for MomIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for MomIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let period = Self::period(params)?;
        let mut values = crate::math::warmup_none(closes.len());
        if period < closes.len() {
            for (i, slot) in values.iter_mut().enumerate().skip(period) {
                *slot = fin_or_none(closes[i] - closes[i - period]);
            }
        }
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        state.push(bar, period + 1);
        if state.len() < period + 1 {
            return Ok(None);
        }
        let back = state.closes().next().unwrap_or(f64::NAN);
        let raw = bar.close - back;
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Shared shift-ratio core for the rate-of-change family: `base` maps the
/// `(current, back)` pair, `scale` maps the warmed ratio.
fn roc_batch(
    id: &str,
    closes: &[f64],
    period: usize,
    ratio: impl Fn(f64, f64) -> Option<f64>,
) -> Vec<Option<f64>> {
    let mut values = crate::math::warmup_none(closes.len());
    if period < closes.len() {
        for (i, slot) in values.iter_mut().enumerate().skip(period) {
            *slot = ratio(closes[i], closes[i - period]);
        }
    }
    let _ = id;
    values
}

/// Shifted-leg state helper: retain `period + 1` bars and split current from
/// the bar `period` back, returning `None` while warming.
fn roc_shift(state: &mut IndicatorState, bar: BarTick, period: usize) -> Option<(f64, f64)> {
    state.push(bar, period + 1);
    if state.len() < period + 1 {
        return None;
    }
    state.closes().next().map(|back| (bar.close, back))
}

/// Rate of change percentage (`rocp`): `(close - back) / back`.
pub struct RocpIndicator {
    spec: IndicatorSpec,
}

impl RocpIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "rocp";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Rate of Change Percentage".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    fn ratio(current: f64, back: f64) -> Option<f64> {
        crate::math::div_or_none(current - back, back)
    }
}

impl Default for RocpIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for RocpIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        Ok(IndicatorResult::new(
            Self::ID.to_owned(),
            roc_batch(Self::ID, data.close, period, Self::ratio),
        ))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        let raw = match roc_shift(state, bar, period) {
            Some((current, back)) => Self::ratio(current, back).unwrap_or(f64::NAN),
            None => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Rate of change (`roc`): `rocp` scaled by 100, same warmup and legs.
pub struct RocIndicator {
    spec: IndicatorSpec,
}

impl RocIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "roc";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Rate of Change".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for RocIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for RocIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        Ok(IndicatorResult::new(
            Self::ID.to_owned(),
            roc_batch(Self::ID, data.close, period, |current, back| {
                RocpIndicator::ratio(current, back).and_then(|r| fin_or_none(r * 100.0))
            }),
        ))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        let raw = match roc_shift(state, bar, period) {
            Some((current, back)) => RocpIndicator::ratio(current, back)
                .and_then(|r| fin_or_none(r * 100.0))
                .unwrap_or(f64::NAN),
            None => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Rate of change ratio (`rocr`): `close / back`.
pub struct RocrIndicator {
    spec: IndicatorSpec,
}

impl RocrIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "rocr";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Rate of Change Ratio".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for RocrIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for RocrIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        Ok(IndicatorResult::new(
            Self::ID.to_owned(),
            roc_batch(Self::ID, data.close, period, crate::math::div_or_none),
        ))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        let raw = match roc_shift(state, bar, period) {
            Some((current, back)) => crate::math::div_or_none(current, back).unwrap_or(f64::NAN),
            None => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Rate of change ratio ×100 (`rocr100`): `rocr` scaled by 100.
pub struct Rocr100Indicator {
    spec: IndicatorSpec,
}

impl Rocr100Indicator {
    /// Stable registry id.
    pub const ID: &'static str = "rocr100";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Rate of Change Ratio 100".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for Rocr100Indicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for Rocr100Indicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        Ok(IndicatorResult::new(
            Self::ID.to_owned(),
            roc_batch(Self::ID, data.close, period, |current, back| {
                crate::math::div_or_none(current, back).and_then(|r| fin_or_none(r * 100.0))
            }),
        ))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        let raw = match roc_shift(state, bar, period) {
            Some((current, back)) => crate::math::div_or_none(current, back)
                .and_then(|r| fin_or_none(r * 100.0))
                .unwrap_or(f64::NAN),
            None => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Raw stochastic value (`rsv`): close position inside the rolling
/// high/low range. An exactly flat window is the defined `0`, not `na`
/// (the reference's `otherwise` branch), and the same defined zero covers
/// the still-warming prefix — so this kernel reports `warmup` 1.
pub struct RsvIndicator {
    spec: IndicatorSpec,
}

impl RsvIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "rsv";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Raw Stochastic Value".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    /// Position of `close` inside `[lowest, highest]`; exactly-flat windows
    /// read the defined zero. Exact comparison, like the legacy `stoch`:
    /// microscopically gapped markets must not read as flat.
    fn ratio_of(close: f64, highest: f64, lowest: f64) -> f64 {
        #[allow(clippy::float_cmp)]
        if highest == lowest {
            0.0
        } else {
            (close - lowest) / (highest - lowest)
        }
    }
}

impl Default for RsvIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for RsvIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let values = data
            .close
            .iter()
            .enumerate()
            .map(|(i, &close)| {
                if i + 1 < period {
                    return Some(0.0);
                }
                let window_h = &high[i + 1 - period..=i];
                let window_l = &low[i + 1 - period..=i];
                let highest = window_h.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let lowest = window_l.iter().copied().fold(f64::INFINITY, f64::min);
                fin_or_none(Self::ratio_of(close, highest, lowest))
            })
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        state.push(bar, period);
        if state.len() < period {
            state.set_last(0.0);
            return Ok(Some(0.0));
        }
        let highest = state.highs().fold(f64::NEG_INFINITY, f64::max);
        let lowest = state.lows().fold(f64::INFINITY, f64::min);
        let raw = Self::ratio_of(bar.close, highest, lowest);
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Williams %R (`willr`): the `rsv` mirror — distance from the rolling
/// highest high, with the same defined-zero floor and `warmup` 1.
pub struct WillrIndicator {
    spec: IndicatorSpec,
}

impl WillrIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "willr";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Williams Percent R".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    /// Distance of `close` below `highest`, as a fraction of the range;
    /// exactly-flat windows read the defined zero.
    fn ratio_of(close: f64, highest: f64, lowest: f64) -> f64 {
        #[allow(clippy::float_cmp)]
        if highest == lowest {
            0.0
        } else {
            (highest - close) / (highest - lowest)
        }
    }
}

impl Default for WillrIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for WillrIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let values = data
            .close
            .iter()
            .enumerate()
            .map(|(i, &close)| {
                if i + 1 < period {
                    return Some(0.0);
                }
                let window_h = &high[i + 1 - period..=i];
                let window_l = &low[i + 1 - period..=i];
                let highest = window_h.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let lowest = window_l.iter().copied().fold(f64::INFINITY, f64::min);
                fin_or_none(Self::ratio_of(close, highest, lowest))
            })
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        state.push(bar, period);
        if state.len() < period {
            state.set_last(0.0);
            return Ok(Some(0.0));
        }
        let highest = state.highs().fold(f64::NEG_INFINITY, f64::max);
        let lowest = state.lows().fold(f64::INFINITY, f64::min);
        let raw = Self::ratio_of(bar.close, highest, lowest);
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Fast stochastics (`stochf`): `rsv` primary with its `fastd_length` SMA as
/// the `fastd` secondary, parameterized by `fastk_length`/`fastd_length`.
pub struct StochfIndicator {
    spec: IndicatorSpec,
}

impl StochfIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "stochf";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Fast Stochastics".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn lengths(params: &ParamSet) -> Result<(usize, usize), KernelError> {
        Ok((
            crate::math::period_len(Self::ID, params, "fastk_length")?,
            crate::math::period_len(Self::ID, params, "fastd_length")?,
        ))
    }
}

impl Default for StochfIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for StochfIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        // The fastk leg carries the reference's defined zero from bar zero.
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (k_len, d_len) = Self::lengths(params)?;
        let rsv = RsvIndicator::new();
        let fastk = rsv.batch(data, &ParamSet::new().with_int("length", k_len as i64))?;
        // The fastk leg never warms to `None` (defined-zero floor), so the
        // smoothing input is a plain finite series here.
        let fastk_raw: Vec<f64> = fastk.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        let sma = SmaIndicator::new();
        let fastd = sma.batch(
            &Ohlcv::close_only(&fastk_raw),
            &ParamSet::new().with_int("length", d_len as i64),
        )?;
        Ok(IndicatorResult::new(Self::ID.to_owned(), fastk.values)
            .with_secondary(NamedSeries::new("fastd", fastd.values)))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(self.update_full(state, bar, params)?.primary)
    }

    fn update_full(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<FullTick, KernelError> {
        let (k_len, d_len) = Self::lengths(params)?;
        let rsv = RsvIndicator::new();
        let fastk = rsv.update(
            state.child(0),
            bar,
            &ParamSet::new().with_int("length", k_len as i64),
        )?;
        let sma = SmaIndicator::new();
        let fastd = sma.update(
            state.child(1),
            BarTick::close_only(fastk.unwrap_or(f64::NAN)),
            &ParamSet::new().with_int("length", d_len as i64),
        )?;
        state.set_last(fastk.unwrap_or(f64::NAN));
        Ok(FullTick {
            primary: fastk,
            secondary: vec![NamedValue {
                name: "fastd".to_owned(),
                value: fastd,
            }],
        })
    }
}

/// Triple-EMA rate of change (`trix`): three cascaded `EmaIndicator` passes
/// (the `tema` composition shape) with a one-bar relative shift of the third
/// pass. `warmup` is `3n - 1`: `3n - 2` for the third pass plus one bar for
/// the shift.
pub struct TrixIndicator {
    spec: IndicatorSpec,
}

impl TrixIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "trix";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Triple EMA Rate of Change".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    fn ema_params(period: usize) -> ParamSet {
        ParamSet::new().with_int("length", period as i64)
    }

    fn ratio_of(current: f64, prev: f64) -> f64 {
        crate::math::div_or_none(current - prev, prev).unwrap_or(f64::NAN)
    }
}

impl Default for TrixIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for TrixIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let period = Self::period(params)?;
        Ok(3 * period - 1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let ema = EmaIndicator::new();
        let first = ema.batch(data, &Self::ema_params(period))?;
        let first_raw: Vec<f64> = first.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        let second = ema.batch(&Ohlcv::close_only(&first_raw), &Self::ema_params(period))?;
        let second_raw: Vec<f64> =
            second.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        let third = ema.batch(&Ohlcv::close_only(&second_raw), &Self::ema_params(period))?;
        let mut values = crate::math::warmup_none(data.len());
        for (i, slot) in values.iter_mut().enumerate().skip(1) {
            *slot = match (third.values[i], third.values[i - 1]) {
                (Some(current), Some(prev)) => fin_or_none(Self::ratio_of(current, prev)),
                _ => None,
            };
        }
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        let ema = EmaIndicator::new();
        let first = ema.update(state.child(0), bar, &Self::ema_params(period))?;
        let second = ema.update(
            state.child(1),
            BarTick::close_only(first.unwrap_or(f64::NAN)),
            &Self::ema_params(period),
        )?;
        let third = ema.update(
            state.child(2),
            BarTick::close_only(second.unwrap_or(f64::NAN)),
            &Self::ema_params(period),
        )?;
        // Own `last` slot carries the previous third-pass value for the
        // one-bar shift; the smoothing windows live in the child states.
        let prev = state.last();
        let raw = match (third, prev) {
            (Some(current), Some(prev)) => Self::ratio_of(current, prev),
            _ => f64::NAN,
        };
        state.set_last(third.unwrap_or(f64::NAN));
        Ok(fin_or_none(raw))
    }
}

/// Aroon (`aroon` primary; `down` secondary), parameterized by `length`.
/// Each leg is `1 - argrev / n` where `argrev` is the distance back to the
/// most recent window extreme — ties resolve to the most recent bar, matching
/// the reference's reversed argmax. A `NaN` anywhere in the window refuses
/// the leg (the reference nulls those positions); otherwise every warmed leg
/// lands on the `(0, 1]` grid.
pub struct AroonIndicator {
    spec: IndicatorSpec,
}

impl AroonIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "aroon";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Aroon".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    /// Distance back to the most recent window maximum (`find_max`) or
    /// minimum; `None` when the window holds a `NaN`.
    fn argrev(window: &[f64], find_max: bool) -> Option<usize> {
        if window.iter().any(|v| v.is_nan()) {
            return None;
        }
        let mut best = if find_max {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
        let mut index = 0;
        for (j, &v) in window.iter().enumerate() {
            let takes = if find_max { v >= best } else { v <= best };
            if takes {
                best = v;
                index = j;
            }
        }
        Some(window.len() - 1 - index)
    }

    fn leg(window: &[f64], find_max: bool) -> Option<f64> {
        Self::argrev(window, find_max)
            .map(|back| (window.len() - back) as f64 / window.len() as f64)
    }
}

impl Default for AroonIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for AroonIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let mut up = crate::math::warmup_none(data.len());
        let mut down = crate::math::warmup_none(data.len());
        if period <= data.len() {
            for i in (period - 1)..data.len() {
                let window_h = &high[i + 1 - period..=i];
                let window_l = &low[i + 1 - period..=i];
                up[i] = Self::leg(window_h, true);
                down[i] = Self::leg(window_l, false);
            }
        }
        Ok(IndicatorResult::new(Self::ID.to_owned(), up)
            .with_secondary(NamedSeries::new("down", down)))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(self.update_full(state, bar, params)?.primary)
    }

    fn update_full(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<FullTick, KernelError> {
        let period = Self::period(params)?;
        state.push(bar, period);
        let (up_raw, down_raw) = if state.len() < period {
            (f64::NAN, f64::NAN)
        } else {
            let (highs, lows) = state.scratch_highs_lows();
            (
                Self::leg(highs, true).unwrap_or(f64::NAN),
                Self::leg(lows, false).unwrap_or(f64::NAN),
            )
        };
        state.set_last(up_raw);
        Ok(FullTick {
            primary: fin_or_none(up_raw),
            secondary: vec![NamedValue {
                name: "down".to_owned(),
                value: fin_or_none(down_raw),
            }],
        })
    }
}

/// Average Directional Index (`adx`) with `plus_di`/`minus_di` secondaries,
/// parameterized by `length`.
///
/// Directional movement follows the reference decomposition (+DM/-DM/TR),
/// but every smoothing pass composes the shared [`RmaIndicator`] — Wilder
/// smoothing with an SMA seed — instead of hand-rolled Wilder sums, so the
/// batch and incremental paths share each recurrence exactly like the
/// `atr`/`macd` compositions. Steady-state levels match the hand-rolled
/// variant; only the first `2 * length - 1` bars differ (RMA seeds one bar
/// earlier, from the first window mean).
///
/// `NaN` in any consumed leg emits `None` at that position: directional
/// legs derive `NaN`, the RMA children propagate it, and the ratios refuse
/// zero denominators via [`div_or_none`](crate::math::div_or_none).
pub struct AdxIndicator {
    spec: IndicatorSpec,
}

impl AdxIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "adx";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Average Directional Index".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    /// Sub-pass parameters shared by batch, update, and composition: one
    /// construction site for the smoothing key.
    fn rma_params(length: usize) -> ParamSet {
        ParamSet::new().with_int("length", length as i64)
    }

    /// Raw (+DM, -DM, TR) legs for bar `i`, or `NaN` legs when the bar (or,
    /// past bar zero, its predecessor) carries a non-finite leg. Bar zero
    /// measures no movement yet reads `(0, 0, high - low)`, matching the
    /// reference seed and the `tr` kernel's first bar.
    fn legs(high: &[f64], low: &[f64], close: &[f64], i: usize) -> (f64, f64, f64) {
        if ![high[i], low[i], close[i]].iter().all(|v| v.is_finite()) {
            return (f64::NAN, f64::NAN, f64::NAN);
        }
        if i == 0 {
            return (0.0, 0.0, high[0] - low[0]);
        }
        Self::legs_pair(high[i - 1], low[i - 1], close[i - 1], high[i], low[i], close[i])
    }

    /// Raw (+DM, -DM, TR) legs for one bar against its predecessor, or `NaN`
    /// legs when either side carries a non-finite leg. Shared by the batch
    /// pairing and the incremental path so both read one rule.
    fn legs_pair(
        prev_high: f64,
        prev_low: f64,
        prev_close: f64,
        high: f64,
        low: f64,
        close: f64,
    ) -> (f64, f64, f64) {
        if ![prev_high, prev_low, prev_close, high, low, close]
            .iter()
            .all(|v| v.is_finite())
        {
            return (f64::NAN, f64::NAN, f64::NAN);
        }
        let up_move = high - prev_high;
        let down_move = prev_low - low;
        let plus = if up_move > down_move && up_move > 0.0 {
            up_move
        } else {
            0.0
        };
        let minus = if down_move > up_move && down_move > 0.0 {
            down_move
        } else {
            0.0
        };
        let range = crate::volatility::true_range(high, low, Some(prev_close));
        (plus, minus, range)
    }

    /// Directional leg from smoothed parts: `100 * part / smoothed_tr`,
    /// or `NaN` while either side is missing (zero denominators refuse via
    /// `div_or_none`, so flat markets read `None`, never `inf`).
    fn leg_of(smoothed: Option<f64>, smoothed_tr: Option<f64>) -> f64 {
        match (smoothed, smoothed_tr) {
            (Some(part), Some(total)) => {
                crate::math::div_or_none(100.0 * part, total).unwrap_or(f64::NAN)
            }
            _ => f64::NAN,
        }
    }
}

impl Default for AdxIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for AdxIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let period = Self::period(params)?;
        Ok(2 * period - 1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let n = data.len();
        let mut dm_plus = vec![f64::NAN; n];
        let mut dm_minus = vec![f64::NAN; n];
        let mut ranges = vec![f64::NAN; n];
        for i in 0..n {
            let (plus, minus, range) = Self::legs(high, low, data.close, i);
            dm_plus[i] = plus;
            dm_minus[i] = minus;
            ranges[i] = range;
        }
        let rma = RmaIndicator::new();
        let smooth = |series: &[f64]| {
            rma.batch(&Ohlcv::close_only(series), &Self::rma_params(period))
                .map(|result| result.values)
        };
        let smoothed_plus = smooth(&dm_plus)?;
        let smoothed_minus = smooth(&dm_minus)?;
        let smoothed_tr = smooth(&ranges)?;
        let mut plus_di = vec![f64::NAN; n];
        let mut minus_di = vec![f64::NAN; n];
        let mut dx = vec![f64::NAN; n];
        for i in 0..n {
            plus_di[i] = Self::leg_of(smoothed_plus[i], smoothed_tr[i]);
            minus_di[i] = Self::leg_of(smoothed_minus[i], smoothed_tr[i]);
            if plus_di[i].is_finite() && minus_di[i].is_finite() {
                dx[i] = crate::math::div_or_none(
                    100.0 * (plus_di[i] - minus_di[i]).abs(),
                    plus_di[i] + minus_di[i],
                )
                .unwrap_or(f64::NAN);
            }
        }
        let adx = rma.batch(&Ohlcv::close_only(&dx), &Self::rma_params(period))?;
        Ok(IndicatorResult::new(
            Self::ID.to_owned(),
            adx.values.clone(),
        )
        .with_secondary(NamedSeries::new(
            "plus_di",
            plus_di.iter().map(|&v| fin_or_none(v)).collect(),
        ))
        .with_secondary(NamedSeries::new(
            "minus_di",
            minus_di.iter().map(|&v| fin_or_none(v)).collect(),
        )))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(self.update_full(state, bar, params)?.primary)
    }

    fn update_full(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<FullTick, KernelError> {
        let period = Self::period(params)?;
        state.push(bar, 2);
        let (dm_plus, dm_minus, range) = if state.len() < 2 {
            Self::legs(&[bar.high], &[bar.low], &[bar.close], 0)
        } else {
            // Retained previous bar against the current tick: the same
            // pairing the batch loop reads at each index past zero. Only
            // back-indexed scalars ever enter — no window is collected.
            let back = state.len().saturating_sub(2);
            let prev_high = state.highs().nth(back).unwrap_or(f64::NAN);
            let prev_low = state.lows().nth(back).unwrap_or(f64::NAN);
            let prev_close = state.prev_close().unwrap_or(f64::NAN);
            Self::legs_pair(prev_high, prev_low, prev_close, bar.high, bar.low, bar.close)
        };
        let rma = RmaIndicator::new();
        let rma_ps = Self::rma_params(period);
        let smoothed_plus =
            rma.update(state.child(0), BarTick::close_only(dm_plus), &rma_ps)?;
        let smoothed_minus =
            rma.update(state.child(1), BarTick::close_only(dm_minus), &rma_ps)?;
        let smoothed_tr = rma.update(state.child(2), BarTick::close_only(range), &rma_ps)?;
        let plus = Self::leg_of(smoothed_plus, smoothed_tr);
        let minus = Self::leg_of(smoothed_minus, smoothed_tr);
        let dx = if plus.is_finite() && minus.is_finite() {
            crate::math::div_or_none(100.0 * (plus - minus).abs(), plus + minus)
                .unwrap_or(f64::NAN)
        } else {
            f64::NAN
        };
        let adx = rma.update(state.child(3), BarTick::close_only(dx), &rma_ps)?;
        state.set_last(adx.unwrap_or(f64::NAN));
        Ok(FullTick {
            primary: adx,
            secondary: vec![
                NamedValue {
                    name: "plus_di".to_owned(),
                    value: fin_or_none(plus),
                },
                NamedValue {
                    name: "minus_di".to_owned(),
                    value: fin_or_none(minus),
                },
            ],
        })
    }

    // No declared sub-series: every RMA pass reads a derived (intermediate)
    // series, never a frame leg, so there is no shared frame key — the
    // `macd` signal-line precedent.
}

#[cfg(all(test, feature = "research"))]
#[path = "../tests/ta_momentum.rs"]
mod tests;
