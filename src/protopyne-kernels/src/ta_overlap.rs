//! Overlap remainder: Bollinger-style bands, midpoint/midprice, triangular
//! average, Donchian channels, Ichimoku Cloud, and Parabolic SAR.
//!
//! `DEMA`, `TEMA`, `EMA`, `RMA`, and the `SMA`/`WMA` aliases already ship in
//! `averages`/`ema`; this module adds only what is missing. `KAMA` raises
//! unconditionally in the reference (no smoothing is defined there), so no
//! kernel is provided for it: lookup fails closed instead of inventing one.
//! Band envelopes use the shared population deviation
//! ([`std_pop`](crate::math::std_pop)); the triangular average composes two
//! [`SmaIndicator`](crate::sma::SmaIndicator) passes through child states,
//! exactly like the double-`EmaIndicator` composition behind `dema`.

use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, BarTick, FullTick, Indicator, IndicatorId, IndicatorResult, IndicatorSpec,
    IndicatorState, NamedSeries, NamedValue, Ohlcv, ParamSet,
};
use crate::sma::SmaIndicator;

/// Bollinger-style bands (`middle` primary; `upper`/`lower` secondaries),
/// parameterized by `length`, `nbdevup`, and `nbdevdn`. The envelope is the
/// shared population deviation, so symmetric settings reproduce the legacy
/// `bb` kernel bit for bit. Only SMA smoothing exists (`matype` 0): any
/// other moving-average type is refused instead of silently substituted.
pub struct BbandsIndicator {
    spec: IndicatorSpec,
}

impl BbandsIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "bbands";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Bollinger Bands".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn shape(params: &ParamSet) -> Result<(usize, f64, f64), KernelError> {
        let length = crate::math::period_len(Self::ID, params, "length")?;
        let up = params.float(Self::ID, "nbdevup")?;
        if !up.is_finite() {
            return Err(KernelError::InvalidParam(format!(
                "{}: nbdevup must be finite, got {up}",
                Self::ID
            )));
        }
        let dn = params.float(Self::ID, "nbdevdn")?;
        if !dn.is_finite() {
            return Err(KernelError::InvalidParam(format!(
                "{}: nbdevdn must be finite, got {dn}",
                Self::ID
            )));
        }
        // Absent matype defaults to SMA, matching the reference default.
        if params.float(Self::ID, "matype").unwrap_or(0.0) != 0.0 {
            return Err(KernelError::InvalidParam(format!(
                "{}: non-SMA matype unsupported, only 0 (SMA) is implemented",
                Self::ID
            )));
        }
        Ok((length, up, dn))
    }
}

impl Default for BbandsIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for BbandsIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(Self::shape(params)?.0)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let (period, up, dn) = Self::shape(params)?;
        let mut middle = vec![None; closes.len()];
        let mut upper = vec![None; closes.len()];
        let mut lower = vec![None; closes.len()];
        if period <= closes.len() {
            for i in (period - 1)..closes.len() {
                let window = &closes[i + 1 - period..=i];
                let mean = crate::math::window_mean(window);
                let dev = crate::math::std_pop(window);
                middle[i] = fin_or_none(mean);
                upper[i] = fin_or_none(mean + up * dev);
                lower[i] = fin_or_none(mean - dn * dev);
            }
        }
        Ok(IndicatorResult::new(Self::ID.to_owned(), middle)
            .with_secondary(NamedSeries::new("upper", upper))
            .with_secondary(NamedSeries::new("lower", lower)))
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
        let (period, up, dn) = Self::shape(params)?;
        state.push(bar, period);
        if state.len() < period {
            return Ok(FullTick {
                primary: None,
                secondary: vec![
                    NamedValue {
                        name: "upper".to_owned(),
                        value: None,
                    },
                    NamedValue {
                        name: "lower".to_owned(),
                        value: None,
                    },
                ],
            });
        }
        let window: Vec<f64> = state.closes().collect();
        let mean = crate::math::window_mean(&window);
        let dev = crate::math::std_pop(&window);
        state.set_last(mean);
        Ok(FullTick {
            primary: fin_or_none(mean),
            secondary: vec![
                NamedValue {
                    name: "upper".to_owned(),
                    value: fin_or_none(mean + up * dev),
                },
                NamedValue {
                    name: "lower".to_owned(),
                    value: fin_or_none(mean - dn * dev),
                },
            ],
        })
    }
}

