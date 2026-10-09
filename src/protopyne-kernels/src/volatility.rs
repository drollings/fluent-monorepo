//! Range and band kernels: `tr`, `atr`, `bb`, `kc`.
//!
//! True range follows Pine's `na(prev_close) ? high - low : max(...)` shape;
//! ATR is Wilder smoothing over it; Bollinger Bands pair an SMA basis with a
//! population-standard-deviation envelope (dividing by `length`, not
//! `length - 1`); Keltner Channels pair the same basis with a smoothed range
//! (true range by default, high-minus-low on request).

use crate::averages::RmaIndicator;
use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, BarTick, FullTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState,
    NamedSeries, NamedValue, Ohlcv, ParamSet,
};
use crate::sma::SmaIndicator;

/// True range of one bar given the previous close, if any.
pub fn true_range(high: f64, low: f64, prev_close: Option<f64>) -> f64 {
    match prev_close {
        Some(prev) => (high - low)
            .max((high - prev).abs())
            .max((low - prev).abs()),
        None => high - low,
    }
}

/// True range (`tr`). Parameter-free; first bar is `high - low`.
pub struct TrIndicator {
    spec: IndicatorSpec,
}

impl TrIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "tr";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("True Range".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }
}

impl Default for TrIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for TrIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let mut values = Vec::with_capacity(data.len());
        for (i, (&h, &l)) in high.iter().zip(low.iter()).enumerate() {
            let prev = if i == 0 { None } else { Some(data.close[i - 1]) };
            values.push(fin_or_none(true_range(h, l, prev)));
        }
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        state.push(bar, 2);
        let prev = state.prev_close();
        let tr = true_range(bar.high, bar.low, prev);
        state.set_last(tr);
        Ok(fin_or_none(tr))
    }
}

/// Average true range (`atr`), parameterized by `length`.
pub struct AtrIndicator {
    spec: IndicatorSpec,
}

impl AtrIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "atr";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Average True Range".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for AtrIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for AtrIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let tr = TrIndicator::new();
        let ranges = tr.batch(data, &ParamSet::new())?;
        let nans: Vec<f64> = ranges
            .values
            .iter()
            .map(|v| v.unwrap_or(f64::NAN))
            .collect();
        let rma = RmaIndicator::new();
        let smoothed = rma.batch(
            &Ohlcv::close_only(&nans),
            &ParamSet::new().with_int("length", period as i64),
        )?;
        Ok(IndicatorResult::new(Self::ID.to_owned(), smoothed.values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let period = Self::period(params)?;
        state.push(bar, 2);
        let prev = state.prev_close();
        let tr = true_range(bar.high, bar.low, prev);
        let rma = RmaIndicator::new();
        let smoothed = rma.update(
            state.child(0),
            BarTick::close_only(tr),
            &ParamSet::new().with_int("length", period as i64),
        )?;
        state.set_last(smoothed.unwrap_or(f64::NAN));
        Ok(smoothed)
    }
}

/// Bollinger Bands (`basis` primary; `upper`/`lower` secondaries),
/// parameterized by `length` and `mult`. The envelope uses the shared
/// population standard deviation ([`std_pop`](crate::math::std_pop),
/// dividing by `length`); statistic kernels use the sample sibling instead —
/// the two denominators never mix.
pub struct BbIndicator {
    spec: IndicatorSpec,
}

impl BbIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "bb";

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
    pub fn shape(params: &ParamSet) -> Result<(usize, f64), KernelError> {
        let length = crate::math::period_len(Self::ID, params, "length")?;
        let mult = params.float(Self::ID, "mult")?;
        if !mult.is_finite() {
            return Err(KernelError::InvalidParam(format!(
                "{}: mult must be finite, got {mult}",
                Self::ID
            )));
        }
        Ok((length, mult))
    }
}

impl Default for BbIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for BbIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(Self::shape(params)?.0)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let (period, mult) = Self::shape(params)?;
        let mut basis = vec![None; closes.len()];
        let mut upper = vec![None; closes.len()];
        let mut lower = vec![None; closes.len()];
        if period <= closes.len() {
            for i in (period - 1)..closes.len() {
                let window = &closes[i + 1 - period..=i];
                let mean = window.iter().sum::<f64>() / period as f64;
                let dev = crate::math::std_pop(window);
                basis[i] = fin_or_none(mean);
                upper[i] = fin_or_none(mean + mult * dev);
                lower[i] = fin_or_none(mean - mult * dev);
            }
        }
        Ok(IndicatorResult::new(Self::ID.to_owned(), basis)
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
        let (period, mult) = Self::shape(params)?;
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
        let mean = window.iter().sum::<f64>() / period as f64;
        let dev = crate::math::std_pop(&window);
        state.set_last(mean);
        Ok(FullTick {
            primary: fin_or_none(mean),
            secondary: vec![
                NamedValue {
                    name: "upper".to_owned(),
                    value: fin_or_none(mean + mult * dev),
                },
                NamedValue {
                    name: "lower".to_owned(),
                    value: fin_or_none(mean - mult * dev),
                },
            ],
        })
    }
}

