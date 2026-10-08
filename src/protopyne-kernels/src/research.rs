//! Research-frame helpers: trimmed windows to Polars frames for analysis.
//!
//! These exist for offline research (parity checks, calibration sweeps,
//! distribution studies) and never run in the per-bar hot loop: they
//! allocate columnar frames, which is exactly what the streaming executor
//! must not do per tick. [`frame_of_bars`] carries the OHLCV legs plus epoch
//! timestamps; [`frame_of_values`] carries one kernel output next to them.

use polars::prelude::*;

use crate::error::KernelError;
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

#[cfg(test)]
#[path = "../tests/research.rs"]
mod tests;
