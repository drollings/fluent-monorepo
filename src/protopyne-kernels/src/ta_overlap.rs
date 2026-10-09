//! Overlap remainder: Bollinger-style bands, midpoint/midprice, triangular average.
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
    fin_or_none, BarTick, FullTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState,
    NamedSeries, NamedValue, Ohlcv, ParamSet,
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
        let first = sma.batch(
            &Ohlcv::close_only(data.close),
            &ParamSet::new().with_int("length", first_len as i64),
        )?;
        let first_raw: Vec<f64> =
            first.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        let second = sma.batch(
            &Ohlcv::close_only(&first_raw),
            &ParamSet::new().with_int("length", second_len as i64),
        )?;
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
        let first = sma.update(
            state.child(0),
            bar,
            &ParamSet::new().with_int("length", first_len as i64),
        )?;
        let second = sma.update(
            state.child(1),
            BarTick::close_only(first.unwrap_or(f64::NAN)),
            &ParamSet::new().with_int("length", second_len as i64),
        )?;
        if let Some(value) = second {
            state.set_last(value);
        }
        Ok(second)
    }
}

#[cfg(test)]
#[path = "../tests/ta_overlap.rs"]
mod tests;