/// Rolling extreme of one retained leg, skipping `na` values; `None` when
/// every retained value is `na`.
fn retained_extreme(
    values: impl Iterator<Item = f64>,
    identity: f64,
    fold: fn(f64, f64) -> f64,
) -> Option<f64> {
    let mut extreme = identity;
    let mut seen = false;
    for v in values {
        if !v.is_nan() {
            extreme = fold(extreme, v);
            seen = true;
        }
    }
    if seen { fin_or_none(extreme) } else { None }
}

/// Midpoint of closes (`(highest close + lowest close) / 2`), parameterized
/// by `length`.
pub struct MidpointIndicator {
    spec: IndicatorSpec,
}

impl MidpointIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "midpoint";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Midpoint".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for MidpointIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for MidpointIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let hi = crate::math::rolling_max(data.close, period);
        let lo = crate::math::rolling_min(data.close, period);
        let values = hi
            .iter()
            .zip(lo.iter())
            .map(|(h, l)| match (h, l) {
                (Some(h), Some(l)) => fin_or_none(crate::math::midpoint_of(*h, *l)),
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
        state.push(bar, period);
        if state.len() < period {
            return Ok(None);
        }
        let hi = retained_extreme(state.closes(), f64::NEG_INFINITY, f64::max);
        let lo = retained_extreme(state.closes(), f64::INFINITY, f64::min);
        let mid = match (hi, lo) {
            (Some(h), Some(l)) => crate::math::midpoint_of(h, l),
            _ => f64::NAN,
        };
        state.set_last(mid);
        Ok(fin_or_none(mid))
    }
}

/// Midprice of high/low (`(highest high + lowest low) / 2`), parameterized
/// by `length`.
pub struct MidpriceIndicator {
    spec: IndicatorSpec,
}

impl MidpriceIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "midprice";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Midprice".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for MidpriceIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for MidpriceIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let period = Self::period(params)?;
        let hi = crate::math::rolling_max(high, period);
        let lo = crate::math::rolling_min(low, period);
        let values = hi
            .iter()
            .zip(lo.iter())
            .map(|(h, l)| match (h, l) {
                (Some(h), Some(l)) => fin_or_none(crate::math::midpoint_of(*h, *l)),
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
        state.push(bar, period);
        if state.len() < period {
            return Ok(None);
        }
        let hi = retained_extreme(state.highs(), f64::NEG_INFINITY, f64::max);
        let lo = retained_extreme(state.lows(), f64::INFINITY, f64::min);
        let mid = match (hi, lo) {
            (Some(h), Some(l)) => crate::math::midpoint_of(h, l),
            _ => f64::NAN,
        };
        state.set_last(mid);
        Ok(fin_or_none(mid))
    }
}

/// Triangular moving average (`trima`), parameterized by `length`.
///
/// Two cascaded simple averages with sub-periods `ceil(n / 2)` and
/// `floor(n / 2) + 1`, so the first valid output lands at index `n - 1` and
/// warmup is the length itself. Both passes are the shared
/// [`SmaIndicator`], composed through child states exactly like the
/// double-`EmaIndicator` composition behind `dema`.
pub struct TrimaIndicator {
    spec: IndicatorSpec,
}

impl TrimaIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "trima";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Triangular Moving Average".to_owned())
                .warmup_bars(1_usize)
                .depends(vec![SmaIndicator::ID.to_owned()])
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    /// Cascaded sub-periods: `ceil(n / 2)` then `floor(n / 2) + 1`.
    fn sub_periods(period: usize) -> (usize, usize) {
        (period.div_ceil(2), period / 2 + 1)
    }

    /// Sub-pass parameters shared by batch, update, and the declared
    /// dependency edges: one construction site for the composed keys.
    fn sma_params(length: usize) -> ParamSet {
        ParamSet::new().with_int("length", length as i64)
    }
}

