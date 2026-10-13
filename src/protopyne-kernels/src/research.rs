//! Research-frame helpers: trimmed windows to Polars frames for analysis.
//!
//! These exist for offline research (Expr-parity checks, calibration sweeps,
//! distribution studies) and never run in the per-bar hot loop: they
//! allocate columnar frames, which is exactly what the streaming executor
//! must not do per tick. [`frame_of_bars`] carries the OHLCV legs plus epoch
//! timestamps; [`frame_of_values`] carries one kernel output next to them.
//!
//! The noise and RSRS helpers below are research-only by the same token:
//! full-series regressions over `&[f64]` slices, hand-rolled (no frame
//! allocation inside), verified against reference expressions in tests.
//! They never enter the indicator registry: they score history, they do not
//! stream bars.

use polars::prelude::*;

use crate::error::KernelError;
use crate::indicator::fin_or_none;
use crate::math::strict_window;
use protopyne_types::Bar;

/// Convert closed bars to a columnar research frame.
///
/// Columns: `ts` (epoch seconds), `open`, `high`, `low`, `close`, `volume`.
pub fn frame_of_bars(bars: &[Bar]) -> Result<DataFrame, KernelError> {
    let ts: Vec<i64> = bars.iter().map(|b| b.ts.timestamp()).collect();
    let f = |get: fn(&Bar) -> f64| bars.iter().map(get).collect::<Vec<f64>>();
    let decimal_f = |get: fn(&Bar) -> rust_decimal::Decimal| {
        bars.iter()
            .map(|b| {
                use rust_decimal::prelude::ToPrimitive as _;
                get(b).to_f64().unwrap_or(f64::NAN)
            })
            .collect::<Vec<f64>>()
    };
    DataFrame::new(vec![
        Series::new("ts".into(), &ts).into(),
        Series::new("open".into(), &decimal_f(|b| b.o)).into(),
        Series::new("high".into(), &decimal_f(|b| b.h)).into(),
        Series::new("low".into(), &decimal_f(|b| b.l)).into(),
        Series::new("close".into(), &decimal_f(|b| b.c)).into(),
        Series::new("volume".into(), &f(|b| b.volume as f64)).into(),
    ])
    .map_err(|e| KernelError::Research(format!("frame_of_bars: {e}")))
}

/// Convert one kernel output to a single-column frame (`None` becomes null).
pub fn frame_of_values(name: &str, values: &[Option<f64>]) -> Result<DataFrame, KernelError> {
    DataFrame::new(vec![Series::new(name.into(), values).into()])
        .map_err(|e| KernelError::Research(format!("frame_of_values: {e}")))
}

/// Kaufman-style efficiency ratio over closes: net displacement over total
/// path length, `|c[i] - c[i-p]| / sum |diff|`. Reads `None` while warming
/// and on degenerate (flat) windows, matching the reference's null there.
///
/// Note: this deliberately shares its name with the per-bar candle measure
/// and therefore stays module-pathed (`research::efficiency_ratio`) instead
/// of joining the crate-root re-exports — same name, different arity and
/// meaning.
#[must_use]
pub fn efficiency_ratio(closes: &[f64], period: usize) -> Vec<Option<f64>> {
    closes
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let window = strict_window(closes, i, period + 1)?;
            let displacement = (window[period] - window[0]).abs();
            let path: f64 = window.windows(2).map(|pair| (pair[1] - pair[0]).abs()).sum();
            fin_or_none(displacement / path)
        })
        .collect()
}

/// Price density: summed bar ranges over the window's total extent,
/// `sum(h - l) / (max(h) - min(l))`. Flat windows read `None` on both sides
/// of the Expr-parity comparison (the reference divides zero by zero there).
#[must_use]
pub fn price_density(highs: &[f64], lows: &[f64], period: usize) -> Vec<Option<f64>> {
    let len = highs.len().min(lows.len());
    (0..len)
        .map(|i| {
            let high = strict_window(highs, i, period)?;
            let low = strict_window(lows, i, period)?;
            let inside: f64 = high.iter().zip(low.iter()).map(|(h, l)| h - l).sum();
            let extent =
                high.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b))
                    - low.iter().fold(f64::INFINITY, |a, b| a.min(*b));
            fin_or_none(inside / extent)
        })
        .collect()
}

