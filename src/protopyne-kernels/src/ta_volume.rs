//! Volume remainder: accumulation/distribution, its oscillator, Chaikin
//! money flow, and on-balance volume.
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
    fin_or_none, BarTick, Indicator, IndicatorId, IndicatorResult, IndicatorSpec, IndicatorState,
    Ohlcv, ParamSet,
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
    /// flat. Shared with the `cmf` kernel, which weights the same flow over
    /// a trailing window instead of accumulating it.
    pub(crate) fn multiplier(close: f64, high: f64, low: f64) -> f64 {
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
                .depends(vec![AdIndicator::ID.to_owned()])
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

    /// Smoother parameters shared by batch, update, and the declared
    /// dependency edges: one construction site for the composed keys.
    fn ema_params(length: usize) -> ParamSet {
        ParamSet::new().with_int("length", length as i64)
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
            .batch(&Ohlcv::close_only(&line_raw), &Self::ema_params(fast_len))?
            .values;
        let slow = ema
            .batch(&Ohlcv::close_only(&line_raw), &Self::ema_params(slow_len))?
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
            &Self::ema_params(fast_len),
        )?;
        let slow = ema.update(
            state.child(2),
            BarTick::close_only(line.unwrap_or(f64::NAN)),
            &Self::ema_params(slow_len),
        )?;
        let raw = match (fast, slow) {
            (Some(f), Some(s)) => f - s,
            _ => f64::NAN,
        };
        state.set_last(raw);
        Ok(fin_or_none(raw))
    }

    fn deps_with_params(&self, params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        // The AD line reads frame legs when co-requested; the fast/slow
        // smoothers read intermediate output and stay inline.
        match Self::lengths(params) {
            Ok(_) => vec![(AdIndicator::ID.to_owned(), ParamSet::new())],
            Err(_) => Vec::new(),
        }
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

/// Money Flow Index (`mfi`): `100 * up / (up + dn)` where `up` sums
/// `volume * src` over rising bars and `dn` over falling bars, both across
/// `length` changes (hence `length + 1` source bars). The source series
/// drives direction and weight (honoring the caller's `source` argument);
/// `volume` comes from the bar legs. A flat window is `0/0` (`na`).
pub struct MfiIndicator {
    spec: IndicatorSpec,
}

impl MfiIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "mfi";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Money Flow Index".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    fn mfi_of(prices: &[f64], volumes: &[f64]) -> f64 {
        let (up, dn) = prices
            .windows(2)
            .zip(volumes.iter().skip(1))
            .fold((0.0, 0.0), |(up, dn), (pair, volume)| {
                let flow = volume * pair[1];
                if pair[1] > pair[0] {
                    (up + flow, dn)
                } else if pair[1] < pair[0] {
                    (up, dn + flow)
                } else {
                    (up, dn)
                }
            });
        crate::math::div_or_none(100.0 * up, up + dn).unwrap_or(f64::NAN)
    }
}

impl Default for MfiIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for MfiIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Ok(Self::period(params)? + 1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let volume = data.require("volume", data.volume)?;
        let mut values = crate::math::warmup_none(data.len());
        if period < data.len() {
            for (i, value) in values.iter_mut().enumerate().skip(period) {
                *value = fin_or_none(Self::mfi_of(
                    &data.close[i - period..=i],
                    &volume[i - period..=i],
                ));
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
        let (prices, volumes) = state.scratch_closes_volumes();
        let out = Self::mfi_of(prices, volumes);
        state.set_last(out);
        Ok(fin_or_none(out))
    }
}

/// Volume-weighted average price (`vwap`): cumulative `src * volume` over
/// cumulative `volume` since the run opened. The source series drives the
/// price leg (honoring the caller's `source` argument); `volume` comes from
/// the bar legs. Bars with `na` source leave both accumulators alone, so the
/// stream resumes where it paused; zero cumulative volume is `na`.
pub struct VwapIndicator {
    spec: IndicatorSpec,
}

impl VwapIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "vwap";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Volume Weighted Average Price".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }
}