impl Default for TrimaIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for TrimaIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let (first_len, second_len) = Self::sub_periods(period);
        let sma = SmaIndicator::new();
        let first = sma.batch(&Ohlcv::close_only(data.close), &Self::sma_params(first_len))?;
        let first_raw: Vec<f64> =
            first.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        let second = sma.batch(&Ohlcv::close_only(&first_raw), &Self::sma_params(second_len))?;
        Ok(IndicatorResult::new(Self::ID.to_owned(), second.values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        let (first_len, second_len) = Self::sub_periods(period);
        let sma = SmaIndicator::new();
        let first = sma.update(state.child(0), bar, &Self::sma_params(first_len))?;
        let second = sma.update(
            state.child(1),
            BarTick::close_only(first.unwrap_or(f64::NAN)),
            &Self::sma_params(second_len),
        )?;
        if let Some(value) = second {
            state.set_last(value);
        }
        Ok(second)
    }

    fn deps_with_params(&self, params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        // Both sub-periods read frame legs when co-requested, so both are
        // declared; the kernel still runs its own inline cascade (the second
        // pass reads intermediate output, never the shared frame).
        match Self::period(params) {
            Ok(period) => {
                let (first_len, second_len) = Self::sub_periods(period);
                vec![
                    (SmaIndicator::ID.to_owned(), Self::sma_params(first_len)),
                    (SmaIndicator::ID.to_owned(), Self::sma_params(second_len)),
                ]
            }
            Err(_) => Vec::new(),
        }
    }
}

/// Donchian midline over the trailing `length`-bar window ending at `end`:
/// `(highest high + lowest low) / 2`, or `NaN` when the window holds a
/// non-finite leg. The `f64::max`/`min` folds would silently drop `NaN`,
/// so the window is checked first: a hole poisons its own window, never
/// its neighbors. Shared by the Donchian and Ichimoku kernels, whose legs
/// are all Donchian midlines at different lengths.
pub(crate) fn donchian_mid(high: &[f64], low: &[f64], length: usize, end: usize) -> f64 {
    let start = end + 1 - length;
    let (window_h, window_l) = (&high[start..=end], &low[start..=end]);
    if !window_h.iter().chain(window_l.iter()).all(|v| v.is_finite()) {
        return f64::NAN;
    }
    let highest = window_h.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let lowest = window_l.iter().copied().fold(f64::INFINITY, f64::min);
    f64::midpoint(highest, lowest)
}

/// Donchian channels (`donchian`): trailing `length`-bar highest high and
/// lowest low with their midpoint, parameterized by `length`. The midpoint
/// is primary; `upper`/`lower` ride as secondaries, the same shape as the
/// `bbands` envelope.
pub struct DonchianIndicator {
    spec: IndicatorSpec,
}

impl DonchianIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "donchian";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Donchian Channels".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    /// Full (upper, middle, lower) channel over the trailing window, or
    /// `NaN`s when the window is short or holds a hole. The middle proves
    /// the window finite, so the extreme folds below cannot see `NaN`.
    fn channel(high: &[f64], low: &[f64], period: usize) -> (f64, f64, f64) {
        if high.len() < period {
            return (f64::NAN, f64::NAN, f64::NAN);
        }
        let end = high.len() - 1;
        let middle = donchian_mid(high, low, period, end);
        if middle.is_nan() {
            return (f64::NAN, f64::NAN, f64::NAN);
        }
        let start = end + 1 - period;
        let upper = high[start..=end].iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let lower = low[start..=end].iter().copied().fold(f64::INFINITY, f64::min);
        (upper, middle, lower)
    }
}

