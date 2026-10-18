use super::*;
use chrono::{DateTime, TimeZone as _, Utc};

fn bar_at_15m(step: i64) -> Bar {
    let ts: DateTime<Utc> = Utc
        .with_ymd_and_hms(2026, 1, 2, 9, 0, 0)
        .single()
        .expect("valid test timestamp")
        + chrono::Duration::minutes(15 * step);
    Bar::new()
        .ts(ts)
        .o("100.00".parse().expect("valid decimal"))
        .h("101.00".parse().expect("valid decimal"))
        .l("99.00".parse().expect("valid decimal"))
        .c("100.50".parse().expect("valid decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn clean_window_has_no_gaps() {
    let mut window = BarWindow::new(Timeframe::M15);
    for step in 0..5 {
        window.push(bar_at_15m(step)).expect("in-order push");
    }
    assert_eq!(window.len(), 5);
    assert!(!window.is_empty());
    assert!(!window.has_gaps());
    assert!(window.gaps().is_empty());
}

#[test]
fn flags_synthetic_missing_bar() {
    let mut window = BarWindow::new(Timeframe::M15);
    for step in [0, 1, 3, 4] {
        window.push(bar_at_15m(step)).expect("in-order push");
    }
    let gaps = window.gaps();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].at_index, 2);
    assert_eq!(gaps[0].missing_bars, 1);
    assert_eq!(gaps[0].expected_ts, bar_at_15m(2).ts);
    assert_eq!(gaps[0].actual_ts, bar_at_15m(3).ts);
    assert!(window.has_gaps());
}

#[test]
fn rejects_out_of_order_push() {
    let mut window = BarWindow::new(Timeframe::M15);
    window.push(bar_at_15m(1)).expect("first push");
    let err = window.push(bar_at_15m(0)).expect_err("must reject rewind");
    assert!(matches!(err, TypesError::InvalidBar(_)));
    let dup = window
        .push(bar_at_15m(1))
        .expect_err("must reject duplicate ts");
    assert!(matches!(dup, TypesError::InvalidBar(_)));
    assert_eq!(window.len(), 1);
}
