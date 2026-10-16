//! Statistic remainder: population deviation and variance plus rolling
//! high/low correlation.
//!
//! The reference defines `STDDEV`/`VAR` with `ddof=0` (population) — the same
//! denominator as the legacy band envelope, so no sample/population fork
//! exists here despite the milestone's original premise: symmetric bands
//! reproduce the shared deviation exactly, and the suite locks that identity
//! instead of a divergence. `BETA`, the `LINEARREG` family, and `TSF` raise
//! unconditionally in the reference, so no kernels are provided for them:
//! lookup fails closed instead of inventing smoothing the reference refuses
//! to define (the `KAMA` precedent). `CORREL` correlates the high and low
//! legs — the `TA-Lib CORREL(high, low)` leg convention — checked against the
//! staged moment-form reference.

use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, BarTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv,
    ParamSet,
};

/// Population standard deviation (`stddev`), parameterized by `length` and
/// the `nbdev` multiplier. Same denominator as the band kernels by
/// construction: both divide by `n`.
pub struct StddevIndicator {
    spec: IndicatorSpec,
}

impl StddevIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "stddev";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Standard Deviation".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn shape(params: &ParamSet) -> Result<(usize, f64), KernelError> {
        let length = crate::math::period_len(Self::ID, params, "length")?;
        let nbdev = params.float(Self::ID, "nbdev")?;
        if !nbdev.is_finite() {
            return Err(KernelError::InvalidParam(format!(
                "{}: nbdev must be finite, got {nbdev}",
                Self::ID
            )));
        }
        Ok((length, nbdev))
    }
}

impl Default for StddevIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for StddevIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(Self::shape(params)?.0)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let (period, nbdev) = Self::shape(params)?;
        let mut values = crate::math::warmup_none(closes.len());
        if period <= closes.len() {
            for (i, slot) in values.iter_mut().enumerate().skip(period - 1) {
                let window = &closes[i + 1 - period..=i];
                // NaN poisons the deviation through plain arithmetic, matching
                // the reference's strict full-window nulling.
                *slot = fin_or_none(crate::math::std_pop(window) * nbdev);
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
        let (period, nbdev) = Self::shape(params)?;
        state.push(bar, period);
        if state.len() < period {
            return Ok(None);
        }
        let window = state.scratch_closes();
        let raw = crate::math::std_pop(window) * nbdev;
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Population variance (`var`), parameterized by `length` and `nbdev`. Uses
/// the shared population variance directly — not a re-squared deviation —
/// so the operation order matches the reference exactly.
pub struct VarIndicator {
    spec: IndicatorSpec,
}

impl VarIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "var";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Variance".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn shape(params: &ParamSet) -> Result<(usize, f64), KernelError> {
        let length = crate::math::period_len(Self::ID, params, "length")?;
        let nbdev = params.float(Self::ID, "nbdev")?;
        if !nbdev.is_finite() {
            return Err(KernelError::InvalidParam(format!(
                "{}: nbdev must be finite, got {nbdev}",
                Self::ID
            )));
        }
        Ok((length, nbdev))
    }
}

impl Default for VarIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for VarIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(Self::shape(params)?.0)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let (period, nbdev) = Self::shape(params)?;
        let mut values = crate::math::warmup_none(closes.len());
        if period <= closes.len() {
            for (i, slot) in values.iter_mut().enumerate().skip(period - 1) {
                let window = &closes[i + 1 - period..=i];
                *slot = fin_or_none(crate::math::variance_pop(window) * nbdev);
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
        let (period, nbdev) = Self::shape(params)?;
        state.push(bar, period);
        if state.len() < period {
            return Ok(None);
        }
        let window = state.scratch_closes();
        let raw = crate::math::variance_pop(window) * nbdev;
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// Rolling high/low correlation (`correl`), parameterized by `length`.
/// Constant or `na`-bearing windows carry no direction and read `None`
/// (the shared correlation core refuses them, as the reference nulls them).
pub struct CorrelIndicator {
    spec: IndicatorSpec,
}

impl CorrelIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "correl";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("High Low Correlation".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for CorrelIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for CorrelIndicator {
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
        let mut values = crate::math::warmup_none(data.len());
        if period <= data.len() {
            for (i, slot) in values.iter_mut().enumerate().skip(period - 1) {
                *slot = crate::math::corr_pearson(
                    &high[i + 1 - period..=i],
                    &low[i + 1 - period..=i],
                );
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
        state.push(bar, period);
        if state.len() < period {
            return Ok(None);
        }
        let (highs, lows) = state.scratch_highs_lows();
        let out = crate::math::corr_pearson(highs, lows);
        state.set_last(out.unwrap_or(f64::NAN));
        Ok(out)
    }
}

#[cfg(all(test, feature = "research"))]
#[path = "../tests/ta_stat.rs"]
mod tests;
