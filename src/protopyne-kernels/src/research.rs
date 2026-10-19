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
use crate::indicator::{fin_or_none, nan_hole};
use crate::math::{div_or_none, strict_window};
use protopyne_types::Bar;

pub use crate::scores::{rolling_slope, rsrs};

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
                nan_hole(get(b).to_f64())
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
            let path: f64 = window
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).abs())
                .sum();
            // Displacement never exceeds path on the same window, so a zero
            // path means a zero displacement (`0/0` refuses on both
            // spellings) and `div_or_none` matches the guarded division
            // everywhere this arm can reach.
            div_or_none(displacement, path)
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
            let extent = high.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b))
                - low.iter().fold(f64::INFINITY, |a, b| a.min(*b));
            // A zero extent means every high equals every low, so the summed
            // ranges are zero too (`0/0` refuses on both spellings) and
            // `div_or_none` matches the guarded division everywhere here.
            div_or_none(inside, extent)
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
            // `scaled = ln(2·period)` is finite and ≥ ln 2 for every period
            // that reaches this arm (period 0 windows refuse first), so the
            // guarded quotient matches `div_or_none` on all inputs here.
            let quotient = div_or_none(length.ln() + std::f64::consts::LN_2, scaled)?;
            fin_or_none(1.0 + quotient)
        })
        .collect()
}

/// Rolling least-squares slope of `high` on `low` over `n` bars, then its
/// `m`-bar z-score (population denominator): the RSRS timing score.
/// Constant windows (zero slope variance) and short histories read `None` —
/// there is nothing to rank. Window means are demeaned per window for
/// numerical stability; the reference moment form agrees to Expr-parity eps.
///
/// The implementation lives in the featureless [`crate::scores`] home and is
/// re-exported here so offline callers keep one entry point.

#[cfg(test)]
#[path = "../tests/research.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/research_rsrs_parity.rs"]
mod rsrs_parity_tests;