impl Default for DonchianIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for DonchianIndicator {
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
        let n = data.len();
        let mut upper = vec![f64::NAN; n];
        let mut middle = vec![f64::NAN; n];
        let mut lower = vec![f64::NAN; n];
        for i in 0..n {
            if i + 1 < period {
                continue;
            }
            let (u, m, l) = Self::channel(&high[..=i], &low[..=i], period);
            upper[i] = u;
            middle[i] = m;
            lower[i] = l;
        }
        Ok(IndicatorResult::new(
            Self::ID.to_owned(),
            middle.iter().map(|&v| fin_or_none(v)).collect(),
        )
        .with_secondary(NamedSeries::new(
            "upper",
            upper.iter().map(|&v| fin_or_none(v)).collect(),
        ))
        .with_secondary(NamedSeries::new(
            "lower",
            lower.iter().map(|&v| fin_or_none(v)).collect(),
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
        state.push(bar, period);
        // Retained legs run oldest-first, so the whole retention is the
        // trailing window the batch slices — one shared `channel` rule.
        let high: Vec<f64> = state.highs().collect();
        let low: Vec<f64> = state.lows().collect();
        let (u, m, l) = Self::channel(&high, &low, period);
        state.set_last(m);
        Ok(FullTick {
            primary: fin_or_none(m),
            secondary: vec![
                NamedValue {
                    name: "upper".to_owned(),
                    value: fin_or_none(u),
                },
                NamedValue {
                    name: "lower".to_owned(),
                    value: fin_or_none(l),
                },
            ],
        })
    }
}

/// Ichimoku Cloud (`ichimoku`): tenkan-sen, kijun-sen, senkou spans A/B,
/// and chikou span, parameterized by `tenkan_length`, `kijun_length`, and
/// `senkou_b_length`. Every leg but chikou is a [`donchian_mid`] midline;
/// senkou A is the midpoint of tenkan and kijun. Spans publish undisplaced
/// (aligned with their computing bar, like the reference data model):
/// forward/backward displacement is display-side.
///
/// Tenkan-sen is primary; `kijun`, `senkou_a`, `senkou_b`, and `chikou`
/// ride as secondaries.
pub struct IchimokuIndicator {
    spec: IndicatorSpec,
}

impl IchimokuIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "ichimoku";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Ichimoku Cloud".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the three lengths shared by batch and update paths.
    pub fn lengths(params: &ParamSet) -> Result<(usize, usize, usize), KernelError> {
        let each = |name: &str| crate::math::period_len(Self::ID, params, name);
        Ok((
            each("tenkan_length")?,
            each("kijun_length")?,
            each("senkou_b_length")?,
        ))
    }

    /// One leg over retained legs, or `NaN` while the retention is short.
    /// The retention runs oldest-first, so its tail is the trailing window.
    fn leg(retained: &[f64], other: &[f64], period: usize) -> f64 {
        if retained.len() < period {
            return f64::NAN;
        }
        donchian_mid(retained, other, period, retained.len() - 1)
    }
}

impl Default for IchimokuIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for IchimokuIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let (tenkan, kijun, senkou_b) = Self::lengths(params)?;
        Ok(tenkan.max(kijun).max(senkou_b))
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (tenkan_len, kijun_len, senkou_b_len) = Self::lengths(params)?;
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let n = data.len();
        let mut tenkan = vec![f64::NAN; n];
        let mut kijun = vec![f64::NAN; n];
        let mut senkou_a = vec![f64::NAN; n];
        let mut senkou_b = vec![f64::NAN; n];
        for i in 0..n {
            if i + 1 >= tenkan_len {
                tenkan[i] = donchian_mid(high, low, tenkan_len, i);
            }
            if i + 1 >= kijun_len {
                kijun[i] = donchian_mid(high, low, kijun_len, i);
            }
            if tenkan[i].is_finite() && kijun[i].is_finite() {
                senkou_a[i] = f64::midpoint(tenkan[i], kijun[i]);
            }
            if i + 1 >= senkou_b_len {
                senkou_b[i] = donchian_mid(high, low, senkou_b_len, i);
            }
        }
        let fin = |series: &[f64]| series.iter().map(|&v| fin_or_none(v)).collect::<Vec<_>>();
        Ok(IndicatorResult::new(Self::ID.to_owned(), fin(&tenkan))
            .with_secondary(NamedSeries::new("kijun", fin(&kijun)))
            .with_secondary(NamedSeries::new("senkou_a", fin(&senkou_a)))
            .with_secondary(NamedSeries::new("senkou_b", fin(&senkou_b)))
            .with_secondary(NamedSeries::new(
                "chikou",
                data.close.iter().map(|&v| fin_or_none(v)).collect(),
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
        let (tenkan_len, kijun_len, senkou_b_len) = Self::lengths(params)?;
        let span = tenkan_len.max(kijun_len).max(senkou_b_len);
        state.push(bar, span);
        let high: Vec<f64> = state.highs().collect();
        let low: Vec<f64> = state.lows().collect();
        let tenkan = Self::leg(&high, &low, tenkan_len);
        let kijun = Self::leg(&high, &low, kijun_len);
        let senkou_a = if tenkan.is_finite() && kijun.is_finite() {
            f64::midpoint(tenkan, kijun)
        } else {
            f64::NAN
        };
        let senkou_b = Self::leg(&high, &low, senkou_b_len);
        state.set_last(tenkan);
        Ok(FullTick {
            primary: fin_or_none(tenkan),
            secondary: vec![
                NamedValue {
                    name: "kijun".to_owned(),
                    value: fin_or_none(kijun),
                },
                NamedValue {
                    name: "senkou_a".to_owned(),
                    value: fin_or_none(senkou_a),
                },
                NamedValue {
                    name: "senkou_b".to_owned(),
                    value: fin_or_none(senkou_b),
                },
                NamedValue {
                    name: "chikou".to_owned(),
                    value: fin_or_none(bar.close),
                },
            ],
        })
    }
}

/// Parabolic SAR (`sar`): Wilder's trailing stop-and-reverse, parameterized
/// by `start`, `increment`, and `maximum` acceleration. The SAR rides below
/// price in uptrends and above it in downtrends; a close through the SAR
/// flips the trend and restarts acceleration.
///
/// The batch loop and the incremental path share [`PsarFrame::seed`] and
/// [`PsarFrame::step`] verbatim, so the two agree bit for bit: both seed
/// from the first two clean bars and both read reversal clamps off the last
/// two clean bars. A hole therefore pauses the trail (state held, `None`
/// emitted) instead of poisoning it — the only deliberate deviation from
/// the hole-free reference transcription.
pub struct PsarIndicator {
    spec: IndicatorSpec,
}

/// One Wilder trail frame: the stop level, its trend sign, the extreme
/// point, and the current acceleration factor.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PsarFrame {
    sar: f64,
    trend: f64,
    ep: f64,
    af: f64,
}

impl PsarFrame {
    /// Seed from the first two clean bars: trend follows the close, the
    /// stop starts at the prior extreme, and acceleration restarts.
    fn seed(prev: (f64, f64, f64), cur: (f64, f64, f64), start: f64) -> Self {
        let (prev_high, prev_low, prev_close) = prev;
        let (cur_high, cur_low, cur_close) = cur;
        if cur_close >= prev_close {
            Self {
                sar: prev_low,
                trend: 1.0,
                ep: cur_high.max(prev_high),
                af: start,
            }
        } else {
            Self {
                sar: prev_high,
                trend: -1.0,
                ep: cur_low.min(prev_low),
                af: start,
            }
        }
    }