/// Fractal dimension `1 + (ln(L) + ln2) / ln(2p)` with
/// `L = sum sqrt(1/p^2 + diff(close) / extent)`. Any non-finite leg in the
/// window (including a zero extent) yields `None`, matching the reference
/// null that its `NaN` maps to in fixtures.
#[must_use]
pub fn fractal_dimension(
    highs: &[f64],
    lows: &[f64],
    closes: &[f64],
    period: usize,
) -> Vec<Option<f64>> {
    let len = highs.len().min(lows.len()).min(closes.len());
    let period_f = period as f64;
    let scaled = (2.0 * period_f).ln();
    (0..len)
        .map(|i| {
            let high = strict_window(highs, i, period)?;
            let low = strict_window(lows, i, period)?;
            let close = strict_window(closes, i, period + 1)?;
            let extent = high.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b))
                - low.iter().fold(f64::INFINITY, |a, b| a.min(*b));
            let unit = 1.0 / (period_f * period_f);
            let length: f64 = close
                .windows(2)
                .map(|pair| (unit + (pair[1] - pair[0]) / extent).sqrt())
                .sum();
            fin_or_none(1.0 + (length.ln() + std::f64::consts::LN_2) / scaled)
        })
        .collect()
}

/// Rolling least-squares slope of `high` on `low` over `n` bars, then its
/// `m`-bar z-score (population denominator): the RSRS timing score.
/// Constant windows (zero slope variance) and short histories read `None` —
/// there is nothing to rank. Window means are demeaned per window for
/// numerical stability; the reference moment form agrees to Expr-parity eps.
#[must_use]
pub fn rsrs(highs: &[f64], lows: &[f64], n: usize, m: usize) -> Vec<Option<f64>> {
    let len = highs.len().min(lows.len());
    let slopes: Vec<Option<f64>> = (0..len).map(|i| rolling_slope(highs, lows, i, n)).collect();
    (0..len)
        .map(|i| {
            if m == 0 || i + 1 < m {
                return None;
            }
            let window = &slopes[i + 1 - m..=i];
            let mut scores = Vec::with_capacity(m);
            for slope in window.iter().flatten() {
                if slope.is_finite() {
                    scores.push(*slope);
                } else {
                    return None;
                }
            }
            if scores.len() != m {
                return None;
            }
            let mean = scores.iter().sum::<f64>() / m as f64;
            let variance = scores.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / m as f64;
            #[allow(clippy::float_cmp)]
            if variance == 0.0 {
                return None;
            }
            fin_or_none((scores[m - 1] - mean) / variance.sqrt())
        })
        .collect()
}

/// Least-squares slope of `y` on `x` over the `n`-bar window ending at `end`.
#[allow(clippy::float_cmp)]
fn rolling_slope(highs: &[f64], lows: &[f64], end: usize, n: usize) -> Option<f64> {
    let high = strict_window(highs, end, n)?;
    let low = strict_window(lows, end, n)?;
    let (mean_x, mean_y) = (
        low.iter().sum::<f64>() / n as f64,
        high.iter().sum::<f64>() / n as f64,
    );
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for (x, y) in low.iter().zip(high.iter()) {
        sxx += (x - mean_x) * (x - mean_x);
        sxy += (x - mean_x) * (y - mean_y);
    }
    // An exactly zero sum of squares means a constant window (sums of
    // squares are exact here); no slope to score.
    if sxx == 0.0 {
        return None;
    }
    fin_or_none(sxy / sxx)
}

#[cfg(test)]
#[path = "../tests/research.rs"]
mod tests;
