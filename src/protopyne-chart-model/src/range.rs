//! Preset date-range selectors for fetch sizing and toolbar state.
//!
//! Pure time math: a preset maps to an absolute start timestamp (or `None`
//! for "all data") and to a bar-count estimate for a given bar spacing. No
//! I/O, no clock beyond `Utc::now` at the call instant.

use chrono::{DateTime, Datelike as _, Duration, NaiveDate, Utc};

/// Seconds in the 30-day calendar month the month presets measure. One home
/// for the month unit the presets, the fetch sizing, and the load-more window
/// all count in.
pub const MONTH_SECONDS: i64 = 30 * 24 * 3_600;

/// Quick-pick lookback windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DateRange {
    /// One day.
    Day1,
    /// Five days.
    Day5,
    /// One month (default).
    #[default]
    Month1,
    /// Three months.
    Month3,
    /// Six months.
    Month6,
    /// January 1st to now.
    Ytd,
    /// One year.
    Year1,
    /// Five years.
    Year5,
    /// Everything available.
    All,
}

impl DateRange {
    /// Short button label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Day1 => "1D",
            Self::Day5 => "5D",
            Self::Month1 => "1M",
            Self::Month3 => "3M",
            Self::Month6 => "6M",
            Self::Ytd => "YTD",
            Self::Year1 => "1Y",
            Self::Year5 => "5Y",
            Self::All => "All",
        }
    }

    /// Parse a preset from its short label (`1D`, `1M`, `YTD`, …), the
    /// inverse of [`label`](Self::label). Unknown spellings answer `None`
    /// rather than guessing a default.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::default_presets()
            .into_iter()
            .find(|preset| preset.label() == label)
    }

    /// Longer tooltip text.
    #[must_use]
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Day1 => "1 day",
            Self::Day5 => "5 days",
            Self::Month1 => "1 month",
            Self::Month3 => "3 months",
            Self::Month6 => "6 months",
            Self::Ytd => "Year to date",
            Self::Year1 => "1 year",
            Self::Year5 => "5 years",
            Self::All => "All available data",
        }
    }

    /// Every preset in toolbar order.
    #[must_use]
    pub fn default_presets() -> Vec<Self> {
        vec![
            Self::Day1,
            Self::Day5,
            Self::Month1,
            Self::Month3,
            Self::Month6,
            Self::Ytd,
            Self::Year1,
            Self::Year5,
            Self::All,
        ]
    }

    /// Chart-range presets offered in the range menu, in display order: the
    /// coarse spans an operator scans with (one month through everything).
    /// Orthogonal to the bar timeframe — the preset sets months or years of
    /// range and the loader completes it on demand.
    #[must_use]
    pub fn chart_presets() -> Vec<Self> {
        vec![
            Self::Month1,
            Self::Month3,
            Self::Ytd,
            Self::Year1,
            Self::Year5,
            Self::All,
        ]
    }

    /// Lookback in whole months for the load-more window: `None` for
    /// [`DateRange::All`] (no fixed horizon). Day/week presets round up to
    /// one month so the window always holds at least the requested span.
    #[must_use]
    pub fn lookback_months(self) -> Option<u32> {
        match self {
            Self::Day1 | Self::Day5 | Self::Month1 => Some(1),
            Self::Month3 => Some(3),
            Self::Month6 => Some(6),
            Self::Ytd | Self::Year1 => Some(12),
            Self::Year5 => Some(60),
            Self::All => None,
        }
    }

    /// Absolute start timestamp, or `None` for [`DateRange::All`].
    #[must_use]
    pub fn start_timestamp(self) -> Option<DateTime<Utc>> {
        let now = Utc::now();
        match self {
            Self::Day1 => Some(now - Duration::days(1)),
            Self::Day5 => Some(now - Duration::days(5)),
            Self::Month1 => Some(now - Duration::days(30)),
            Self::Month3 => Some(now - Duration::days(90)),
            Self::Month6 => Some(now - Duration::days(180)),
            Self::Ytd => {
                let year_start = NaiveDate::from_ymd_opt(now.year(), 1, 1)?;
                Some(DateTime::from_naive_utc_and_offset(
                    year_start.and_hms_opt(0, 0, 0)?,
                    Utc,
                ))
            }
            Self::Year1 => Some(now - Duration::days(365)),
            Self::Year5 => Some(now - Duration::days(365 * 5)),
            Self::All => None,
        }
    }

    /// `(start, end)` pair with `end` fixed at now.
    #[must_use]
    pub fn to_timestamps(self) -> (Option<DateTime<Utc>>, DateTime<Utc>) {
        (self.start_timestamp(), Utc::now())
    }

    /// The preset's calendar span in seconds: the one definition of how wide
    /// each range is. [`estimated_bars`](Self::estimated_bars) divides this by
    /// the bar spacing, and the fetch planner sizes the trailing read from it,
    /// so the window and the read can never disagree about the span.
    #[must_use]
    pub fn span_seconds(self) -> u64 {
        let seconds: i64 = match self {
            Self::Day1 => 24 * 3_600,
            Self::Day5 => 5 * 24 * 3_600,
            Self::Month1 => MONTH_SECONDS,
            Self::Month3 => 3 * MONTH_SECONDS,
            Self::Month6 => 6 * MONTH_SECONDS,
            Self::Ytd => i64::from(Utc::now().ordinal()) * 24 * 3_600,
            Self::Year1 => 365 * 24 * 3_600,
            Self::Year5 => 5 * 365 * 24 * 3_600,
            Self::All => 10 * 365 * 24 * 3_600,
        };
        seconds.max(0) as u64
    }

    /// Rough bar count for `bar_duration_seconds` spacing.
    ///
    /// Calendar math, not session-aware: weekends and holidays still count.
    /// Fetch planners treat it as an upper bound alongside warmup headroom.
    #[must_use]
    pub fn estimated_bars(self, bar_duration_seconds: u64) -> usize {
        (self.span_seconds() / bar_duration_seconds.max(1)) as usize
    }
}

impl std::fmt::Display for DateRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
#[path = "../tests/range.rs"]
mod tests;
