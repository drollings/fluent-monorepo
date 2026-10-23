//! Wall-clock predicate goldens: session gating, daylight flips, ET dates,
//! and overnight refusal. Moved with the clock from the kernels bar suite;
//! behavior identical, only the error type changed.

use super::*;

#[test]
fn sessions_gate_rth_and_dst() {
    use chrono::TimeZone as _;
    let session = TradingSession::parse("0930-1600").expect("rth parses");
    let stamp = |y: i32, mo: u32, d: u32, h: u32, mi: u32| {
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid stamp")
            .timestamp_millis()
    };
    // Standard time (EST, UTC-5): 09:30 ET == 14:30 UTC.
    assert!(!session.contains(stamp(2026, 1, 9, 14, 29)));
    assert!(session.contains(stamp(2026, 1, 9, 14, 30)));
    assert!(session.contains(stamp(2026, 1, 9, 20, 59)));
    assert!(!session.contains(stamp(2026, 1, 9, 21, 0)));
    // Daylight time (EDT, UTC-4): 09:30 ET == 13:30 UTC.
    assert!(!session.contains(stamp(2026, 7, 9, 13, 29)));
    assert!(session.contains(stamp(2026, 7, 9, 13, 30)));
    assert!(!session.contains(stamp(2026, 7, 9, 20, 0)));
    // Spring forward: second Sunday of March 2026 is the 8th; 07:00 UTC flips.
    assert!(!is_dst_et(stamp(2026, 3, 8, 6, 59)));
    assert!(is_dst_et(stamp(2026, 3, 8, 7, 0)));
    assert_eq!(et_offset_ms(stamp(2026, 3, 8, 6, 59)), -5 * 3_600_000);
    assert_eq!(et_offset_ms(stamp(2026, 3, 8, 7, 0)), -4 * 3_600_000);
    // Fall back: first Sunday of November 2026 is the 1st; 06:00 UTC flips.
    assert!(is_dst_et(stamp(2026, 11, 1, 5, 59)));
    assert!(!is_dst_et(stamp(2026, 11, 1, 6, 0)));
    // Session dates follow ET wall clock, not UTC days.
    assert_eq!(
        session.session_date(stamp(2026, 1, 10, 0, 30)),
        (2026, 1, 9)
    );
    for bad in [
        "0930",
        "0930-1600-1700",
        "abcd-efgh",
        "1600-0930",
        "0930-0930",
        "2400-2500",
        "",
    ] {
        assert!(
            TradingSession::parse(bad).is_err(),
            "session {bad:?} must refuse"
        );
    }
}

/// Weekend/holiday-aware session calendar: regular hours on weekdays,
/// full closes on listed dates, early closes on half days. Weekends never
/// trade however the dates fall; daylight flips ride the shared ET clock.
fn equities_calendar() -> SessionCalendar {
    SessionCalendar::us_equities()
        .with_holiday((2026, 11, 26))
        .with_half_day(
            (2026, 11, 27),
            TradingSession::parse("0930-1300").expect("half parses"),
        )
}

#[test]
fn calendar_gates_weekends_holidays_and_half_days() {
    use chrono::TimeZone as _;
    let stamp = |y: i32, mo: u32, d: u32, h: u32, mi: u32| {
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid stamp")
            .timestamp_millis()
    };
    let calendar = equities_calendar();
    // A regular Friday session: 09:30–16:00 ET == 13:30–20:00 UTC (EDT).
    assert!(calendar.contains(stamp(2026, 6, 5, 13, 30)));
    assert!(calendar.contains(stamp(2026, 6, 5, 19, 59)));
    assert!(!calendar.contains(stamp(2026, 6, 5, 20, 0)));
    // Weekends never trade.
    assert!(!calendar.contains(stamp(2026, 6, 6, 15, 0)));
    assert!(!calendar.contains(stamp(2026, 6, 7, 15, 0)));
    // Thanksgiving full close.
    assert!(!calendar.contains(stamp(2026, 11, 26, 15, 0)));
    // Half day: open 09:30, closed from 13:00 ET == 18:00 UTC (EST).
    assert!(calendar.contains(stamp(2026, 11, 27, 17, 59)));
    assert!(!calendar.contains(stamp(2026, 11, 27, 18, 0)));
    // DST flip mid-fixture: Friday EST, Monday EDT, both full sessions.
    assert!(calendar.contains(stamp(2026, 3, 6, 19, 59)));
    assert!(calendar.contains(stamp(2026, 3, 9, 13, 30)));
    assert_eq!(et_weekday(stamp(2026, 6, 5, 12, 0)), 5, "Friday is 5");
    assert_eq!(et_weekday(stamp(2026, 6, 7, 12, 0)), 7, "Sunday is 7");
}

