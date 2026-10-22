//! Single wall-clock predicate for exchange sessions: the daylight-aware
//! New York clock every crate reads instead of keeping its own.
//!
//! Pine session strings (`input.session("0930-1600")`) name exchange wall
//! clock; this clock evaluates them with the US daylight rules encoded
//! directly (no timezone database): EST (UTC-5) from the first Sunday of
//! November 02:00 local to the second Sunday of March 02:00 local, EDT
//! (UTC-4) between. Membership is start-inclusive, end-exclusive on
//! wall-clock minutes. Overnight ranges (open past close) refuse: regular
//! sessions never wrap midnight.
//!
//! Consumers: kernels gates bar/session logic on it; chart-model session
//! providers delegate their ET date math to it (no second DST copy).

use crate::error::TypesError;
use fluent_wvr::validate::Validate;

/// Daylight-aware regular-hours clock over millisecond stamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradingSession {
    open_min: u32,
    close_min: u32,
}

impl TradingSession {
    /// Parse a `HHMM-HHMM` session string. Overnight ranges (open past
    /// close) refuse: regular sessions never wrap midnight.
    pub fn parse(session: &str) -> Result<Self, TypesError> {
        let bytes = session.as_bytes();
        let bad =
            || TypesError::InvalidSession(format!("session must read HHMM-HHMM, got {session:?}"));
        if bytes.len() != 9 || bytes[4] != b'-' {
            return Err(bad());
        }
        let num = |lo: usize| {
            if !bytes[lo].is_ascii_digit() || !bytes[lo + 1].is_ascii_digit() {
                return Err(bad());
            }
            Ok(u32::from(bytes[lo] - b'0') * 10 + u32::from(bytes[lo + 1] - b'0'))
        };
        let (oh, om, ch, cm) = (num(0)?, num(2)?, num(5)?, num(7)?);
        if oh > 23 || ch > 23 || om > 59 || cm > 59 {
            return Err(bad());
        }
        let (open_min, close_min) = (oh * 60 + om, ch * 60 + cm);
        if open_min >= close_min {
            return Err(TypesError::InvalidSession(format!(
                "session must open before it closes, got {session:?}"
            )));
        }
        Ok(Self {
            open_min,
            close_min,
        })
    }

    /// Whether the stamp falls inside the session on its ET wall clock.
    #[must_use]
    pub fn contains(self, ts_ms: i64) -> bool {
        let mins = et_wall_minutes(ts_ms);
        mins >= self.open_min && mins < self.close_min
    }

    /// ET calendar date `(year, month, day)` of the stamp.
    #[must_use]
    pub fn session_date(self, ts_ms: i64) -> (i32, u32, u32) {
        let _ = self;
        et_civil_date(ts_ms)
    }
}

impl Validate for TradingSession {
    type Err = TypesError;

    fn validate(&self) -> Result<(), Self::Err> {
        if self.open_min >= self.close_min || self.close_min > 24 * 60 {
            return Err(TypesError::InvalidSession(format!(
                "session opens {} and closes {}: must open strictly before close inside one day",
                self.open_min, self.close_min
            )));
        }
        Ok(())
    }
}

/// ET weekday of the stamp: Monday = 1 through Sunday = 7. The shared
/// calendar reads this instead of keeping its own day math.
#[must_use]
pub fn et_weekday(ts_ms: i64) -> u32 {
    weekday_of((ts_ms + et_offset_ms(ts_ms)).div_euclid(86_400_000)) as u32
}

/// ET calendar date key.
type EtDate = (i32, u32, u32);

/// Weekend/holiday-aware session calendar: regular hours on weekdays, full
/// closes on listed dates, early closes on half days. Weekends never trade.
/// Dates are ET calendar dates; daylight flips ride the shared ET clock, so
/// a session day never contains a transition (transitions fall on Sundays).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCalendar {
    regular: TradingSession,
    holidays: std::collections::BTreeSet<EtDate>,
    half_days: std::collections::BTreeMap<EtDate, TradingSession>,
}

impl SessionCalendar {
    /// US equity regular hours with no dated exceptions. Weekends are closed
    /// by construction; holidays and half days join per fixture.
    #[must_use]
    pub fn us_equities() -> Self {
        Self {
            regular: TradingSession {
                open_min: 9 * 60 + 30,
                close_min: 16 * 60,
            },
            holidays: std::collections::BTreeSet::new(),
            half_days: std::collections::BTreeMap::new(),
        }
    }

    /// Close the whole ET date.
    #[must_use]
    pub fn with_holiday(mut self, date: EtDate) -> Self {
        self.holidays.insert(date);
        self
    }

    /// Replace the ET date's session with an early close.
    #[must_use]
    pub fn with_half_day(mut self, date: EtDate, session: TradingSession) -> Self {
        self.holidays.remove(&date);
        self.half_days.insert(date, session);
        self
    }

    /// Whether the ET date trades at all.
    #[must_use]
    pub fn is_trading_day(&self, date: EtDate) -> bool {
        self.session_for(date).is_some()
    }