impl Default for VwapIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for VwapIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let _ = params;
        let volume = data.require("volume", data.volume)?;
        let mut price_volume = 0.0;
        let mut total_volume = 0.0;
        let values = data
            .close
            .iter()
            .zip(volume.iter())
            .map(|(price, vol)| {
                if price.is_nan() || vol.is_nan() {
                    return fin_or_none(price_volume / total_volume);
                }
                price_volume += price * vol;
                total_volume += vol;
                fin_or_none(price_volume / total_volume)
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
        // Own `last` slot carries the running price-volume total and the
        // first child slot the running volume total (both 0 at the open).
        if bar.close.is_nan() || bar.volume.is_nan() {
            let price_volume = state.last().unwrap_or(0.0);
            let total_volume = state.child(0).last().unwrap_or(0.0);
            return Ok(fin_or_none(price_volume / total_volume));
        }
        let price_volume = state.last().unwrap_or(0.0) + bar.close * bar.volume;
        let total_volume = state.child(0).last().unwrap_or(0.0) + bar.volume;
        state.set_last(price_volume);
        state.child(0).set_last(total_volume);
        Ok(fin_or_none(price_volume / total_volume))
    }
}

/// Chaikin money flow (`cmf`): trailing `length`-bar money-flow-volume
/// over trailing volume, parameterized by `length`. The per-bar flow reuses
/// the shared [`AdIndicator::multiplier`] (flat ranges read the defined
/// zero), but instead of accumulating it weights each window by its own
/// volume — accumulation pressure reads positive, distribution negative,
/// bounded by construction in `[-1, 1]` on clean data.
///
/// A zero-volume window is `0/0` (`None`), not zero: unlike the reference,
/// which divides through and prints `0`, this kernel follows the house
/// rule that flat windows are `na` (the `stoch` flat-window precedent). A
/// `NaN` in any consumed leg poisons its window to `None` without failing
/// the batch.
pub struct CmfIndicator {
    spec: IndicatorSpec,
}

impl CmfIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "cmf";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Chaikin Money Flow".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `length` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    /// One bar's money-flow-volume, or `NaN` when any consumed leg is
    /// missing. Flat ranges contribute the defined zero times the volume.
    fn flow(high: f64, low: f64, close: f64, volume: f64) -> f64 {
        if ![high, low, close, volume].iter().all(|v| v.is_finite()) {
            return f64::NAN;
        }
        AdIndicator::multiplier(close, high, low) * volume
    }

    /// Flow ratio over one trailing window: `NaN` while warming, when any
    /// window leg is missing, or when the window traded no volume.
    fn window_ratio(flows: &[f64], volumes: &[f64]) -> f64 {
        crate::math::div_or_none(flows.iter().sum(), volumes.iter().sum())
            .unwrap_or(f64::NAN)
    }
}

impl Default for CmfIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for CmfIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, params: &ParamSet) -> Result<usize, KernelError> {
        Self::period(params)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let period = Self::period(params)?;
        let (high, low, volume) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
            data.require("volume", data.volume)?,
        );
        let n = data.len();
        let flows: Vec<f64> = (0..n)
            .map(|i| Self::flow(high[i], low[i], data.close[i], volume[i]))
            .collect();
        let mut values = crate::math::warmup_none(n);
        for (i, slot) in values.iter_mut().enumerate() {
            if i + 1 < period {
                continue;
            }
            let start = i + 1 - period;
            *slot = fin_or_none(Self::window_ratio(&flows[start..=i], &volume[start..=i]));
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
        // Retained legs run oldest-first in one shared order, so zipping
        // them rebuilds exactly the trailing window the batch slices. The
        // derived flows stay a per-bar `Vec`: `window_ratio` is shared with
        // the batch path, which forbids reshaping it around the scratch
        // buffer — only the retained volumes leg rides the reuse buffer.
        let flows: Vec<f64> = state
            .highs()
            .zip(state.lows())
            .zip(state.closes())
            .zip(state.volumes())
            .map(|(((high, low), close), volume)| Self::flow(high, low, close, volume))
            .collect();
        let volumes = state.scratch_volumes();
        let ratio = Self::window_ratio(&flows, volumes);
        state.set_last(ratio);
        Ok(fin_or_none(ratio))
    }
}

#[cfg(all(test, feature = "research"))]
#[path = "../tests/ta_volume.rs"]
mod tests;
