//! Adaptive and predictive filters: the Exponential Indicator Transform,
//! a scalar Kalman filter, and the Voss predictive filter.
//!
//! Each formula is transcribed literally from the Pine library under test
//! (named locals keep their Pine spellings where the transcription stays
//! readable), with `nz(x)` becoming "missing maps to the stated default"
//! and `var` state living in the [`IndicatorState`] family: own history for
//! source lookbacks, child histories for output lookbacks, child `last`
//! slots for scalar memories. Until a staged reference expression exists
//! these kernels stand at `FormulaVerified`, never `ParityVerified`.

use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, nan_hole, BarTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState,
    NamedSeries, Ohlcv, ParamSet,
};

/// Exponential Indicator Transform (`eit`): a second-order predictive
/// smoother parameterized by `alpha`. Per bar, with `s1`/`s2` the two prior
/// sources and `e1`/`e2` the two prior outputs:
/// `init = (src + 2*nz(s1, src) + nz(s2, src)) / 4`, then
/// `eit = (alpha - alpha^2/4)*src + alpha^2/2*nz(s1, src)
/// - (alpha - 3*alpha^2/4)*nz(s2, src) + 2*(1-alpha)*nz(e1, init)
/// - (1-alpha)^2*nz(e2, init)`.
pub struct EitIndicator {
    spec: IndicatorSpec,
}

impl EitIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "eit";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Exponential Indicator Transform".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `alpha` smoothing factor shared by batch and update.
    pub fn sensitivity(params: &ParamSet) -> Result<f64, KernelError> {
        let alpha = params.float(Self::ID, "alpha")?;
        if !alpha.is_finite() {
            return Err(KernelError::InvalidParam(format!(
                "{}: alpha must be finite, got {alpha}",
                Self::ID
            )));
        }
        Ok(alpha)
    }

    fn step(
        alpha: f64,
        src: f64,
        src1: Option<f64>,
        src2: Option<f64>,
        eit1: Option<f64>,
        eit2: Option<f64>,
    ) -> f64 {
        let s1 = src1.unwrap_or(src);
        let s2 = src2.unwrap_or(src);
        let init = (src + 2.0 * s1 + s2) / 4.0;
        let e1 = eit1.unwrap_or(init);
        let e2 = eit2.unwrap_or(init);
        (alpha - alpha * alpha / 4.0) * src + 0.5 * alpha * alpha * s1
            - (alpha - 0.75 * alpha * alpha) * s2
            + 2.0 * (1.0 - alpha) * e1
            - (1.0 - alpha) * (1.0 - alpha) * e2
    }
}

impl Default for EitIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for EitIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        // Recursive with `nz`-seeding: the opening print is defined, so the
        // honest warmup is 1 (cumulative-kernel precedent).
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let alpha = Self::sensitivity(params)?;
        let closes = data.close;
        let mut prior_eit: Vec<f64> = Vec::with_capacity(closes.len());
        let values = closes
            .iter()
            .enumerate()
            .map(|(i, src)| {
                let src1 = if i >= 1 { Some(closes[i - 1]) } else { None };
                let src2 = if i >= 2 { Some(closes[i - 2]) } else { None };
                let eit1 = prior_eit.last().copied();
                let eit2 = if prior_eit.len() >= 2 {
                    Some(prior_eit[prior_eit.len() - 2])
                } else {
                    None
                };
                let out = Self::step(alpha, *src, src1, src2, eit1, eit2);
                prior_eit.push(out);
                fin_or_none(out)
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
        let alpha = Self::sensitivity(params)?;
        // Own history carries the source lookbacks; the first child history
        // carries prior outputs as its closes. Only back-indexed scalars
        // ever enter: the parent window rides the reuse buffer under the
        // shared `checked_back` rule, child legs read by index — no window
        // is collected.
        state.push(bar, 3);
        let (src1, src2) = {
            let history = state.scratch_closes();
            (checked_back(history, 1), checked_back(history, 2))
        };
        let child_len = state.child(0).len();
        let eit1 = state.child(0).closes().nth(child_len.saturating_sub(1));
        let eit2 = child_len
            .checked_sub(2)
            .and_then(|index| state.child(0).closes().nth(index));
        let out = Self::step(alpha, bar.close, src1, src2, eit1, eit2);
        state.child(0).push(BarTick::close_only(out), 3);
        state.set_last(out);
        Ok(fin_or_none(out))
    }
}

/// Scalar Kalman filter (`kalman`) over the source, parameterized by
/// `period`, `alpha` (default `0.012`), and `beta` (default `0.1`).
/// Transcribed state: `v1` (estimate, `na`-seeded) lives in the first
/// child's `last` slot, `v2` (error, seeded `1.0`) in the second's, and
/// `v3 = alpha * period` is derived per call. Per bar:
/// `v1 = nz(src[1], src)` when unseeded, then `v5 = v1`,
/// `v4 = v2 / (v2 + v3)`, `v1 = v5 + v4 * (src - v5)`,
/// `v2 = (1 - v4) * v2 + beta / period`.
pub struct KalmanIndicator {
    spec: IndicatorSpec,
}

