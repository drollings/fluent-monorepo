//! Trading-session boundaries over closed bars.
//!
//! Markets run in sessions (daily closes, week ends, month ends); axes and
//! volume accounting need those breaks. Providers are pure time math over
//! [`protopyne_types::Bar`] timestamps — no venue dependency, no `egui`.

use chrono::{DateTime, Datelike as _, Utc, Weekday};
use protopyne_types::Bar;

/// One detected session boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBreak {
    /// Timestamp of the first bar in the new session.
    pub ts: DateTime<Utc>,
    /// How the boundary was classified.
    pub break_type: SessionBreakType,
    /// Short human label for axis rendering.
    pub label: Option<String>,
}

/// Classification of a session boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionBreakType {
    /// End of a trading day.
    Daily,
    /// End of a trading week.
    Weekly,
    /// End of a calendar month.
    Monthly,
    /// Provider-defined boundary.
    Custom,
}

/// Detects session boundaries between two timestamps.
///
/// Implement this for exchange-specific calendars; the built-in providers
/// below cover the common daily/weekly/monthly shapes.
pub trait SessionProvider: Send + Sync {
    /// Boundary between `from` and `to`, if any.
    fn session_break_between(
        &self,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Option<SessionBreak>;

    /// Display name for pickers and diagnostics.
    fn name(&self) -> &str;
}

/// Daily boundaries at calendar-day changes (UTC).
///
/// The hour/minute fields record the intended break clock time for display
/// purposes; detection itself keys on the calendar-day change so intraday
/// bar grids without session-clock metadata still split correctly.
#[derive(Debug, Clone)]
pub struct DailySessionProvider {
    /// Intended break hour (0–23), display hint.
    pub break_hour: u32,
    /// Intended break minute (0–59), display hint.
    pub break_minute: u32,
}

impl DailySessionProvider {
    /// Break clock time in UTC, clamped into range.
    #[must_use]
    pub fn new(hour: u32, minute: u32) -> Self {
        Self { break_hour: hour.min(23), break_minute: minute.min(59) }
    }

    /// Regular-session close preset (16:00 ET ≈ 21:00 UTC in standard time).
    #[must_use]
    pub fn nyse() -> Self {
        Self::new(21, 0)
    }

    /// Twenty-four-hour markets: midnight-UTC boundary.
    #[must_use]
    pub fn continuous() -> Self {
        Self::new(0, 0)
    }
}

impl Default for DailySessionProvider {
    fn default() -> Self {
        Self::continuous()
    }
}

impl SessionProvider for DailySessionProvider {
    fn session_break_between(
        &self,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Option<SessionBreak> {
        if from.ordinal() == to.ordinal() && from.year() == to.year() {
            None
        } else {
            Some(SessionBreak {
                ts: *to,
                break_type: SessionBreakType::Daily,
                label: Some(to.format("%Y-%m-%d").to_string()),
            })
        }
    }

    fn name(&self) -> &str {
        "Daily Sessions"
    }
}

/// Weekly boundaries at ISO-week changes.
#[derive(Debug, Clone)]
pub struct WeeklySessionProvider {
    /// Weekday the week is considered to break on (informational today).
    pub break_day: Weekday,
}

impl WeeklySessionProvider {
    /// Break on the given weekday (stored; detection keys on ISO-week change).
    #[must_use]
    pub fn new(break_day: Weekday) -> Self {
        Self { break_day }
    }
}

impl Default for WeeklySessionProvider {
    fn default() -> Self {
        Self { break_day: Weekday::Fri }
    }
}

impl SessionProvider for WeeklySessionProvider {
    fn session_break_between(
        &self,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Option<SessionBreak> {
        let from_week = from.iso_week();
        let to_week = to.iso_week();
        if from_week == to_week {
            None
        } else {
            Some(SessionBreak {
                ts: *to,
                break_type: SessionBreakType::Weekly,
                label: Some(format!("Week {}", to_week.week())),
            })
        }
    }

    fn name(&self) -> &str {
        "Weekly Sessions"
    }
}

/// Monthly boundaries at calendar-month changes.
#[derive(Debug, Clone, Default)]
pub struct MonthlySessionProvider;

impl SessionProvider for MonthlySessionProvider {
    fn session_break_between(
        &self,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Option<SessionBreak> {
        if from.month() == to.month() && from.year() == to.year() {
            None
        } else {
            Some(SessionBreak {
                ts: *to,
                break_type: SessionBreakType::Monthly,
                label: Some(to.format("%B %Y").to_string()),
            })
        }
    }

    fn name(&self) -> &str {
        "Monthly Sessions"
    }
}

/// First-match combination of providers.
///
/// Providers are consulted in insertion order; the first boundary reported
/// wins. [`CompositeSessionProvider::standard`] wires daily + weekly +
/// monthly in that priority.
pub struct CompositeSessionProvider {
    providers: Vec<Box<dyn SessionProvider>>,
}

impl CompositeSessionProvider {
    /// Empty combination.
    #[must_use]
    pub fn new() -> Self {
        Self { providers: Vec::new() }
    }

    /// Add one provider, consulted after the previously added ones.
    pub fn add_provider(&mut self, provider: Box<dyn SessionProvider>) {
        self.providers.push(provider);
    }

    /// Daily + weekly + monthly combination.
    #[must_use]
    pub fn standard() -> Self {
        let mut composite = Self::new();
        composite.add_provider(Box::new(DailySessionProvider::default()));
        composite.add_provider(Box::new(WeeklySessionProvider::default()));
        composite.add_provider(Box::new(MonthlySessionProvider));
        composite
    }
}

impl Default for CompositeSessionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionProvider for CompositeSessionProvider {
    fn session_break_between(
        &self,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Option<SessionBreak> {
        for provider in &self.providers {
            if let Some(found) = provider.session_break_between(from, to) {
                return Some(found);
            }
        }
        None
    }

    fn name(&self) -> &str {
        "Composite Sessions"
    }
}

/// Scan closed bars for session boundaries.
///
/// Returns `(index, break)` pairs where `index` is the first bar of the new
/// session.
#[must_use]
pub fn find_session_breaks(
    bars: &[Bar],
    provider: &dyn SessionProvider,
) -> Vec<(usize, SessionBreak)> {
    let mut breaks = Vec::new();
    for (index, window) in bars.windows(2).enumerate() {
        if let Some(found) = provider.session_break_between(&window[0].ts, &window[1].ts) {
            breaks.push((index + 1, found));
        }
    }
    breaks
}

#[cfg(test)]
#[path = "../tests/session.rs"]
mod tests;
