//! Oscillators: `rsi`, `macd`, `stoch`.
//!
//! All three follow the published Pine formulations: RSI is Wilder-smoothed
//! gains over losses (`100 - 100/(1 + rma(u)/rma(d))`, `0/0` staying `na` on
//! flat markets); MACD is fast/slow EMA convergence with an EMA signal line;
//! stochastics pair the raw `%K` with its 3-bar SMA (`%D`). Composite legs
//! reuse the single-output kernels through child states, so batch and
//! incremental paths share every recurrence.

use crate::averages::RmaIndicator;
use crate::ema::EmaIndicator;
use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, nan_hole, BarTick, FullTick, Indicator, IndicatorId, IndicatorResult, IndicatorSpec,
    IndicatorState, NamedSeries, NamedValue, Ohlcv, ParamSet,
};
use crate::sma::SmaIndicator;

/// Relative strength index (`rsi`), parameterized by `length`.
pub struct RsiIndicator {
    spec: IndicatorSpec,
}

impl RsiIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "rsi";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Relative Strength Index".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    fn rsi_of(avg_gain: f64, avg_loss: f64) -> f64 {
        100.0 - 100.0 / (1.0 + avg_gain / avg_loss)
    }
}

impl Default for RsiIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for RsiIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(Self::period(params)? + 1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let period = Self::period(params)?;
        let rma = RmaIndicator::new();
        let rma_ps = ParamSet::new().with_int("length", period as i64);
        let mut gains = vec![f64::NAN; closes.len()];
        let mut losses = vec![f64::NAN; closes.len()];
        for (i, pair) in closes.windows(2).enumerate() {
            let diff = pair[1] - pair[0];
            gains[i + 1] = diff.max(0.0);
            losses[i + 1] = (-diff).max(0.0);
        }
        let gain_data = Ohlcv::close_only(&gains);
        let loss_data = Ohlcv::close_only(&losses);
        let avg_gain = rma.batch(&gain_data, &rma_ps)?;
        let avg_loss = rma.batch(&loss_data, &rma_ps)?;
        let values = avg_gain
            .values
            .iter()
            .zip(avg_loss.values.iter())
            .map(|(g, l)| match (g, l) {
                (Some(g), Some(l)) => fin_or_none(Self::rsi_of(*g, *l)),
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
        // Own history keeps only the previous close for gain/loss splits;
        // the smoothing windows live in the child states. Read the previous
        // close after pushing: before that, the second bar cannot see the
        // first.
        state.push(bar, 2);
        let gain = match state.prev_close() {
            Some(prev) => (bar.close - prev).max(0.0),
            None => f64::NAN,
        };
        let loss = match state.prev_close() {
            Some(prev) => (prev - bar.close).max(0.0),
            None => f64::NAN,
        };
        let rma = RmaIndicator::new();
        let (child, child_params) =
            state.child_with_params(0, || ParamSet::new().with_int("length", period as i64));
        let avg_gain = rma.update(child, BarTick::close_only(gain), child_params)?;
        let (child, child_params) =
            state.child_with_params(1, || ParamSet::new().with_int("length", period as i64));
        let avg_loss = rma.update(child, BarTick::close_only(loss), child_params)?;
        let raw = match (avg_gain, avg_loss) {
            (Some(g), Some(l)) => Self::rsi_of(g, l),
            _ => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// MACD (`macd` line primary; `signal` and `hist` secondaries),
/// parameterized by `fast_length`, `slow_length`, `signal_length`.
pub struct MacdIndicator {
    spec: IndicatorSpec,
}

impl MacdIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "macd";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("MACD".to_owned())
                .warmup_bars(1_usize)
                .depends(vec![EmaIndicator::ID.to_owned()])
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn lengths(params: &ParamSet) -> Result<(usize, usize, usize), KernelError> {
        let each = |name: &str| crate::math::period_len(Self::ID, params, name);
        Ok((each("fast_length")?, each("slow_length")?, each("signal_length")?))
    }

    /// Sub-pass parameters shared by batch, update, and the declared
    /// dependency edges: one construction site for the composed keys.
    fn ema_params(length: usize) -> ParamSet {
        ParamSet::new().with_int("length", length as i64)
    }
}

impl Default for MacdIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for MacdIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let (_, slow, signal) = Self::lengths(params)?;
        Ok(slow + signal - 1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (fast_len, slow_len, signal_len) = Self::lengths(params)?;
        let ema = EmaIndicator::new();
        let fast = ema.batch(data, &Self::ema_params(fast_len))?;
        let slow = ema.batch(data, &Self::ema_params(slow_len))?;
        let line: Vec<f64> = fast
            .values
            .iter()
            .zip(slow.values.iter())
            .map(|(f, s)| match (f, s) {
                (Some(f), Some(s)) => f - s,
                _ => f64::NAN,
            })
            .collect();
        let line_data = Ohlcv::close_only(&line);
        let signal = ema.batch(&line_data, &Self::ema_params(signal_len))?;
        let hist: Vec<Option<f64>> = line
            .iter()
            .zip(signal.values.iter())
            .map(|(m, s)| match (m.is_nan(), s) {
                (false, Some(s)) => fin_or_none(m - s),
                _ => None,
            })
            .collect();
        Ok(IndicatorResult::new(
            Self::ID.to_owned(),
            line.iter().map(|&v| fin_or_none(v)).collect(),
        )
        .with_secondary(NamedSeries::new("signal", signal.values))
        .with_secondary(NamedSeries::new("hist", hist)))
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
        let (fast_len, slow_len, signal_len) = Self::lengths(params)?;
        let ema = EmaIndicator::new();
        let (child, child_params) = state.child_with_params(0, || Self::ema_params(fast_len));
        let fast = ema.update(child, bar, child_params)?;
        let (child, child_params) = state.child_with_params(1, || Self::ema_params(slow_len));
        let slow = ema.update(child, bar, child_params)?;
        let line_raw = match (fast, slow) {
            (Some(f), Some(s)) => f - s,
            _ => f64::NAN,
        };
        let (child, child_params) = state.child_with_params(2, || Self::ema_params(signal_len));
        let signal = ema.update(
            child,
            BarTick::close_only(line_raw),
            child_params,
        )?;
        let signal_raw = nan_hole(signal);
        let hist_raw = if line_raw.is_nan() || signal_raw.is_nan() {
            f64::NAN
        } else {
            line_raw - signal_raw
        };
        state.set_last(line_raw);
        Ok(FullTick {
            primary: fin_or_none(line_raw),
            secondary: vec![
                NamedValue {
                    name: "signal".to_owned(),
                    value: fin_or_none(signal_raw),
                },
                NamedValue {
                    name: "hist".to_owned(),
                    value: fin_or_none(hist_raw),
                },
            ],
        })
    }

    fn deps_with_params(&self, params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        // Fast/slow legs read frame legs when co-requested; the signal line
        // smooths the intermediate MACD line and stays inline.
        match Self::lengths(params) {
            Ok((fast_len, slow_len, _)) => vec![
                (EmaIndicator::ID.to_owned(), Self::ema_params(fast_len)),
                (EmaIndicator::ID.to_owned(), Self::ema_params(slow_len)),
            ],
            Err(_) => Vec::new(),
        }
    }
}

/// Stochastics (`k` primary, `d` secondary), parameterized by `length`.
/// `%D` is the 3-bar SMA of `%K`, matching Pine's fixed smoothing.
pub struct StochIndicator {
    spec: IndicatorSpec,
}

impl StochIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "stoch";

    /// Pine's fixed `%D` smoothing length.
    pub const SMOOTH: usize = 3;

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Stochastics".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    // No declared sub-series: `%D` smooths the intermediate `%K` series, not
    // a frame leg, so there is no frame-computable key to share.

    fn k_of(close: f64, lowest: f64, highest: f64) -> f64 {
        // Exact zero-range detection: a flat window is 0/0 (na), while any
        // nonzero range divides normally. An epsilon here would misclassify
        // microscopically gapped markets as flat.
        #[allow(clippy::float_cmp)]
        if highest == lowest {
            f64::NAN
        } else {
            100.0 * (close - lowest) / (highest - lowest)
        }
    }
}

impl Default for StochIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for StochIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(Self::period(params)? + Self::SMOOTH - 1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let mut k = vec![f64::NAN; data.len()];
        if period <= data.len() {
            for (i, slot) in k.iter_mut().enumerate().skip(period - 1) {
                let window_h = &high[i + 1 - period..=i];
                let window_l = &low[i + 1 - period..=i];
                let highest = window_h.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let lowest = window_l.iter().copied().fold(f64::INFINITY, f64::min);
                *slot = Self::k_of(data.close[i], lowest, highest);
            }
        }
        let sma = SmaIndicator::new();
        let smooth_ps = ParamSet::new().with_int("length", Self::SMOOTH as i64);
        let d = sma.batch(&Ohlcv::close_only(&k), &smooth_ps)?;
        Ok(
            IndicatorResult::new(Self::ID.to_owned(), k.iter().map(|&v| fin_or_none(v)).collect())
                .with_secondary(NamedSeries::new("d", d.values)),
        )
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
        let k_raw = if state.len() < period {
            f64::NAN
        } else {
            let highest = state.highs().fold(f64::NEG_INFINITY, f64::max);
            let lowest = state.lows().fold(f64::INFINITY, f64::min);
            Self::k_of(bar.close, lowest, highest)
        };
        let sma = SmaIndicator::new();
        let (child, child_params) = state
            .child_with_params(0, || ParamSet::new().with_int("length", Self::SMOOTH as i64));
        let d = sma.update(child, BarTick::close_only(k_raw), child_params)?;
        state.set_last(k_raw);
        Ok(FullTick {
            primary: fin_or_none(k_raw),
            secondary: vec![NamedValue {
                name: "d".to_owned(),
                value: d,
            }],
        })
    }
}

