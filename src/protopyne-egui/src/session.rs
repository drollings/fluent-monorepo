//! Session-break overlays over closed bars.
//!
//! Composes the canonical break scan with the generic viewport's index
//! mapping: each returned line sits exactly on its break bar's x pixel, so
//! bar indices and gridlines can never drift apart. The scan is memoized by
//! content ([`SessionBreakCache`]), so a per-frame paint path re-scans only
//! when the window's contents move.

use chrono::{DateTime, Utc};
use common_core::hash::fnv1a64;
use fluent_egui::{BreakLine, Viewport};
use protopyne_chart_model::{session_breaks_between, ChartRow, SessionBreak, SessionProvider};
use protopyne_types::Bar;

/// Content-addressed memo of the session-break scan over closed bars.
///
/// A break depends on the full ordered series — daylight rules and session
/// boundaries inside the window, not just its endpoints — so the key is a
/// fingerprint of every bar timestamp plus the provider identity; a
/// two-endpoint key could serve a stale break when the interior changes.
/// Mirrors the pane cache's revision model: identical content keeps the
/// revision and skips [`find_session_breaks`]; changed content bumps it and
/// re-runs the scan. Display-side only — the durable bars are never touched.
#[derive(Debug, Default)]
pub struct SessionBreakCache {
    breaks: Vec<(usize, SessionBreak)>,
    fingerprint: Option<u64>,
    revision: u64,
    rebuilds: u64,
}

impl SessionBreakCache {
    /// Empty cache: the next [`refresh`](Self::refresh) always scans.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rescan iff the content moved, returning the cache revision. Identical
    /// content — including an equal-content clone — keeps the revision and
    /// leaves the scan untouched.
    pub fn refresh(&mut self, bars: &[Bar], provider: &dyn SessionProvider) -> u64 {
        self.refresh_times(&bars.iter().map(|bar| bar.ts).collect::<Vec<_>>(), provider)
    }

    /// Rescan iff the timestamp content moved: the same revision contract as
    /// [`refresh`](Self::refresh) over bare timestamps, so a caller holding
    /// display rows (epoch times, no bars) shares the memo instead of
    /// rescanning per frame. The scan itself is the one
    /// [`session_breaks_between`] core — never a second implementation.
    pub fn refresh_times(
        &mut self,
        times: &[DateTime<Utc>],
        provider: &dyn SessionProvider,
    ) -> u64 {
        let fingerprint = fingerprint_times(times, provider);
        let (revision, dirty) =
            fluent_egui::plan::content_revision(fingerprint, self.fingerprint, self.revision);
        if !dirty {
            return self.revision;
        }
        self.breaks = session_breaks_between(times, provider);
        self.fingerprint = Some(fingerprint);
        self.revision = revision;
        self.rebuilds += 1;
        self.revision
    }

    /// The cached `(index, break)` scan in bar order.
    #[must_use]
    pub fn breaks(&self) -> &[(usize, SessionBreak)] {
        &self.breaks
    }

    /// Gridlines for `viewport` from the cached scan: each line sits on its
    /// break bar's x pixel through the shared mapping.
    #[must_use]
    pub fn lines(&self, viewport: &Viewport) -> Vec<BreakLine> {
        self.breaks
            .iter()
            .map(|(index, _)| BreakLine {
                index: *index,
                x: viewport.index_to_x(*index),
            })
            .collect()
    }

    /// Cache revision: bumped only when the content moved.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Scans actually executed; unchanged content never advances it.
    #[must_use]
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }
}

/// Content fingerprint of the scan inputs: every timestamp (fixed-width
/// millis, length-prefixed so the trailing provider name can never be
/// mistaken for a timestamp) plus the provider name. Hashed once through
/// the shared [`fnv1a64`], the same content-hash idiom the pane cache uses.
/// The provider is assumed fixed for the cache's life (the terminal installs
/// one).
fn fingerprint_times(times: &[DateTime<Utc>], provider: &dyn SessionProvider) -> u64 {
    let mut bytes = Vec::with_capacity(8 + times.len() * 8 + provider.name().len());
    bytes.extend_from_slice(&(times.len() as u64).to_le_bytes());
    for at in times {
        bytes.extend_from_slice(&at.timestamp_millis().to_le_bytes());
    }
    bytes.extend_from_slice(provider.name().as_bytes());
    fnv1a64(&bytes)
}

/// Session-break gridlines over closed bars, served from `cache`.
///
/// Composes the canonical break scan with this viewport's index mapping: each
/// returned line sits exactly on its break bar's x pixel. The scan runs only
/// when the content moves (see [`SessionBreakCache`]).
#[must_use]
pub fn break_lines(
    cache: &mut SessionBreakCache,
    viewport: &Viewport,
    bars: &[Bar],
    provider: &dyn SessionProvider,
) -> Vec<BreakLine> {
    cache.refresh(bars, provider);
    cache.lines(viewport)
}