#[test]
fn calendar_counts_only_trading_buckets_between_stamps() {
    use chrono::TimeZone as _;
    let stamp = |y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32| {
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .single()
            .expect("valid stamp")
            .timestamp_millis()
    };
    let calendar = equities_calendar();
    let m1 = 60_000;
    // Contiguous minute bars: nothing between.
    assert_eq!(
        calendar.trading_buckets_between(
            stamp(2026, 6, 3, 14, 0, 0),
            stamp(2026, 6, 3, 14, 1, 0),
            m1
        ),
        0
    );
    // One dropped minute bucket intraday: exactly one trading gap.
    assert_eq!(
        calendar.trading_buckets_between(
            stamp(2026, 6, 3, 14, 0, 0),
            stamp(2026, 6, 3, 14, 2, 0),
            m1
        ),
        1
    );
    // Overnight Friday close to Monday open: the weekend counts nothing.
    assert_eq!(
        calendar.trading_buckets_between(
            stamp(2026, 6, 5, 19, 59, 0),
            stamp(2026, 6, 8, 13, 30, 0),
            m1
        ),
        0
    );
    // DST weekend: Friday EST close to Monday EDT open counts nothing.
    assert_eq!(
        calendar.trading_buckets_between(
            stamp(2026, 3, 6, 20, 59, 0),
            stamp(2026, 3, 9, 13, 30, 0),
            m1
        ),
        0
    );
    // Half-day close to Monday open: Friday afternoon and the weekend count
    // nothing.
    assert_eq!(
        calendar.trading_buckets_between(
            stamp(2026, 11, 27, 17, 59, 0),
            stamp(2026, 11, 30, 14, 30, 0),
            m1
        ),
        0
    );
    // Thanksgiving full close inside the span counts nothing either:
    // Wednesday's 15:59 ET bar to Friday's 09:30 ET open skips only the
    // Wednesday close, the Thursday holiday, and Friday pre-open.
    assert_eq!(
        calendar.trading_buckets_between(
            stamp(2026, 11, 25, 20, 59, 0),
            stamp(2026, 11, 27, 14, 30, 0),
            m1
        ),
        0
    );
    // Degenerate inputs refuse by returning zero, never a span.
    assert_eq!(calendar.trading_buckets_between(100, 100, m1), 0);
    assert_eq!(calendar.trading_buckets_between(200, 100, m1), 0);
    assert_eq!(calendar.trading_buckets_between(100, 200, 0), 0);
}

#[test]
fn calendar_freeze_pins_regular_hours_and_dst_offsets() {
    use chrono::TimeZone as _;
    let stamp = |y: i32, mo: u32, d: u32, h: u32, mi: u32| {
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid stamp")
            .timestamp_millis()
    };
    let calendar = SessionCalendar::us_equities();
    // Regular US equity hours are frozen: 09:30–16:00 ET.
    assert!(calendar.contains(stamp(2026, 6, 3, 13, 30)));
    assert!(!calendar.contains(stamp(2026, 6, 3, 13, 29)));
    assert!(calendar.contains(stamp(2026, 6, 3, 19, 59)));
    assert!(!calendar.contains(stamp(2026, 6, 3, 20, 0)));
    // Winter offset is UTC-5, summer UTC-4: the clock both crates read.
    assert_eq!(et_offset_ms(stamp(2026, 1, 15, 12, 0)), -5 * 3_600_000);
    assert_eq!(et_offset_ms(stamp(2026, 6, 15, 12, 0)), -4 * 3_600_000);
}

#[test]
fn next_close_after_clips_a_span_to_the_session_close() {
    use chrono::TimeZone as _;
    const DAY_MS: i64 = 86_400_000;
    let stamp = |y: i32, mo: u32, d: u32, h: u32, mi: u32| {
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("valid stamp")
            .timestamp_millis()
    };
    let calendar = SessionCalendar::us_equities();

    // A one-minute bar in the middle of the session crosses no close.
    assert_eq!(
        calendar.next_close_after(stamp(2026, 3, 6, 15, 0), 60_000),
        None
    );

    // The last minute of the session is clipped to it.
    assert_eq!(
        calendar.next_close_after(stamp(2026, 3, 6, 20, 59), 60_000),
        Some(stamp(2026, 3, 6, 21, 0))
    );

    // A daily bucket stamped at UTC midnight closes at Friday's session
    // close — not at the next bucket's Monday open.
    assert_eq!(
        calendar.next_close_after(stamp(2026, 3, 6, 0, 0), DAY_MS),
        Some(stamp(2026, 3, 6, 21, 0))
    );

    // The Monday after the spring transition closes an hour earlier in UTC
    // than the Friday before it.
    assert_eq!(
        calendar.next_close_after(stamp(2026, 3, 9, 0, 0), DAY_MS),
        Some(stamp(2026, 3, 9, 20, 0))
    );

    // A holiday shifts the close to the next session, and an empty horizon
    // never reports one.
    let with_holiday = SessionCalendar::us_equities().with_holiday((2026, 3, 9));
    assert_eq!(
        with_holiday.next_close_after(stamp(2026, 3, 9, 0, 0), DAY_MS),
        None,
        "no session closes inside that day"
    );
    assert_eq!(calendar.next_close_after(stamp(2026, 3, 6, 15, 0), 0), None);
}