    /// The session owning the ET date, if it trades.
    #[must_use]
    pub fn session_for(&self, date: EtDate) -> Option<TradingSession> {
        if self.holidays.contains(&date) {
            return None;
        }
        if let Some(session) = self.half_days.get(&date) {
            return Some(*session);
        }
        let (year, month, day) = date;
        let weekday = weekday_of(days_from_civil(year, month, day));
        if weekday >= 6 {
            return None;
        }
        Some(self.regular)
    }

    /// Whether the stamp falls inside a trading session.
    #[must_use]
    pub fn contains(&self, ts_ms: i64) -> bool {
        let date = et_civil_date(ts_ms);
        self.session_for(date)
            .is_some_and(|session| session.contains(ts_ms))
    }

    /// In-session bucket starts strictly between two stamps: the trading
    /// gap a bar sequence with those endpoints skips. Zero means contiguous
    /// (or a non-trading span such as a weekend); a positive count names
    /// missing trading bars. Degenerate inputs return zero.
    #[must_use]
    pub fn trading_buckets_between(&self, prev_ms: i64, next_ms: i64, timeframe_ms: i64) -> u64 {
        if timeframe_ms <= 0 || next_ms <= prev_ms {
            return 0;
        }
        let mut total = 0u64;
        let mut day = et_civil_date(prev_ms);
        let end = et_civil_date(next_ms);
        loop {
            if let Some(session) = self.session_for(day) {
                // Session days never contain a DST transition, so one offset
                // prices the whole day; ET midnight in UTC anchors the window.
                let day_noon_utc = days_from_civil(day.0, day.1, day.2) * 86_400_000 + 43_200_000;
                let midnight_utc =
                    days_from_civil(day.0, day.1, day.2) * 86_400_000 - et_offset_ms(day_noon_utc);
                let open_ms = midnight_utc + i64::from(session.open_min) * 60_000;
                let close_ms = midnight_utc + i64::from(session.close_min) * 60_000;
                // Bucket starts B with prev < B < next inside [open, close).
                let lo = open_ms.max(prev_ms.saturating_add(1));
                let hi = close_ms.min(next_ms);
                if lo < hi {
                    let first = lo.div_euclid(timeframe_ms) * timeframe_ms;
                    let first = if first < lo {
                        first.saturating_add(timeframe_ms)
                    } else {
                        first
                    };
                    if first < hi {
                        total = total.saturating_add(((hi - 1 - first) / timeframe_ms + 1) as u64);
                    }
                }
            }
            if day == end {
                break;
            }
            day = civil_from_days(days_from_civil(day.0, day.1, day.2) + 1);
        }
        total
    }
}

/// Whether America/New_York observes daylight saving at the stamp: from the
/// second Sunday of March 07:00 UTC up to the first Sunday of November
/// 06:00 UTC (02:00 local both ends).
#[must_use]
pub fn is_dst_et(ts_ms: i64) -> bool {
    let (year, _, _) = civil_from_days(ts_ms.div_euclid(86_400_000));
    ts_ms >= dst_spring_utc_ms(year) && ts_ms < dst_fall_utc_ms(year)
}

/// UTC offset of America/New_York at the stamp, in milliseconds.
#[must_use]
pub fn et_offset_ms(ts_ms: i64) -> i64 {
    if is_dst_et(ts_ms) {
        -4 * 3_600_000
    } else {
        -5 * 3_600_000
    }
}

/// Spring-forward instant: second Sunday of March, 07:00 UTC.
fn dst_spring_utc_ms(year: i32) -> i64 {
    let first = days_from_civil(year, 3, 1);
    let to_sunday = (7 - weekday_of(first)) % 7;
    (first + to_sunday + 7) * 86_400_000 + 7 * 3_600_000
}

/// Fall-back instant: first Sunday of November, 06:00 UTC.
fn dst_fall_utc_ms(year: i32) -> i64 {
    let first = days_from_civil(year, 11, 1);
    let to_sunday = (7 - weekday_of(first)) % 7;
    (first + to_sunday) * 86_400_000 + 6 * 3_600_000
}

/// Monday = 1 through Sunday = 7 for a day count (day zero was a Thursday).
fn weekday_of(day_count: i64) -> i64 {
    (day_count.rem_euclid(7) + 3) % 7 + 1
}

fn et_wall_minutes(ts_ms: i64) -> u32 {
    let local = ts_ms + et_offset_ms(ts_ms);
    local.rem_euclid(86_400_000).div_euclid(60_000) as u32
}

fn et_civil_date(ts_ms: i64) -> (i32, u32, u32) {
    civil_from_days((ts_ms + et_offset_ms(ts_ms)).div_euclid(86_400_000))
}

/// Days since the Unix epoch for a proleptic Gregorian date.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let adj_year = i64::from(if month <= 2 { year - 1 } else { year });
    let era = adj_year.div_euclid(400);
    let yoe = adj_year - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Proleptic Gregorian date for days since the Unix epoch.
fn civil_from_days(day_count: i64) -> (i32, u32, u32) {
    let shifted = day_count + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let full_year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (if month <= 2 { full_year + 1 } else { full_year }) as i32;
    (year, month, day)
}

#[cfg(test)]
#[path = "../tests/session.rs"]
mod tests;
