//! Weighted and adaptive moving averages: `rma`, `wma`, `hma`, `vwma`, `alma`.
//!
//! All five follow the published Pine formulations exactly:
//!
//! - `rma`: Wilder smoothing, `alpha = 1/length`, SMA-seeded with Pine's
//!   `na(prev) ? seed : recurrence` re-seeding shape.
//! - `wma`: linearly decreasing weights with the current bar heaviest:
//!   weight `i` bars back is `(length - i) * length`.
//! - `hma`: `wma(2 * wma(length/2) - wma(length), round(sqrt(length)))`
//!   with integer halving, composed from the `wma` kernel itself so the two
//!   can never diverge.
//! - `vwma`: `sum(close * volume) / sum(volume)` over the window.
//! - `alma`: Gaussian-weighted window peaking `offset * (length - 1)` bars
//!   back with spread `length / sigma`.
//!
//! `NaN` in any leg propagates to `None`, matching Pine `na` semantics.

use crate::indicator::{
    fin_or_none, BarTick, Indicator, IndicatorId, IndicatorResult, IndicatorSpec, IndicatorState,
    Ohlcv, ParamSet,
};
use crate::ema::EmaIndicator;
use crate::error::KernelError;

/// Wilder's smoothing (`rma`), parameterized by `length`.
pub struct RmaIndicator {
    spec: IndicatorSpec,
}

impl RmaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "rma";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Wilder Smoothing".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for RmaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for RmaIndicator {
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
            let mut prev = f64::NAN;
            for (i, value) in values.iter_mut().enumerate().skip(period - 1) {
                let window = &closes[i + 1 - period..=i];
                let next = if prev.is_nan() {
                    crate::math::window_mean(window)
                } else {
                    crate::math::wilder_step(prev, closes[i], period)
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
        // The previous output copies out before the scratch fill: the window
        // slice borrows the state, so all scalar reads lead it.
        let prev = state.last();
        let window = state.scratch_closes();
        let next = match prev {
            Some(prev) if !prev.is_nan() => crate::math::wilder_step(prev, bar.close, period),
            _ => crate::math::window_mean(window),
        };
        state.set_last(next);
        Ok(fin_or_none(next))
    }
}

/// Weighted moving average (`wma`), parameterized by `length`.
pub struct WmaIndicator {
    spec: IndicatorSpec,
}

impl WmaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "wma";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Weighted Moving Average".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    /// Weighted mean of a full window, oldest first, current bar heaviest:
    /// the `i`-th oldest of `n` bars carries weight `(i + 1) * n`.
    fn of_window(window: &[f64]) -> f64 {
        let length = window.len() as f64;
        let mut norm = 0.0;
        let mut sum = 0.0;
        for (i, &value) in window.iter().enumerate() {
            let weight = (i as f64 + 1.0) * length;
            norm += weight;
            sum += value * weight;
        }
        sum / norm
    }
}

impl Default for WmaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for WmaIndicator {
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
            for (i, value) in values.iter_mut().enumerate().skip(period - 1) {
                *value = fin_or_none(Self::of_window(&closes[i + 1 - period..=i]));
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
        let mean = Self::of_window(window);
        state.set_last(mean);
        Ok(fin_or_none(mean))
    }
}

/// Hull moving average (`hma`), parameterized by `length`.
pub struct HmaIndicator {
    spec: IndicatorSpec,
}

impl HmaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "hma";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Hull Moving Average".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate `length` and derive the three composed `wma` periods
    /// (integer halving, rounded root, each at least 1).
    pub fn periods(params: &ParamSet) -> Result<(usize, usize, usize), KernelError> {
        let length = crate::math::period_len(Self::ID, params, "length")?;
        let full = length;
        let half = (full / 2).max(1);
        let root = (full as f64).sqrt().round() as usize;
        Ok((half, full, root.max(1)))
    }

    fn wma_params(length: usize) -> ParamSet {
        ParamSet::new().with_int("length", length as i64)
    }
}