impl KalmanIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "kalman";

    /// Fallback process-noise coupling when `alpha` is undeclared.
    pub const DEFAULT_ALPHA: f64 = 0.012;

    /// Fallback error injection when `beta` is undeclared.
    pub const DEFAULT_BETA: f64 = 0.1;

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Scalar Kalman Filter".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the `period` parameter shared by batch and update paths.
    pub fn period(params: &ParamSet) -> Result<usize, KernelError> {
        crate::math::period_len(Self::ID, params, "length")
    }

    fn shape(params: &ParamSet) -> Result<(usize, f64, f64), KernelError> {
        let period = Self::period(params)?;
        let alpha = params
            .float(Self::ID, "alpha")
            .unwrap_or(Self::DEFAULT_ALPHA);
        // Non-negative couplings keep the error update inside [0, 1], so no
        // division can leak an infinity (0/0 degrades to `na` instead).
        if !alpha.is_finite() || alpha < 0.0 {
            return Err(KernelError::InvalidParam(format!(
                "{}: alpha must be finite and non-negative, got {alpha}",
                Self::ID
            )));
        }
        let beta = params.float(Self::ID, "beta").unwrap_or(Self::DEFAULT_BETA);
        if !beta.is_finite() || beta < 0.0 {
            return Err(KernelError::InvalidParam(format!(
                "{}: beta must be finite and non-negative, got {beta}",
                Self::ID
            )));
        }
        Ok((period, alpha, beta))
    }
}

impl Default for KalmanIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for KalmanIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        // Recursive with seeded estimate: the opening print is defined.
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (period, alpha, beta) = Self::shape(params)?;
        let v3 = alpha * period as f64;
        let closes = data.close;
        let mut v1: Option<f64> = None;
        let mut v2 = 1.0;
        let values = closes
            .iter()
            .enumerate()
            .map(|(i, src)| {
                if v1.is_none() {
                    v1 = Some(if i >= 1 { closes[i - 1] } else { *src });
                }
                let v5 = nan_hole(v1);
                let v4 = v2 / (v2 + v3);
                let next = v5 + v4 * (*src - v5);
                v2 = (1.0 - v4) * v2 + beta / period as f64;
                v1 = Some(next);
                fin_or_none(next)
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
        let (period, alpha, beta) = Self::shape(params)?;
        let v3 = alpha * period as f64;
        state.push(bar, 2);
        let mut v1 = state.child(0).last();
        if v1.is_none() {
            v1 = Some(state.prev_close().unwrap_or(bar.close));
        }
        let v2 = state.child(1).last().unwrap_or(1.0);
        let v5 = nan_hole(v1);
        let v4 = v2 / (v2 + v3);
        let next = v5 + v4 * (bar.close - v5);
        let next_v2 = (1.0 - v4) * v2 + beta / period as f64;
        state.child(0).set_last(next);
        state.child(1).set_last(next_v2);
        state.set_last(next);
        Ok(fin_or_none(next))
    }
}

/// Voss predictive filter (`voss`, primary) with the bandpass `filter` as
/// secondary. Parameterized by `period`, `predict` (default `3`), and
/// `band` (default `0.25`); `order = 3 * predict`. Transcribed constants:
/// `alpha = cos(2pi / period)`, `gamma = cos(band * 2pi / period)`,
/// `delta = 1/gamma - sqrt(1/gamma^2 - 1)`, `s2 = 1 + delta`,
/// `s3 = 1 - delta`. While `order` or fewer bars have been seen both outputs
/// are `0`; afterwards
/// `filter = s3/2 * (nz(data) - nz(data[2])) + alpha*s2*filter[1]
/// - delta*filter[2]` and
/// `voss = ((3 + order) / 2) * filter - sumC` with
/// `sumC = sum_{i in 0..order} (i + 1) / order * voss[order - i]`.
/// Prior filters live in the first child's history, prior predictions in
/// the second's (which also encodes the initialization gate: computing
/// starts once it holds more than `order` outputs).
pub struct VossIndicator {
    spec: IndicatorSpec,
}

impl VossIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "voss";

    /// Fallback prediction length when `predict` is undeclared.
    pub const DEFAULT_PREDICT: i64 = 3;

    /// Fallback bandwidth when `band` is undeclared.
    pub const DEFAULT_BAND: f64 = 0.25;

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Voss Predictive Filter".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }

    /// Validate the shape parameters shared by batch and update paths:
    /// `(period, order, alpha, s2, s3)`.
    pub fn shape(params: &ParamSet) -> Result<(usize, usize, f64, f64, f64), KernelError> {
        let period = crate::math::period_len(Self::ID, params, "length")?;
        let predict = params
            .int(Self::ID, "predict")
            .unwrap_or(Self::DEFAULT_PREDICT);
        if predict < 1 {
            return Err(KernelError::InvalidParam(format!(
                "{}: predict must be at least 1, got {predict}",
                Self::ID
            )));
        }
        let band = params.float(Self::ID, "band").unwrap_or(Self::DEFAULT_BAND);
        if !band.is_finite() {
            return Err(KernelError::InvalidParam(format!(
                "{}: band must be finite, got {band}",
                Self::ID
            )));
        }
        let order = predict as usize * 3;
        let tau = std::f64::consts::TAU;
        let alpha = (tau / period as f64).cos();
        let gamma = (band * tau / period as f64).cos();
        let delta = 1.0 / gamma - (1.0 / (gamma * gamma) - 1.0).sqrt();
        Ok((period, order, alpha, 1.0 + delta, 1.0 - delta))
    }
}

