//! Simple moving average over closes.
//!
//! Each window is summed directly (oldest to newest) rather than through a
//! rolling total, so [`batch`](crate::indicator::Indicator::batch) agrees
//! bit-for-bit with the incremental [`update`](crate::indicator::Indicator::update)
//! fold. Clarity and batch/update identity win over a rolling-sum fast path
//! at this scale.

use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, BarTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv,
    ParamSet,
};
use crate::windowed::{self, Window, WindowLegs, Windowed};

/// Simple moving average (`sma`), parameterized by `length`.
pub struct SmaIndicator {
    spec: IndicatorSpec,
}

impl SmaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "sma";

    /// Describe the kernel. Warmup refines per parameters via [`warmup`](crate::indicator::Indicator::warmup).
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Simple Moving Average".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for SmaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Windowed for SmaIndicator {
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

    fn eval(&self, _shape: usize, window: &Window<'_>, _previous: Option<f64>) -> Option<f64> {
        fin_or_none(crate::math::window_mean(window.closes))
    }
}

impl Indicator for SmaIndicator {
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
#[path = "../tests/sma.rs"]
mod tests;
