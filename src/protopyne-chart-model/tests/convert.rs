use super::*;
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn dec(s: &str) -> Decimal {
    s.parse().expect("valid test decimal")
}

fn ts(year: i32, month: u32, day: u32) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(year, month, day, 12, 0, 0)
        .single()
        .expect("valid test time")
}

fn bar_at(year: i32, month: u32, day: u32, open: &str, high: &str, low: &str, close: &str) -> Bar {
    Bar::new()
        .ts(ts(year, month, day))
        .o(dec(open))
        .h(dec(high))
        .l(dec(low))
        .c(dec(close))
        .volume(100_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn finite_legs_convert() {
    assert_eq!(to_display_float(dec("100.50")), Some(100.5));
    assert_eq!(to_display_float(Decimal::ZERO), Some(0.0));
}

#[test]
fn row_of_keeps_time_and_volume() {
    let bar = bar_at(2026, 1, 2, "100", "101", "99", "100.5");
    let row = row_of(&bar).expect("valid bar converts");
    assert_eq!(row.time, bar.ts.timestamp());
    assert_eq!(row.volume, 100);
    assert!((row.open - 100.0).abs() < 1e-9);
    assert!((row.close - 100.5).abs() < 1e-9);
}

#[test]
fn rows_of_counts_without_failing() {
    let bars = vec![
        bar_at(2026, 1, 2, "100", "101", "99", "100.5"),
        bar_at(2026, 1, 3, "101", "102", "100", "101"),
    ];
    let out = rows_of(&bars);
    assert_eq!(out.rows.len(), 2);
    assert_eq!(out.dropped, 0);
}

#[test]
fn rows_of_empty_is_empty() {
    let out = rows_of(&[]);
    assert!(out.rows.is_empty());
    assert_eq!(out.dropped, 0);
}

#[test]
fn row_json_round_trip() {
    let bar = bar_at(2026, 1, 2, "100", "101", "99", "100.5");
    let row = row_of(&bar).expect("valid bar converts");
    let json = serde_json::to_string(&row).expect("serialize row");
    let back: ChartRow = serde_json::from_str(&json).expect("deserialize row");
    assert_eq!(back, row);
}
