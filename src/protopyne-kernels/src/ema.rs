//! Exponential moving average over closes.
//!
//! The seed is the simple mean of the first `length` closes (the standard
//! seeding); thereafter `ema = close*k + prev*(1-k)` with `k =
//! 2/(length+1)`. Whenever the previous value is `na` (warm start or a `na`
//! bar inside the window) the kernel re-seeds from the window mean, matching
//! Pine's `na(ema[1]) ? sma : recurrence` shape. [`batch`](crate::indicator::Indicator::batch)
//! and [`update`](crate::indicator::Indicator::update) apply the identical
//! recurrence in the identical order, so the two paths agree bit-for-bit.

use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, BarTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv,
    ParamSet,
};
use crate::windowed::{self, Window, WindowLegs, Windowed};

/// Exponential moving average (`ema`), parameterized by `length`.
pub struct EmaIndicator {
    spec: IndicatorSpec,
}

impl EmaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "ema";

    /// Describe the kernel. Warmup refines per parameters via [`warmup`](crate::indicator::Indicator::warmup).
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Exponential Moving Average".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    /// Smoothing factor for a period.
    pub fn factor(period: usize) -> f64 {
        crate::math::ema_factor(period)
    }
}

impl Default for EmaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Windowed for EmaIndicator {
    type Shape = usize;

    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn shape(&self, params: &ParamSet) -> Result<Self::Shape, KernelError> {
        Self::period(params)
    }

    fn period(period: usize) -> usize {
        period
    }

    fn legs(&self) -> WindowLegs {
        WindowLegs::Close
    }

    /// Seed from the first full window, then step: a window whose previous
    /// output is `na` re-seeds from its own mean, which is Pine's
    /// `na(ema[1]) ? sma : recurrence` shape (the load-bearing divergence
    /// recorded in `compat.rs`).
    fn eval(&self, shape: usize, window: &Window<'_>, previous: Option<f64>) -> Option<f64> {
        fin_or_none(crate::math::seed_or_step(
            previous,
            window.newest(),
            crate::math::window_mean(window.closes),
            |prev, newest| crate::math::ema_step(prev, newest, Self::factor(shape)),
        ))
    }
}

impl Indicator for EmaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        Windowed::spec(self)
    }

    fn refine_warmup(&self, params: &ParamSet) -> Result<Option<usize>, KernelError> {
        Ok(Some(windowed::warmup(self, params)?))
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        windowed::batch(self, data, params)
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        windowed::update(self, state, bar, params)
    }
}

#[cfg(test)]
#[path = "../tests/ema.rs"]
mod tests;
