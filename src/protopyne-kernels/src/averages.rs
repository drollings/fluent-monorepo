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

use crate::composed::{self, Chained};
use crate::ema::EmaIndicator;
use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, BarTick, Indicator, IndicatorId, IndicatorResult, IndicatorSpec, IndicatorState,
    Ohlcv, ParamSet,
};
use crate::windowed::{self, Window, WindowLegs, Windowed};

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

impl Windowed for RmaIndicator {
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

    /// DIVERGENCE (see `compat.rs:11`): seed-then-recurse order is
    /// load-bearing; seeding from the first full window before recursing
    /// preserves Expr parity.
    fn eval(&self, shape: usize, window: &Window<'_>, previous: Option<f64>) -> Option<f64> {
        match previous {
            Some(previous) if !previous.is_nan() => {
                fin_or_none(crate::math::wilder_step(previous, window.newest(), shape))
            }
            _ => fin_or_none(crate::math::window_mean(window.closes)),
        }
    }
}

impl Indicator for RmaIndicator {
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

impl Windowed for WmaIndicator {
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
        fin_or_none(Self::of_window(window.closes))
    }
}

impl Indicator for WmaIndicator {
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

    fn refine_warmup(&self, params: &ParamSet) -> Result<Option<usize>, KernelError> {
        let (_, full, root) = Self::periods(params)?;
        Ok(Some(full + root - 1))
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (half_len, full_len, root_len) = Self::periods(params)?;
        let wma = WmaIndicator::new();
        let half = wma.batch(data, &Self::wma_params(half_len))?;
        let full = wma.batch(data, &Self::wma_params(full_len))?;
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
        let raw = wma.batch(&diff_data, &Self::wma_params(root_len))?;
        Ok(IndicatorResult::new(Self::ID.to_owned(), raw.values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let (half_len, full_len, root_len) = state.resolved(|| Self::periods(params))?;
        let wma = WmaIndicator::shared();
        let (child, child_params) = state.child_with_params(0, || Self::wma_params(half_len));
        let half = wma.update(child, bar, child_params)?;
        let (child, child_params) = state.child_with_params(1, || Self::wma_params(full_len));
        let full = wma.update(child, bar, child_params)?;
        let diff = match (half, full) {
            (Some(h), Some(f)) => 2.0 * h - f,
            _ => f64::NAN,
        };
        let (child, child_params) = state.child_with_params(2, || Self::wma_params(root_len));
        wma.update(child, BarTick::close_only(diff), child_params)
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

impl Windowed for VwmaIndicator {
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
        WindowLegs::CloseVolume
    }

    fn eval(&self, _shape: usize, window: &Window<'_>, _previous: Option<f64>) -> Option<f64> {
        let (numerator, denominator) = window.closes.iter().zip(window.volumes).fold(
            (0.0, 0.0),
            |(numerator, denominator), (&close, &volume)| {
                (numerator + close * volume, denominator + volume)
            },
        );
        fin_or_none(numerator / denominator)
    }
}

impl Indicator for VwmaIndicator {
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

impl Windowed for AlmaIndicator {
    type Shape = (usize, f64, f64);

    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn shape(&self, params: &ParamSet) -> Result<Self::Shape, KernelError> {
        Self::shape(params)
    }

    fn period(shape: Self::Shape) -> usize {
        shape.0
    }

    fn legs(&self) -> WindowLegs {
        WindowLegs::Close
    }

    /// The weights stay a derived per-window `Vec` (computed constants, not a
    /// retained window), so the evaluation is the kernel's own `of_window` on
    /// the caller's window either way.
    fn eval(&self, shape: Self::Shape, window: &Window<'_>, _previous: Option<f64>) -> Option<f64> {
        let (period, offset, sigma) = shape;
        let weights = Self::weights(period, offset, sigma);
        fin_or_none(Self::of_window(window.closes, &weights))
    }
}

impl Indicator for AlmaIndicator {
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

impl Chained for DemaIndicator {
    type Child = EmaIndicator;
    type Shape = usize;

    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn shape(&self, params: &ParamSet) -> Result<Self::Shape, KernelError> {
        Self::period(params)
    }

    fn passes(_shape: Self::Shape) -> usize {
        2
    }

    fn pass_params(shape: Self::Shape, _pass: usize) -> ParamSet {
        Self::ema_params(shape)
    }

    fn warmup(shape: Self::Shape) -> usize {
        2 * shape - 1
    }

    fn combine(passes: &[Option<f64>]) -> Option<f64> {
        match passes {
            [Some(first), Some(second), ..] => fin_or_none(2.0 * first - second),
            _ => None,
        }
    }

    fn child() -> &'static Self::Child {
        EmaIndicator::shared()
    }

    fn child_id() -> &'static str {
        EmaIndicator::ID
    }
}

impl Indicator for DemaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        Chained::spec(self)
    }

    fn refine_warmup(&self, params: &ParamSet) -> Result<Option<usize>, KernelError> {
        Ok(Some(composed::warmup(self, params)?))
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        composed::batch(self, data, params)
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        composed::update(self, state, bar, params)
    }

    fn deps_with_params(&self, params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        composed::deps_with_params(self, params)
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

impl Chained for TemaIndicator {
    type Child = EmaIndicator;
    type Shape = usize;

    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn shape(&self, params: &ParamSet) -> Result<Self::Shape, KernelError> {
        Self::period(params)
    }

    fn passes(_shape: Self::Shape) -> usize {
        3
    }

    fn pass_params(shape: Self::Shape, _pass: usize) -> ParamSet {
        Self::ema_params(shape)
    }

    fn warmup(shape: Self::Shape) -> usize {
        3 * shape - 2
    }

    fn combine(passes: &[Option<f64>]) -> Option<f64> {
        match passes {
            [Some(first), Some(second), Some(third), ..] => {
                fin_or_none(3.0 * first - 3.0 * second + third)
            }
            _ => None,
        }
    }

    fn child() -> &'static Self::Child {
        EmaIndicator::shared()
    }

    fn child_id() -> &'static str {
        EmaIndicator::ID
    }
}

impl Indicator for TemaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        Chained::spec(self)
    }

    fn refine_warmup(&self, params: &ParamSet) -> Result<Option<usize>, KernelError> {
        Ok(Some(composed::warmup(self, params)?))
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        composed::batch(self, data, params)
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        composed::update(self, state, bar, params)
    }

    fn deps_with_params(&self, params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        composed::deps_with_params(self, params)
    }
}

#[cfg(test)]
#[path = "../tests/averages.rs"]
mod tests;
