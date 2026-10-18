use super::*;
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn dec(s: &str) -> rust_decimal::Decimal {
    s.parse().expect("valid test decimal")
}

fn bar_at(y: i32, m: u32, d: u32, h: u32) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .with_ymd_and_hms(y, m, d, h, 0, 0)
            .single()
            .expect("valid test time"))
        .o(dec("100"))
        .h(dec("101"))
        .l(dec("99"))
        .c(dec("100.5"))
        .volume(10_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn daily_breaks_fire_on_day_change() {
    let provider = DailySessionProvider::continuous();
    let bars = vec![
        bar_at(2026, 1, 1, 12),
        bar_at(2026, 1, 2, 12),
        bar_at(2026, 1, 2, 13),
    ];
    let breaks = find_session_breaks(&bars, &provider);
    assert_eq!(breaks.len(), 1);
    assert_eq!(breaks[0].0, 1);
    assert_eq!(breaks[0].1.break_type, SessionBreakType::Daily);
}

#[test]
fn monthly_breaks_fire_on_month_change() {
    let bars = vec![bar_at(2026, 1, 31, 12), bar_at(2026, 2, 1, 12)];
    let breaks = find_session_breaks(&bars, &MonthlySessionProvider);
    assert_eq!(breaks.len(), 1);
    assert_eq!(breaks[0].1.break_type, SessionBreakType::Monthly);
}

#[test]
fn weekly_breaks_fire_on_iso_week_change() {
    // 2026-01-04 is a Sunday; 2026-01-05 starts a new ISO week.
    let bars = vec![bar_at(2026, 1, 4, 12), bar_at(2026, 1, 5, 12)];
    let breaks = find_session_breaks(&bars, &WeeklySessionProvider::default());
    assert_eq!(breaks.len(), 1);
    assert_eq!(breaks[0].1.break_type, SessionBreakType::Weekly);
}

#[test]
fn no_break_inside_one_session() {
    let bars = vec![bar_at(2026, 1, 2, 10), bar_at(2026, 1, 2, 11)];
    let breaks = find_session_breaks(&bars, &DailySessionProvider::continuous());
    assert!(breaks.is_empty());
}

#[test]
fn composite_first_match_wins() {
    let mut composite = CompositeSessionProvider::new();
    composite.add_provider(Box::new(DailySessionProvider::continuous()));
    composite.add_provider(Box::new(MonthlySessionProvider));
    // Month change is also a day change; daily is consulted first.
    let bars = vec![bar_at(2026, 1, 31, 12), bar_at(2026, 2, 1, 12)];
    let breaks = find_session_breaks(&bars, &composite);
    assert_eq!(breaks.len(), 1);
    assert_eq!(breaks[0].1.break_type, SessionBreakType::Daily);
}

#[test]
fn standard_wires_three_providers() {
    let composite = CompositeSessionProvider::standard();
    let bars = vec![bar_at(2026, 1, 31, 12), bar_at(2026, 2, 1, 12)];
    assert_eq!(find_session_breaks(&bars, &composite).len(), 1);
}

#[test]
fn constructor_clamps_clock_fields() {
    let provider = DailySessionProvider::new(99, 99);
    assert_eq!(provider.break_hour, 23);
    assert_eq!(provider.break_minute, 59);
    assert_eq!(DailySessionProvider::nyse().break_hour, 21);
}

fn rth() -> EtSessionProvider {
    EtSessionProvider::new(protopyne_types::TradingSession::parse("0930-1600").expect("rth parses"))
}

#[test]
fn et_breaks_follow_et_dates_not_utc_days() {
    let provider = rth();
    // 23:30 UTC Jan 1 == 18:30 ET Jan 1; 00:30 UTC Jan 2 == 19:30 ET Jan 1:
    // the UTC day changes but the ET date does not.
    let bars = vec![bar_at(2026, 1, 1, 23), bar_at(2026, 1, 2, 0)];
    assert!(find_session_breaks(&bars, &provider).is_empty());
    assert_eq!(
        find_session_breaks(&bars, &DailySessionProvider::continuous()).len(),
        1,
        "utc provider splits where the ET date holds"
    );
}

#[test]
fn et_break_fires_where_utc_holds() {
    let provider = rth();
    // 04:30 UTC Jan 2 == 23:30 ET Jan 1; 05:30 UTC Jan 2 == 00:30 ET Jan 2.
    let bars = vec![bar_at(2026, 1, 2, 4), bar_at(2026, 1, 2, 5)];
    let breaks = find_session_breaks(&bars, &provider);
    assert_eq!(breaks.len(), 1);
    assert_eq!(breaks[0].0, 1);
    assert_eq!(breaks[0].1.break_type, SessionBreakType::Daily);
    assert_eq!(breaks[0].1.label.as_deref(), Some("2026-01-02 ET"));
    assert!(
        find_session_breaks(&bars, &DailySessionProvider::continuous()).is_empty(),
        "utc provider holds where the ET date changes"
    );
}

#[test]
fn et_breaks_respect_daylight_offset() {
    let provider = rth();
    // July: EDT (UTC-4). 03:30 UTC Jul 9 == 23:30 ET Jul 8;
    // 04:30 UTC Jul 9 == 00:30 ET Jul 9.
    let bars = vec![bar_at(2026, 7, 9, 3), bar_at(2026, 7, 9, 4)];
    let breaks = find_session_breaks(&bars, &provider);
    assert_eq!(
        breaks.len(),
        1,
        "daylight offset moves the break off utc midnight"
    );
    assert_eq!(breaks[0].1.label.as_deref(), Some("2026-07-09 ET"));
}

#[test]
fn overnight_sessions_refuse_at_parse() {
    assert!(protopyne_types::TradingSession::parse("1600-0930").is_err());
    assert!(protopyne_types::TradingSession::parse("0930-0930").is_err());
}

#[test]
fn timestamp_slice_scan_matches_the_bar_scan() {
    // One pairwise core: the timestamp entry and the bar entry agree break
    // for break, so row-time callers and bar callers can never drift apart.
    let bars = vec![
        bar_at(2026, 1, 1, 12),
        bar_at(2026, 1, 2, 1),
        bar_at(2026, 1, 2, 12),
        bar_at(2026, 2, 1, 12),
    ];
    let provider = DailySessionProvider::continuous();
    let times: Vec<_> = bars.iter().map(|bar| bar.ts).collect();
    assert_eq!(
        session_breaks_between(&times, &provider),
        find_session_breaks(&bars, &provider)
    );
    assert!(session_breaks_between(&[], &provider).is_empty());
    assert!(session_breaks_between(&times[..1], &provider).is_empty());
}
