//! Exponential moving average over closes.
//!
//! The seed is the simple mean of the first `length` closes (the standard
//! seeding); thereafter `ema = close*k + prev*(1-k)` with `k =
//! 2/(length+1)`. Whenever the previous value is `na` (warm start or a `na`
//! bar inside the window) the kernel re-seeds from the window mean, matching
//! Pine's `na(ema[1]) ? sma : recurrence` shape. [`batch`](crate::indicator::Indicator::batch)
//! and [`update`](crate::indicator::Indicator::update) apply the identical
//! recurrence in the identical order, so the two paths agree bit-for-bit.

use crate::indicator::{
    fin_or_none, BarTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv,
    ParamSet,
};
use crate::error::KernelError;

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
        let length = params.int(Self::ID, "length")?;
        if length < 1 {
            return Err(KernelError::InvalidParam(format!(
                "{}: length must be at least 1, got {length}",
                Self::ID
            )));
        }
        Ok(length as usize)
    }

    /// Smoothing factor for a period.
    pub fn factor(period: usize) -> f64 {
        2.0 / (period as f64 + 1.0)
    }
}

impl Default for EmaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for EmaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let period = Self::period(params)?;
        let mut values = vec![None; closes.len()];
        if period <= closes.len() {
            let factor = Self::factor(period);
            let mut prev = f64::NAN;
            for (i, value) in values.iter_mut().enumerate().skip(period - 1) {
                let window = &closes[i + 1 - period..=i];
                let next = if prev.is_nan() {
                    window.iter().sum::<f64>() / period as f64
                } else {
                    closes[i] * factor + prev * (1.0 - factor)
                };
                prev = next;
                *value = fin_or_none(next);
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
        let next = match state.last() {
            Some(prev) if !prev.is_nan() => {
                bar.close * Self::factor(period) + prev * (1.0 - Self::factor(period))
            }
            _ => state.closes().sum::<f64>() / period as f64,
        };
        state.set_last(next);
        Ok(fin_or_none(next))
    }
}

#[cfg(test)]
#[path = "../tests/ema.rs"]
mod tests;
