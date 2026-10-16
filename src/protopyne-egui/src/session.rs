//! Session-break overlays over closed bars.
//!
//! Composes the canonical break scan with the generic viewport's index
//! mapping: each returned line sits exactly on its break bar's x pixel, so
//! bar indices and gridlines can never drift apart.

use fluent_egui::{BreakLine, Viewport};
use protopyne_chart_model::{ChartRow, SessionProvider, find_session_breaks};
use protopyne_types::Bar;

/// Session-break gridlines over closed bars.
///
/// Composes the canonical break scan with this viewport's index mapping:
/// each returned line sits exactly on its break bar's x pixel.
#[must_use]
pub fn break_lines(viewport: &Viewport, bars: &[Bar], provider: &dyn SessionProvider) -> Vec<BreakLine> {
    find_session_breaks(bars, provider)
        .into_iter()
        .map(|(index, _)| BreakLine { index, x: viewport.index_to_x(index) })
        .collect()
}

/// Seconds in an hour: the finest major time-grid unit.
const HOUR_SECONDS: i64 = 3_600;
/// Seconds in four hours: the major unit above an intraday timeframe.
const FOUR_HOURS_SECONDS: i64 = 4 * HOUR_SECONDS;
/// Seconds in a day.
pub const DAY_SECONDS: i64 = 86_400;
/// Seconds in a week (seven days).
const WEEK_SECONDS: i64 = 7 * DAY_SECONDS;
/// Seconds in a 30-day month.
const MONTH_SECONDS: i64 = 30 * DAY_SECONDS;
/// A window spanning more than this many seconds labels by date rather than
/// by clock time.
pub const INTRADAY_SPAN_SECONDS: i64 = 2 * DAY_SECONDS;
/// The smallest number of candles a major grid unit must span. The unit is
/// chosen as the narrowest nice interval at or above this, so a `15M` chart
/// grids every four hours and an hourly chart every day — never a major unit
/// so dense it crowds the candle spacing.
pub const MIN_CANDLES_PER_GRID_UNIT: u64 = 5;

/// The major time-grid unit (seconds) for a chart whose bars span
/// `bar_seconds`.
///
/// Picks the narrowest nice interval — hour, four hours, day, week, month —
/// that spans at least [`MIN_CANDLES_PER_GRID_UNIT`] candles, so the vertical
/// background grid always sits above the chart's own timeframe. A bar longer
/// than a month falls back to whole months. A zero `bar_seconds` refuses to a
/// one-second floor rather than dividing by zero.
#[must_use]
pub fn time_major_unit(bar_seconds: u64) -> i64 {
    let floor = bar_seconds.saturating_mul(MIN_CANDLES_PER_GRID_UNIT).max(1);
    for unit in [HOUR_SECONDS, FOUR_HOURS_SECONDS, DAY_SECONDS, WEEK_SECONDS, MONTH_SECONDS] {
        if (unit as u64) >= floor {
            return unit;
        }
    }
    let mut unit = MONTH_SECONDS;
    while (unit as u64) < floor {
        unit = unit.saturating_mul(2);
    }
    unit
}

/// Whether time-axis labels for `bar_seconds` bars read as clock time
/// (`%H:%M`) rather than calendar dates (`%m-%d`).
///
/// The granularity follows the chart timeframe, never the window span: any
/// sub-daily timeframe labels down to hour and minute (a month of
/// fifteen-minute bars still reads as clock time), while daily bars and
/// coarser label by date. A zero duration sits below a day and reads clock
/// time, matching the grid unit's one-second floor.
#[must_use]
pub fn labels_show_clock_time(bar_seconds: u64) -> bool {
    bar_seconds < DAY_SECONDS as u64
}

/// Vertical time-gridlines at the major time unit for a chart of
/// `bar_seconds` bars.
///
/// The unit is the narrowest nice interval at or above
/// [`MIN_CANDLES_PER_GRID_UNIT`] candles (see [`time_major_unit`]): a `15M`
/// chart grids every four hours, an hourly chart every day, so the major unit
/// is always above the current timeframe and a short window is never crowded.
/// A line sits on the first bar of each new unit and takes its x from the
/// shared [`Viewport`] mapping, so gridlines and bars can never drift apart.
/// Fewer than two rows, or rows that never cross a boundary, yield no lines.
#[must_use]
pub fn time_grid_lines(viewport: &Viewport, rows: &[ChartRow], bar_seconds: u64) -> Vec<BreakLine> {
    if rows.len() < 2 {
        return Vec::new();
    }
    let unit = time_major_unit(bar_seconds);
    rows.windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[0].time.div_euclid(unit) != pair[1].time.div_euclid(unit))
        .map(|(index, _)| {
            let slot = index + 1;
            BreakLine { index: slot, x: viewport.index_to_x(slot) }
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/session.rs"]
mod tests;