impl Default for VossIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for VossIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        // The initialization gate emits defined zeros, so the opening print
        // is defined even though full-strength output starts later.
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (_, order, alpha, s2, s3) = Self::shape(params)?;
        let closes = data.close;
        let nz = |index: usize| {
            if index < closes.len() {
                closes[index]
            } else {
                0.0
            }
        };
        let mut filters = vec![0.0; closes.len()];
        let mut predictions = vec![0.0; closes.len()];
        // DIVERGENCE (see compat.rs:11): predict-then-update per bar is load-bearing;
        // batching updates after the pass would leak future corrections.
        for i in 0..closes.len() {
            if i <= order {
                continue;
            }
            let data_now = if closes[i].is_nan() { 0.0 } else { closes[i] };
            let data_ago = nz(i.saturating_sub(2));
            let data_ago = if data_ago.is_nan() { 0.0 } else { data_ago };
            let filter_1 = filters[i - 1];
            let filter_2 = filters[i - 2];
            let next_filter =
                0.5 * s3 * (data_now - data_ago) + alpha * s2 * filter_1 - (s2 - 1.0) * filter_2;
            // `voss[order - i]` at bar `n` is `predictions[n - order + i]`:
            // the `order` most recent priors, current bar excluded.
            let sum_c: f64 = (0..order)
                .map(|k| (k + 1) as f64 / order as f64 * predictions[i - order + k])
                .sum();
            filters[i] = next_filter;
            predictions[i] = ((3 + order) as f64 / 2.0) * next_filter - sum_c;
        }
        let primary = predictions.iter().map(|v| fin_or_none(*v)).collect();
        let secondary = filters.iter().map(|v| fin_or_none(*v)).collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), primary)
            .with_secondary(NamedSeries::new("filter", secondary)))
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
    ) -> Result<crate::indicator::FullTick, KernelError> {
        let (_, order, alpha, s2, s3) = Self::shape(params)?;
        let delta = s2 - 1.0;
        state.push(bar, 3);
        // Back-indexed scalars only: the parent window rides the reuse
        // buffer, both child histories read by index — no window collected.
        let data_now = if bar.close.is_nan() { 0.0 } else { bar.close };
        let data_ago = {
            let history = state.scratch_closes();
            history
                .get(history.len().saturating_sub(3))
                .copied()
                .unwrap_or(0.0)
        };
        let data_ago = if data_ago.is_nan() { 0.0 } else { data_ago };
        let filter_len = state.child(0).len();
        let filter_1 = state
            .child(0)
            .closes()
            .nth(filter_len.saturating_sub(1))
            .unwrap_or(0.0);
        let filter_2 = filter_len
            .checked_sub(2)
            .and_then(|index| state.child(0).closes().nth(index))
            .unwrap_or(0.0);
        let past_len = state.child(1).len();
        let (filter, voss) = if past_len <= order {
            (0.0, 0.0)
        } else {
            let next_filter =
                0.5 * s3 * (data_now - data_ago) + alpha * s2 * filter_1 - delta * filter_2;
            let past = state.child(1).scratch_closes();
            let sum_c: f64 = (0..order)
                .map(|k| (k + 1) as f64 / order as f64 * past[past.len() - order + k])
                .sum();
            let next_voss = ((3 + order) as f64 / 2.0) * next_filter - sum_c;
            (next_filter, next_voss)
        };
        state.child(0).push(BarTick::close_only(filter), 3);
        state.child(1).push(BarTick::close_only(voss), order + 1);
        state.set_last(voss);
        Ok(crate::indicator::FullTick {
            primary: fin_or_none(voss),
            secondary: vec![crate::indicator::NamedValue {
                name: "filter".to_owned(),
                value: fin_or_none(filter),
            }],
        })
    }
}

/// Element `bars_back` behind the newest retained value (`1` = prior bar);
/// `None` when history is that short. `saturating_sub` indexing would read
/// the newest bar as its own history on short windows — never that.
fn checked_back(history: &[f64], bars_back: usize) -> Option<f64> {
    history
        .len()
        .checked_sub(bars_back + 1)
        .and_then(|index| history.get(index).copied())
}

#[cfg(test)]
#[path = "../tests/filters.rs"]
mod tests;