    /// Advance across one clean bar. `newer`/`older` are the prior two
    /// clean bars' clamping extremes (lows in an uptrend, highs in a
    /// downtrend): the stop never crosses them, exactly like the reference
    /// two-bar clamp.
    fn step(&self, high: f64, low: f64, newer: f64, older: f64, shape: (f64, f64, f64)) -> Self {
        let (start, increment, maximum) = shape;
        let mut next_sar = self.sar + self.af * (self.ep - self.sar);
        if self.trend > 0.0 {
            next_sar = next_sar.min(newer).min(older);
            if low < next_sar {
                return Self {
                    sar: self.ep,
                    trend: -1.0,
                    ep: low,
                    af: start,
                };
            }
            let (ep, af) = if high > self.ep {
                (high, (self.af + increment).min(maximum))
            } else {
                (self.ep, self.af)
            };
            Self {
                sar: next_sar,
                trend: 1.0,
                ep,
                af,
            }
        } else {
            next_sar = next_sar.max(newer).max(older);
            if high > next_sar {
                return Self {
                    sar: self.ep,
                    trend: 1.0,
                    ep: high,
                    af: start,
                };
            }
            let (ep, af) = if low < self.ep {
                (low, (self.af + increment).min(maximum))
            } else {
                (self.ep, self.af)
            };
            Self {
                sar: next_sar,
                trend: -1.0,
                ep,
                af,
            }
        }
    }
}

impl PsarIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "sar";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Parabolic SAR".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the acceleration triple shared by batch and update paths.
    pub fn shape(params: &ParamSet) -> Result<(f64, f64, f64), KernelError> {
        let num = |name: &str| params.float(Self::ID, name);
        let (start, increment, maximum) = (num("start")?, num("increment")?, num("maximum")?);
        if !start.is_finite() || start <= 0.0 {
            return Err(KernelError::InvalidParam(format!(
                "sar: start must be positive and finite, got {start}"
            )));
        }
        if !increment.is_finite() || increment <= 0.0 {
            return Err(KernelError::InvalidParam(format!(
                "sar: increment must be positive and finite, got {increment}"
            )));
        }
        if !maximum.is_finite() || maximum < start {
            return Err(KernelError::InvalidParam(format!(
                "sar: maximum must be finite and at least start ({start}), got {maximum}"
            )));
        }
        Ok((start, increment, maximum))
    }

