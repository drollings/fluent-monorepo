//! Display-boundary conversion: `Decimal`-in, `f64`-at-the-edge.
//!
//! Mirrors the runner/service skip policy in one place: a bar whose leg has
//! no finite chart float is skipped by [`row_of`]/[`rows_of`], never
//! substituted. Callers that need strictness use [`to_display_float`] per
//! leg and refuse on `None` themselves.

use protopyne_types::Bar;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive as _;

/// Convert one money/price leg to a chart float.
///
/// Returns `None` when the value is unrepresentable as a finite `f64`
/// (overflow to infinity, `NaN`). Never panics.
#[must_use]
pub fn to_display_float(value: Decimal) -> Option<f64> {
    let flat = value.to_f64()?;
    if flat.is_finite() { Some(flat) } else { None }
}

/// One chartable bar row: epoch-seconds time plus finite floats.
///
/// This is the headless twin of the server chart payload: same shape, owned
/// here so headless consumers (transforms, geometry, tests) never depend on
/// the HTTP layer. The server keeps its own row type; conversion between the
/// two is field-wise and total.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChartRow {
    /// Bar close time as epoch seconds.
    pub time: i64,
    /// Open as chart-native float.
    pub open: f64,
    /// High as chart-native float.
    pub high: f64,
    /// Low as chart-native float.
    pub low: f64,
    /// Close as chart-native float.
    pub close: f64,
    /// Traded volume.
    pub volume: u64,
}

/// A converted window plus how many bars never made it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChartRows {
    /// Chartable bars in history order.
    pub rows: Vec<ChartRow>,
    /// Bars skipped for non-finite or unrepresentable floats.
    pub dropped: u64,
}

/// Convert one closed bar, or `None` when a leg has no finite chart float.
#[must_use]
pub fn row_of(bar: &Bar) -> Option<ChartRow> {
    Some(ChartRow {
        time: bar.ts.timestamp(),
        open: to_display_float(bar.o)?,
        high: to_display_float(bar.h)?,
        low: to_display_float(bar.l)?,
        close: to_display_float(bar.c)?,
        volume: bar.volume,
    })
}

/// Convert a window, counting skips instead of failing the batch.
#[must_use]
pub fn rows_of(bars: &[Bar]) -> ChartRows {
    let mut rows = Vec::with_capacity(bars.len());
    let mut dropped = 0_u64;
    for bar in bars {
        match row_of(bar) {
            Some(row) => rows.push(row),
            None => dropped += 1,
        }
    }
    ChartRows { rows, dropped }
}

#[cfg(test)]
#[path = "../tests/convert.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/display_boundary.rs"]
mod display_boundary_tests;
