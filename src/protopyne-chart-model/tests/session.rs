use super::*;
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn dec(s: &str) -> rust_decimal::Decimal {
    s.parse().expect("valid test decimal")
}

fn bar_at(y: i32, m: u32, d: u32, h: u32) -> Bar {
    Bar::new()
        .ts(chrono::Utc.with_ymd_and_hms(y, m, d, h, 0, 0).single().expect("valid test time"))
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
    let bars = vec![bar_at(2026, 1, 1, 12), bar_at(2026, 1, 2, 12), bar_at(2026, 1, 2, 13)];
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
