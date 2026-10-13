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
use crate::validate::Validate;

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
        let bad = || {
            TypesError::InvalidSession(format!("session must read HHMM-HHMM, got {session:?}"))
        };
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
        Ok(Self { open_min, close_min })
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
    if is_dst_et(ts_ms) { -4 * 3_600_000 } else { -5 * 3_600_000 }
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