/// Commodity Channel Index (`cci`): `(src - sma) / (0.015 * mean_deviation)`
/// over `length` source bars. A flat window is `0/0` (`na`); the constant
/// `0.015` is Pine's scaling, transcribed literally.
pub struct CciIndicator {
    spec: IndicatorSpec,
}

impl CciIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "cci";

    /// Pine's deviation scaling.
    pub const SCALE: f64 = 0.015;

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Commodity Channel Index".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    fn cci_of(window: &[f64]) -> f64 {
        let mean = crate::math::window_mean(window);
        let deviation =
            window.iter().map(|v| (v - mean).abs()).sum::<f64>() / window.len() as f64;
        nan_hole(crate::math::div_or_none(
            window[window.len() - 1] - mean,
            Self::SCALE * deviation
        ))
    }
}

impl Default for CciIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for CciIndicator {
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
                *value = fin_or_none(Self::cci_of(&closes[i + 1 - period..=i]));
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
        let out = Self::cci_of(window);
        state.set_last(out);
        Ok(fin_or_none(out))
    }
}

/// Chande Momentum Oscillator (`cmo`): `100 * (up - dn) / (up + dn)` where
/// `up`/`dn` sum gains/losses over `length` changes (hence `length + 1`
/// source bars). A flat window is `0/0` (`na`).
pub struct CmoIndicator {
    spec: IndicatorSpec,
}

impl CmoIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "cmo";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Chande Momentum Oscillator".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    fn cmo_of(window: &[f64]) -> f64 {
        let (up, dn) = window.windows(2).fold((0.0, 0.0), |(up, dn), pair| {
            let change = pair[1] - pair[0];
            (up + change.max(0.0), dn + (-change).max(0.0))
        });
        nan_hole(crate::math::div_or_none(100.0 * (up - dn), up + dn))
    }
}

impl Default for CmoIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for CmoIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(Self::period(params)? + 1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let closes = data.close;
        let period = Self::period(params)?;
        let mut values = crate::math::warmup_none(closes.len());
        if period < closes.len() {
            for (i, value) in values.iter_mut().enumerate().skip(period) {
                *value = fin_or_none(Self::cmo_of(&closes[i - period..=i]));
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
        state.push(bar, period + 1);
        if state.len() < period + 1 {
            return Ok(None);
        }
        let window = state.scratch_closes();
        let out = Self::cmo_of(window);
        state.set_last(out);
        Ok(fin_or_none(out))
    }
}

#[cfg(test)]
#[path = "../tests/oscillators.rs"]
mod tests;