impl Default for HmaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for HmaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let (_, full, root) = Self::periods(params)?;
        Ok(full + root - 1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (half_len, full_len, root_len) = Self::periods(params)?;
        let wma = WmaIndicator::new();
        let half = wma.batch(
            data,
            &Self::wma_params(half_len),
        )?;
        let full = wma.batch(
            data,
            &Self::wma_params(full_len),
        )?;
        let diff: Vec<f64> = half
            .values
            .iter()
            .zip(full.values.iter())
            .map(|(h, f)| match (h, f) {
                (Some(h), Some(f)) => 2.0 * h - f,
                _ => f64::NAN,
            })
            .collect();
        let diff_data = Ohlcv::close_only(&diff);
        let raw = wma.batch(
            &diff_data,
            &Self::wma_params(root_len),
        )?;
        Ok(IndicatorResult::new(Self::ID.to_owned(), raw.values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let (half_len, full_len, root_len) = Self::periods(params)?;
        let wma = WmaIndicator::new();
        let half_ps = Self::wma_params(half_len);
        let full_ps = Self::wma_params(full_len);
        let root_ps = Self::wma_params(root_len);
        let half = wma.update(state.child(0), bar, &half_ps)?;
        let full = wma.update(state.child(1), bar, &full_ps)?;
        let diff = match (half, full) {
            (Some(h), Some(f)) => 2.0 * h - f,
            _ => f64::NAN,
        };
        wma.update(state.child(2), BarTick::close_only(diff), &root_ps)
    }
}

/// Volume-weighted moving average (`vwma`), parameterized by `length`.
pub struct VwmaIndicator {
    spec: IndicatorSpec,
}

impl VwmaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "vwma";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Volume-Weighted Moving Average".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for VwmaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for VwmaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let volume = data.require("volume", data.volume)?;
        let period = Self::period(params)?;
        let mut values = vec![None; closes.len()];
        if period <= closes.len() {
            for (i, value) in values.iter_mut().enumerate().skip(period - 1) {
                let (num, den) = closes[i + 1 - period..=i]
                    .iter()
                    .zip(&volume[i + 1 - period..=i])
                    .fold((0.0, 0.0), |(num, den), (&c, &v)| {
                        (num + c * v, den + v)
                    });
                *value = fin_or_none(num / den);
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
        let (num, den) = state
            .closes()
            .zip(state.volumes())
            .fold((0.0, 0.0), |(num, den), (c, v)| {
                (num + c * v, den + v)
            });
        let vwma = num / den;
        state.set_last(vwma);
        Ok(fin_or_none(vwma))
    }
}

/// Arnaud Legoux moving average (`alma`), parameterized by `length`,
/// `offset` (0..=1), and `sigma`.
pub struct AlmaIndicator {
    spec: IndicatorSpec,
}

impl AlmaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "alma";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Arnaud Legoux Moving Average".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn shape(params: &ParamSet) -> Result<(usize, f64, f64), KernelError> {
        let length = crate::math::period_len(Self::ID, params, "length")?;
        let offset = params.float(Self::ID, "offset")?;
        if !(0.0..=1.0).contains(&offset) {
            return Err(KernelError::InvalidParam(format!(
                "{}: offset must be within [0, 1], got {offset}",
                Self::ID
            )));
        }
        let sigma = params.float(Self::ID, "sigma")?;
        if !sigma.is_finite() || sigma <= 0.0 {
            return Err(KernelError::InvalidParam(format!(
                "{}: sigma must be positive and finite, got {sigma}",
                Self::ID
            )));
        }
        Ok((length, offset, sigma))
    }

    /// Gaussian weights indexed by bars back from the current bar.
    fn weights(length: usize, offset: f64, sigma: f64) -> Vec<f64> {
        let center = offset * (length as f64 - 1.0);
        let spread = length as f64 / sigma;
        (0..length)
            .map(|bars_back| {
                let z = (bars_back as f64 - center) / spread;
                (-z * z / 2.0).exp()
            })
            .collect()
    }

    /// Weighted mean of a full window, oldest first.
    fn of_window(window: &[f64], weights: &[f64]) -> f64 {
        let mut norm = 0.0;
        let mut sum = 0.0;
        for (i, &value) in window.iter().enumerate() {
            let weight = weights[window.len() - 1 - i];
            norm += weight;
            sum += value * weight;
        }
        sum / norm
    }
}

