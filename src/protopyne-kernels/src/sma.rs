//! Simple moving average over closes.
//!
//! Each window is summed directly (oldest to newest) rather than through a
//! rolling total, so [`batch`](crate::indicator::Indicator::batch) agrees
//! bit-for-bit with the incremental [`update`](crate::indicator::Indicator::update)
//! fold. Clarity and batch/update identity win over a rolling-sum fast path
//! at this scale.

use crate::error::KernelError;
use crate::indicator::{
    BarTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv, ParamSet,
};

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

impl Indicator for SmaIndicator {
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
        if period <= closes.len() {
            for (i, value) in values.iter_mut().enumerate().skip(period - 1) {
                let window = &closes[i + 1 - period..=i];
                let mean = crate::math::window_mean(window);
                *value = crate::indicator::fin_or_none(mean);
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
        let window = state.scratch_closes();
        let mean = crate::math::window_mean(window);
        state.set_last(mean);
        Ok(crate::indicator::fin_or_none(mean))
    }
}

#[cfg(test)]
#[path = "../tests/sma.rs"]
mod tests;
