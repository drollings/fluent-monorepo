//! Preset date-range selectors for fetch sizing and toolbar state.
//!
//! Pure time math: a preset maps to an absolute start timestamp (or `None`
//! for "all data") and to a bar-count estimate for a given bar spacing. No
//! I/O, no clock beyond `Utc::now` at the call instant.

use chrono::{DateTime, Datelike as _, Duration, NaiveDate, Utc};

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

    /// Longer tooltip text.
    #[must_use]
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Day1 => "1 day in 1 minute intervals",
            Self::Day5 => "5 days in 5 minutes intervals",
            Self::Month1 => "1 month in 30 minutes intervals",
            Self::Month3 => "3 months in 1 hour intervals",
            Self::Month6 => "6 months in 2 hours intervals",
            Self::Ytd => "Year to day in 1 day intervals",
            Self::Year1 => "1 year in 1 day intervals",
            Self::Year5 => "5 years in 1 week intervals",
            Self::All => "All data in 1 month intervals",
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

    /// Rough bar count for `bar_duration_seconds` spacing.
    ///
    /// Calendar math, not session-aware: weekends and holidays still count.
    /// Fetch planners treat it as an upper bound alongside warmup headroom.
    #[must_use]
    pub fn estimated_bars(self, bar_duration_seconds: u64) -> usize {
        let duration_seconds = match self {
            Self::Day1 => 24 * 3_600,
            Self::Day5 => 5 * 24 * 3_600,
            Self::Month1 => 30 * 24 * 3_600,
            Self::Month3 => 90 * 24 * 3_600,
            Self::Month6 => 180 * 24 * 3_600,
            Self::Ytd => u64::from(Utc::now().ordinal()) * 24 * 3_600,
            Self::Year1 => 365 * 24 * 3_600,
            Self::Year5 => 5 * 365 * 24 * 3_600,
            Self::All => 10 * 365 * 24 * 3_600,
        };
        (duration_seconds / bar_duration_seconds.max(1)) as usize
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
