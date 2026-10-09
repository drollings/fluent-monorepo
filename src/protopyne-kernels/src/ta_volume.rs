//! Volume remainder: accumulation/distribution, its oscillator, and
//! on-balance volume.
//!
//! These are the first kernels reading the `volume` leg beyond `vwma`: every
//! batch path enforces [`Ohlcv::require`](crate::indicator::Ohlcv::require)
//! alignment, so missing or short legs refuse instead of miscomputing. Two
//! reference behaviors are pinned by test, not assumed:
//!
//! * Flat ranges and zero volumes contribute a defined zero flow, never
//!   `na` (the reference's `otherwise(0)` multiplier times the volume).
//! * A `NaN` in any consumed leg emits `None` at that position without
//!   advancing the accumulator — the kernel convention for data the
//!   null-free reference frame never shows.

use crate::ema::EmaIndicator;
use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, BarTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv,
    ParamSet,
};

/// Accumulation/distribution (`ad`): cumulative money-flow-volume. Defined
/// from bar zero on clean data (`warmup` 1); takes no parameters.
pub struct AdIndicator {
    spec: IndicatorSpec,
}

impl AdIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "ad";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Accumulation Distribution".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Money-flow multiplier: signed close position inside the range; an
    /// exactly flat range reads the defined zero. Exact comparison, like the
    /// stochastic kernels: microscopically gapped markets must not read as
    /// flat.
    fn multiplier(close: f64, high: f64, low: f64) -> f64 {
        #[allow(clippy::float_cmp)]
        if high == low {
            0.0
        } else {
            ((close - low) - (high - close)) / (high - low)
        }
    }

    /// One bar's flow, or `None` when any consumed leg is missing (the
    /// accumulator does not advance on missing bars).
    fn flow(tick: BarTick) -> Option<f64> {
        let BarTick {
            high,
            low,
            close,
            volume,
            ..
        } = tick;
        if high.is_nan() || low.is_nan() || close.is_nan() || volume.is_nan() {
            return None;
        }
        Some(Self::multiplier(close, high, low) * volume)
    }
}

impl Default for AdIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for AdIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (high, low, volume) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
            data.require("volume", data.volume)?,
        );
        let mut running = 0.0;
        let values = data
            .close
            .iter()
            .enumerate()
            .map(|(i, &close)| {
                let flow = Self::flow(BarTick {
                    open: f64::NAN,
                    high: high[i],
                    low: low[i],
                    close,
                    volume: volume[i],
                })?;
                running += flow;
                fin_or_none(running)
            })
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        let Some(flow) = Self::flow(bar) else {
            // Missing bar: report `None` and leave the running total alone
            // so the stream resumes exactly where it paused.
            return Ok(None);
        };
        // Own `last` slot carries the running total (0 before the first
        // clean bar); no bar history is needed.
        let running = state.last().unwrap_or(0.0) + flow;
        state.set_last(running);
        Ok(fin_or_none(running))
    }
}

/// Chaikin-style oscillator (`adosc`): fast minus slow EMA of the `ad`
/// line, parameterized by `fast_length`/`slow_length`. The `ad` leg is the
/// shared [`AdIndicator`] itself, composed through a child state exactly
/// like the `macd` line composition.
pub struct AdoscIndicator {
    spec: IndicatorSpec,
}

impl AdoscIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "adosc";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Accumulation Distribution Oscillator".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate parameters shared by batch and update paths.
    pub fn lengths(params: &ParamSet) -> Result<(usize, usize), KernelError> {
        Ok((
            crate::math::period_len(Self::ID, params, "fast_length")?,
            crate::math::period_len(Self::ID, params, "slow_length")?,
        ))
    }
}