impl Default for AlmaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for AlmaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(Self::shape(params)?.0)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let (period, offset, sigma) = Self::shape(params)?;
        let weights = Self::weights(period, offset, sigma);
        let mut values = vec![None; closes.len()];
        if period <= closes.len() {
            for (i, value) in values.iter_mut().enumerate().skip(period - 1) {
                *value = fin_or_none(Self::of_window(&closes[i + 1 - period..=i], &weights));
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
        let (period, offset, sigma) = Self::shape(params)?;
        state.push(bar, period);
        if state.len() < period {
            return Ok(None);
        }
        let weights = Self::weights(period, offset, sigma);
        // The weights stay a derived per-bar `Vec`: they are computed
        // constants, not a retained window, and sharing the recurrence with
        // the batch path (`of_window`) forbids reshaping it around the
        // scratch buffer. Only the window rides the reuse buffer.
        let window = state.scratch_closes();
        let alma = Self::of_window(window, &weights);
        state.set_last(alma);
        Ok(fin_or_none(alma))
    }
}

/// Double exponential moving average (`dema`), parameterized by `length`.
///
/// `2 * ema(close) - ema(ema(close))`: the second smoothing estimates the
/// first pass's lag and adds it back, so trends read with less delay than a
/// single pass. Both passes are the shared [`EmaIndicator`] itself (same
/// seeding and `na` shape), composed through child states exactly like the
/// `macd` signal line, so batch and incremental paths cannot diverge.
pub struct DemaIndicator {
    spec: IndicatorSpec,
}

impl DemaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "dema";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Double Exponential Moving Average".to_owned())
                .warmup_bars(1_usize)
                .depends(vec![EmaIndicator::ID.to_owned()])
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
}

impl Default for DemaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for DemaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let period = Self::period(params)?;
        Ok(2 * period - 1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let ema = EmaIndicator::new();
        let first = ema.batch(data, &Self::ema_params(period))?;
        let first_raw: Vec<f64> =
            first.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        let second = ema.batch(&Ohlcv::close_only(&first_raw), &Self::ema_params(period))?;
        let values = first
            .values
            .iter()
            .zip(second.values.iter())
            .map(|(one, two)| match (one, two) {
                (Some(one), Some(two)) => fin_or_none(2.0 * one - two),
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
        let period = Self::period(params)?;
        let ema = EmaIndicator::new();
        let first = ema.update(state.child(0), bar, &Self::ema_params(period))?;
        let second = ema.update(
            state.child(1),
            BarTick::close_only(first.unwrap_or(f64::NAN)),
            &Self::ema_params(period),
        )?;
        let raw = match (first, second) {
            (Some(one), Some(two)) => 2.0 * one - two,
            _ => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }

    fn deps_with_params(&self, params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        // The first pass reads frame legs when co-requested; the second pass
        // reads intermediate output and stays inline.
        match Self::period(params) {
            Ok(period) => vec![(EmaIndicator::ID.to_owned(), Self::ema_params(period))],
            Err(_) => Vec::new(),
        }
    }
}

/// Triple exponential moving average (`tema`), parameterized by `length`.
///
/// `3 * ema1 - 3 * ema2 + ema3`: the third pass removes the residual lag of
/// the second, so a steady ramp reads with no delay. All three passes are
/// the shared [`EmaIndicator`], composed through child states.
pub struct TemaIndicator {
    spec: IndicatorSpec,
}

impl TemaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "tema";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Triple Exponential Moving Average".to_owned())
                .warmup_bars(1_usize)
                .depends(vec![EmaIndicator::ID.to_owned()])
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
}

impl Default for TemaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for TemaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let period = Self::period(params)?;
        Ok(3 * period - 2)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let ema = EmaIndicator::new();
        let first = ema.batch(data, &Self::ema_params(period))?;
        let first_raw: Vec<f64> =
            first.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        let second = ema.batch(&Ohlcv::close_only(&first_raw), &Self::ema_params(period))?;
        let second_raw: Vec<f64> =
            second.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        let third = ema.batch(&Ohlcv::close_only(&second_raw), &Self::ema_params(period))?;
        let values = first
            .values
            .iter()
            .zip(second.values.iter())
            .zip(third.values.iter())
            .map(|((one, two), three)| match (one, two, three) {
                (Some(one), Some(two), Some(three)) => fin_or_none(3.0 * one - 3.0 * two + three),
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
        let raw = match (first, second, third) {
            (Some(one), Some(two), Some(three)) => 3.0 * one - 3.0 * two + three,
            _ => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }

    fn deps_with_params(&self, params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        // The first pass reads frame legs when co-requested; the second and
        // third passes read intermediate output and stay inline.
        match Self::period(params) {
            Ok(period) => vec![(EmaIndicator::ID.to_owned(), Self::ema_params(period))],
            Err(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/averages.rs"]
mod tests;