/// Keltner Channels (`basis` primary; `upper`/`lower` secondaries),
/// parameterized by `length`, `mult`, and `use_true_range` (default true).
pub struct KcIndicator {
    spec: IndicatorSpec,
}

impl KcIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "kc";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Keltner Channels".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn shape(params: &ParamSet) -> Result<(usize, f64, bool), KernelError> {
        let length = crate::math::period_len(Self::ID, params, "length")?;
        let mult = params.float(Self::ID, "mult")?;
        if !mult.is_finite() {
            return Err(KernelError::InvalidParam(format!(
                "{}: mult must be finite, got {mult}",
                Self::ID
            )));
        }
        let use_true_range = params.bool_or(Self::ID, "use_true_range", true)?;
        Ok((length, mult, use_true_range))
    }
}

impl Default for KcIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for KcIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        // Basis and span both need `length` bars; ranges are valid from the
        // first bar, so nothing lags behind the basis.
        Ok(Self::shape(params)?.0)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let (period, mult, use_true_range) = Self::shape(params)?;
        let sma = SmaIndicator::new();
        let len_ps = ParamSet::new().with_int("length", period as i64);
        let basis = sma.batch(data, &len_ps)?;
        let mut ranges = vec![f64::NAN; closes.len()];
        for (i, slot) in ranges.iter_mut().enumerate() {
            let prev = if i == 0 { None } else { Some(closes[i - 1]) };
            let range = if use_true_range {
                true_range(high[i], low[i], prev)
            } else {
                high[i] - low[i]
            };
            *slot = range;
        }
        let span = sma.batch(&Ohlcv::close_only(&ranges), &len_ps)?;
        let upper: Vec<Option<f64>> = basis
            .values
            .iter()
            .zip(span.values.iter())
            .map(|(b, s)| match (b, s) {
                (Some(b), Some(s)) => fin_or_none(b + mult * s),
                _ => None,
            })
            .collect();
        let lower: Vec<Option<f64>> = basis
            .values
            .iter()
            .zip(span.values.iter())
            .map(|(b, s)| match (b, s) {
                (Some(b), Some(s)) => fin_or_none(b - mult * s),
                _ => None,
            })
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), basis.values)
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
        let (period, mult, use_true_range) = Self::shape(params)?;
        state.push(bar, period);
        let prev = state.prev_close();
        let range = if use_true_range {
            true_range(bar.high, bar.low, prev)
        } else {
            bar.high - bar.low
        };
        let sma = SmaIndicator::new();
        let len_ps = ParamSet::new().with_int("length", period as i64);
        let span = sma.update(state.child(0), BarTick::close_only(range), &len_ps)?;
        let basis = if state.len() < period {
            None
        } else {
            Some(state.closes().sum::<f64>() / period as f64)
        };
        let (upper, lower) = match (basis, span) {
            (Some(b), Some(s)) => (fin_or_none(b + mult * s), fin_or_none(b - mult * s)),
            _ => (None, None),
        };
        if let Some(b) = basis {
            state.set_last(b);
        }
        Ok(FullTick {
            primary: basis.and_then(fin_or_none),
            secondary: vec![
                NamedValue {
                    name: "upper".to_owned(),
                    value: upper,
                },
                NamedValue {
                    name: "lower".to_owned(),
                    value: lower,
                },
            ],
        })
    }
}

/// Normalized average true range (`atr / close`), parameterized by `length`.
///
/// The smoothing is the shared [`AtrIndicator`]; the ratio goes through the
/// division guard, so degenerate zero prices refuse (`None`) instead of
/// fabricating a ratio, while a genuinely still market reads a confident
/// zero.
pub struct NatrIndicator {
    spec: IndicatorSpec,
}

impl NatrIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "natr";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Normalized Average True Range".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }
}

impl Default for NatrIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for NatrIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let atr = AtrIndicator::new().batch(
            data,
            &ParamSet::new().with_int("length", period as i64),
        )?;
        let values = atr
            .values
            .iter()
            .zip(data.close.iter())
            .map(|(a, &c)| a.and_then(|v| crate::math::div_or_none(v, c)))
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
        let atr = AtrIndicator::new().update(
            state.child(0),
            bar,
            &ParamSet::new().with_int("length", period as i64),
        )?;
        let ratio = atr.and_then(|v| crate::math::div_or_none(v, bar.close));
        if let Some(value) = ratio {
            state.set_last(value);
        }
        Ok(ratio)
    }
}

#[cfg(test)]
#[path = "../tests/volatility.rs"]
mod tests;