impl Default for AdoscIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for AdoscIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        let (fast, slow) = Self::lengths(params)?;
        Ok(fast.max(slow))
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (fast_len, slow_len) = Self::lengths(params)?;
        let ad = AdIndicator::new();
        let line = ad.batch(data, &ParamSet::new())?;
        // The AD leg is gap-free on clean data but NaN-poisoned in general:
        // feed the smoothers the raw series so batch/update share the shape.
        let line_raw: Vec<f64> = line.values.iter().map(|v| v.unwrap_or(f64::NAN)).collect();
        let ema = EmaIndicator::new();
        let fast = ema
            .batch(
                &Ohlcv::close_only(&line_raw),
                &ParamSet::new().with_int("length", fast_len as i64),
            )?
            .values;
        let slow = ema
            .batch(
                &Ohlcv::close_only(&line_raw),
                &ParamSet::new().with_int("length", slow_len as i64),
            )?
            .values;
        let values = fast
            .iter()
            .zip(slow.iter())
            .map(|(f, s)| match (f, s) {
                (Some(f), Some(s)) => fin_or_none(f - s),
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
        let (fast_len, slow_len) = Self::lengths(params)?;
        let ad = AdIndicator::new();
        let line = ad.update(state.child(0), bar, &ParamSet::new())?;
        let ema = EmaIndicator::new();
        let fast = ema.update(
            state.child(1),
            BarTick::close_only(line.unwrap_or(f64::NAN)),
            &ParamSet::new().with_int("length", fast_len as i64),
        )?;
        let slow = ema.update(
            state.child(2),
            BarTick::close_only(line.unwrap_or(f64::NAN)),
            &ParamSet::new().with_int("length", slow_len as i64),
        )?;
        let raw = match (fast, slow) {
            (Some(f), Some(s)) => f - s,
            _ => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }
}

/// On-balance volume (`obv`): cumulative signed volume, with the opening
/// print counting long (the reference's `fill_null(1)`) and flat bars
/// contributing nothing. Defined from bar zero (`warmup` 1); takes no
/// parameters.
pub struct ObvIndicator {
    spec: IndicatorSpec,
}

impl ObvIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "obv";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("On Balance Volume".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// One bar's signed volume against its predecessor (`None` for the
    /// opener is the caller's cue to count long). Missing legs emit `None`
    /// without advancing the accumulator. An exactly flat bar contributes
    /// zero: `f64::signum` reads `+0.0` as positive, so the zero arm is
    /// explicit rather than delegated (matching the reference's `sign`).
    #[allow(clippy::float_cmp)]
    fn flow(close: f64, prev: Option<f64>, volume: f64) -> Option<f64> {
        if close.is_nan() || volume.is_nan() {
            return None;
        }
        let sign = match prev {
            None => 1.0,
            Some(prev) if prev.is_nan() => return None,
            Some(prev) => {
                let diff = close - prev;
                if diff == 0.0 {
                    0.0
                } else {
                    diff.signum()
                }
            }
        };
        Some(sign * volume)
    }
}

impl Default for ObvIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for ObvIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let volume = data.require("volume", data.volume)?;
        let mut running = 0.0;
        let values = data
            .close
            .iter()
            .enumerate()
            .map(|(i, &close)| {
                // The predecessor is the immediate prior bar (matching the
                // incremental path's retained window): a missing bar shadows
                // its successor too, so both paths agree positionally.
                let prev = if i == 0 { None } else { Some(data.close[i - 1]) };
                Self::flow(close, prev, volume[i])
                    .map(|flow| {
                        running += flow;
                        running
                    })
                    .and_then(fin_or_none)
            })
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        state: &mut IndicatorState,
        bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        state.push(bar, 2);
        // The opener counts long: no predecessor on the first retained bar.
        let prev = if state.len() < 2 { None } else { state.prev_close() };
        let running = match Self::flow(bar.close, prev, bar.volume) {
            Some(flow) => state.last().unwrap_or(0.0) + flow,
            None => return Ok(None),
        };
        state.set_last(running);
        Ok(fin_or_none(running))
    }
}

#[cfg(test)]
#[path = "../tests/ta_volume.rs"]
mod tests;
