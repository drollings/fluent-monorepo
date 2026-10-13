//! Wall-clock predicate goldens: session gating, daylight flips, ET dates,
//! and overnight refusal. Moved with the clock from the kernels bar suite;
//! behavior identical, only the error type changed.

use super::*;

#[test]
fn sessions_gate_rth_and_dst() {
    use chrono::TimeZone as _;
    let session = TradingSession::parse("0930-1600").expect("rth parses");
    let stamp = |y: i32, mo: u32, d: u32, h: u32, mi: u32| {
        chrono::Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).single().expect("valid stamp").timestamp_millis()
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
    assert_eq!(session.session_date(stamp(2026, 1, 10, 0, 30)), (2026, 1, 9));
    for bad in ["0930", "0930-1600-1700", "abcd-efgh", "1600-0930", "0930-0930", "2400-2500", ""] {
        assert!(TradingSession::parse(bad).is_err(), "session {bad:?} must refuse");
    }
}
