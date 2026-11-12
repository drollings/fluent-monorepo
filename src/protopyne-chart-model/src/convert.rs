//! Display-boundary conversion: `Decimal`-in, `f64`-at-the-edge.
//!
//! Mirrors the runner/service skip policy in one place: a bar whose leg has
//! no finite chart float is skipped by [`row_of`]/[`rows_of`], never
//! substituted. Callers that need strictness use [`to_display_float`] per
//! leg and refuse on `None` themselves.
//!
//! The row and its container are the wire shapes owned by `protopyne-view`
//! ([`ChartBar`]/[`ChartBars`]): conversion here produces them directly, so
//! the terminal and the server share one definition instead of a field-wise
//! twin. [`ChartRow`] is the display side's long-standing name for the same
//! [`ChartBar`].

use protopyne_types::Bar;
use protopyne_view::ChartBar;
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

/// The wire row under the display side's name (one definition, in
/// [`protopyne_view`]).
pub use protopyne_view::ChartBar as ChartRow;
/// The wire container, re-exported so [`rows_of`] never leaves the view
/// crate's definition.
pub use protopyne_view::ChartBars;

/// Convert one money/price leg to a chart float.
///
/// Returns `None` when the value is unrepresentable as a finite `f64`
/// (overflow to infinity, `NaN`). Never panics.
#[must_use]
/// Convert one money/price leg to a chart float.
/// 
/// Returns `None` when the value is unrepresentable as a finite `f64`
/// (overflow to infinity, `NaN`). Never panics.
pub fn to_display_float(value: Decimal) -> Option<f64> {
    let flat = value.to_f64()?;
    if flat.is_finite() {
        Some(flat)
    } else {
        None
    }
}

/// Convert one closed bar, or `None` when a leg has no finite chart float.
#[must_use]
/// Convert one closed bar, or `None` when a leg has no finite chart float.
pub fn row_of(bar: &Bar) -> Option<ChartBar> {
    Some(ChartBar {
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
/// Convert a window, counting skips instead of failing the batch.
pub fn rows_of(bars: &[Bar]) -> ChartBars {
    let mut rows = Vec::with_capacity(bars.len());
    let mut dropped = 0_u64;
    for bar in bars {
        match row_of(bar) {
            Some(row) => rows.push(row),
            None => dropped += 1,
        }
    }
    ChartBars { rows, dropped }
}

#[cfg(test)]
#[path = "../tests/convert.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/display_boundary.rs"]
mod display_boundary_tests;