    /// Clamping extremes for a step: the prior two clean bars' lows in an
    /// uptrend, highs in a downtrend.
    fn clamps(frame: &PsarFrame, newer: (f64, f64, f64), older: (f64, f64, f64)) -> (f64, f64) {
        if frame.trend > 0.0 {
            (newer.1, older.1)
        } else {
            (newer.0, older.0)
        }
    }
}

impl Default for PsarIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for PsarIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let _ = Self::shape(params)?;
        Ok(2)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let shape = Self::shape(params)?;
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let n = data.len();
        let mut sar = vec![None; n];
        let mut trend = vec![None; n];
        let mut frame: Option<PsarFrame> = None;
        // Last three clean bars as (high, low, close): seeding reads the
        // older two, stepping reads the prior two as clamps.
        let mut clean: std::collections::VecDeque<(f64, f64, f64)> =
            std::collections::VecDeque::new();
        for i in 0..n {
            let bar = (high[i], low[i], data.close[i]);
            if ![bar.0, bar.1, bar.2].iter().all(|v| v.is_finite()) {
                continue;
            }
            clean.push_back(bar);
            while clean.len() > 3 {
                clean.pop_front();
            }
            let Some(next) = (match frame {
                None => {
                    if clean.len() < 2 {
                        None
                    } else {
                        let prev = clean[clean.len() - 2];
                        Some(PsarFrame::seed(prev, bar, shape.0))
                    }
                }
                Some(prev) => {
                    let newer = clean[clean.len() - 2];
                    let older = clean[clean.len() - 3];
                    let (clamp_newer, clamp_older) = Self::clamps(&prev, newer, older);
                    Some(prev.step(bar.0, bar.1, clamp_newer, clamp_older, shape))
                }
            }) else {
                continue;
            };
            frame = Some(next);
            sar[i] = Some(next.sar);
            trend[i] = Some(next.trend);
        }
        Ok(IndicatorResult::new(Self::ID.to_owned(), sar)
            .with_secondary(NamedSeries::new("trend", trend)))
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
        let shape = Self::shape(params)?;
        if ![bar.high, bar.low, bar.close].iter().all(|v| v.is_finite()) {
            // Hole: hold every child untouched and emit nothing.
            return Ok(FullTick {
                primary: None,
                secondary: vec![NamedValue {
                    name: "trend".to_owned(),
                    value: None,
                }],
            });
        }
        // Retention holds the last three clean bars; holes never enter, so
        // the deque matches the batch loop's position by position. Leg
        // iterators run oldest-first in one shared order.
        state.push(bar, 3);
        let highs: Vec<f64> = state.highs().collect();
        let lows: Vec<f64> = state.lows().collect();
        let closes: Vec<f64> = state.closes().collect();
        let at = |index: usize| (highs[index], lows[index], closes[index]);
        let seeded = matches!(state.child(0).last(), Some(sar) if sar.is_finite());
        let next = if seeded {
            let frame = PsarFrame {
                sar: state.child(0).last().unwrap_or(f64::NAN),
                trend: state.child(3).last().unwrap_or(1.0),
                ep: state.child(1).last().unwrap_or(f64::NAN),
                af: state.child(2).last().unwrap_or(shape.0),
            };
            let newer = at(state.len() - 2);
            let older = at(state.len() - 3);
            let (clamp_newer, clamp_older) = Self::clamps(&frame, newer, older);
            frame.step(bar.high, bar.low, clamp_newer, clamp_older, shape)
        } else if state.len() < 2 {
            return Ok(FullTick {
                primary: None,
                secondary: vec![NamedValue {
                    name: "trend".to_owned(),
                    value: None,
                }],
            });
        } else {
            PsarFrame::seed(at(state.len() - 2), (bar.high, bar.low, bar.close), shape.0)
        };
        state.child(0).set_last(next.sar);
        state.child(1).set_last(next.ep);
        state.child(2).set_last(next.af);
        state.child(3).set_last(next.trend);
        state.set_last(next.sar);
        Ok(FullTick {
            primary: Some(next.sar),
            secondary: vec![NamedValue {
                name: "trend".to_owned(),
                value: Some(next.trend),
            }],
        })
    }
}

#[cfg(all(test, feature = "research"))]
#[path = "../tests/ta_overlap.rs"]
mod tests;