/// Session-break gridlines over row times, served from `cache`: the row-time
/// read twin of [`break_lines`] for paint paths holding display rows rather
/// than bars. Times arrive as UTC stamps (row epoch seconds lifted); the
/// scan runs only when the content moves (see [`SessionBreakCache`]).
#[must_use]
pub fn break_lines_times(
    cache: &mut SessionBreakCache,
    viewport: &Viewport,
    times: &[DateTime<Utc>],
    provider: &dyn SessionProvider,
) -> Vec<BreakLine> {
    cache.refresh_times(times, provider);
    cache.lines(viewport)
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
/// Seconds in a 365-day year: the coarsest pan unit.
const YEAR_SECONDS: i64 = 365 * DAY_SECONDS;
/// A window spanning more than this many seconds labels by date rather than
/// by clock time.
pub const INTRADAY_SPAN_SECONDS: i64 = 2 * DAY_SECONDS;
/// The major time-grid ladder: `(widest bar it governs, unit it grids on)`,
/// finest row first. A chart grids on the next boundary an operator reads it
/// by, keyed to the bar's own magnitude rather than a single candle count.
///
/// A count threshold cannot express the wanted ladder: a `15M` chart must
/// reject the hour while an hourly chart accepts four hours, so no single
/// "at least N candles" rule yields both. The table states the ladder
/// directly: one- and five-minute bars grid hourly, quarter-hour through
/// hourly bars every four hours, a four-hour chart daily, and a daily chart
/// weekly (any coarser bar by month). Every unit is strictly wider than the
/// chart's own timeframe, so a boundary is never the CM bar spacing.
const TIME_MAJOR_LADDER: [(u64, i64); 4] = [
    (300, HOUR_SECONDS),
    (3_600, FOUR_HOURS_SECONDS),
    (14_400, DAY_SECONDS),
    (86_400, WEEK_SECONDS),
];

/// The major time-grid unit (seconds) for a chart whose bars span
/// `bar_seconds`: the next sensible boundary above the bar's own magnitude
/// (see [`TIME_MAJOR_LADDER`]). A bar longer than a day grids monthly. A zero
/// `bar_seconds` reads as the finest row rather than dividing by zero.
#[must_use]
pub fn time_major_unit(bar_seconds: u64) -> i64 {
    TIME_MAJOR_LADDER
        .iter()
        .find(|(widest, _)| bar_seconds <= *widest)
        .map_or(MONTH_SECONDS, |(_, unit)| *unit)
}

/// Format a time-axis boundary label at the detail of the largest unit that
/// has passed since the previous boundary: clock time (`HH:MM`) for an
/// hourly or four-hourly grid, date and clock time (`MM/DD HH:MM`) for a
/// daily grid, and just the date (`MM/DD`) for weekly or monthly grids. This
/// is the one label rule the chart grid and the pane divider share, so a
/// boundary line and its label always read at the same granularity.
#[must_use]
pub fn fmt_boundary_time(epoch: i64, unit_seconds: i64) -> String {
    match chrono::DateTime::from_timestamp(epoch, 0) {
        Some(moment) if unit_seconds < DAY_SECONDS => moment.format("%H:%M").to_string(),
        Some(moment) if unit_seconds < WEEK_SECONDS => moment.format("%m/%d %H:%M").to_string(),
        Some(moment) => moment.format("%m/%d").to_string(),
        None => String::new(),
    }
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

/// Fewest bars a rewind/forward step spans: the step picks the narrowest
/// calendar unit that covers at least this many bars, so one click moves a
/// couple hundred candles whatever the timeframe.
pub const PAN_UNIT_MIN_BARS: u64 = 150;

/// The rewind/forward step unit (seconds) for a chart whose bars span
/// `bar_seconds`: the narrowest of day, week, month, and year that covers at
/// least [`PAN_UNIT_MIN_BARS`] bars, else the coarsest of those. A 1M or 5M
/// chart therefore steps by a day, a 15M or 1H chart by a week, a 4H chart by
/// a month, and a daily chart by a year. A zero `bar_seconds` refuses to the
/// one-second floor rather than dividing by zero.
#[must_use]
pub fn pan_unit_seconds(bar_seconds: u64) -> i64 {
    let floor = bar_seconds.saturating_mul(PAN_UNIT_MIN_BARS).max(1);
    for unit in [DAY_SECONDS, WEEK_SECONDS, MONTH_SECONDS, YEAR_SECONDS] {
        if (unit as u64) >= floor {
            return unit;
        }
    }
    YEAR_SECONDS
}

/// Vertical time-gridlines at the major time unit for a chart of
/// `bar_seconds` bars.
///
/// The unit is the next sensible boundary above the chart's own magnitude
/// (see [`time_major_unit`]): a `5M` or `1M` chart grids hourly, a `15M` or
/// `1H` chart every four hours, so a boundary line is never finer than a
/// major unit and a short window is never crowded.
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
            BreakLine {
                index: slot,
                x: viewport.index_to_x(slot),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/session.rs"]
mod tests;
