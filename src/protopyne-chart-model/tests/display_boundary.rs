use super::*;
use chrono::TimeZone as _;

fn dec(s: &str) -> Decimal {
    s.parse().expect("valid test decimal")
}

fn bar_with(close: Decimal) -> protopyne_types::Bar {
    protopyne_types::Bar::new()
        .ts(chrono::Utc
            .with_ymd_and_hms(2026, 1, 2, 12, 0, 0)
            .single()
            .expect("valid time"))
        .o(dec("100"))
        .h(dec("101"))
        .l(dec("99"))
        .c(close)
        .volume(100_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn decimal_max_still_converts_finite() {
    let max = Decimal::MAX;
    let got = to_display_float(max);
    assert_eq!(
        got,
        Some(7.922816251426434e28),
        "MAX decimal converts, never drops"
    );
}

#[test]
fn valid_bars_never_drop() {
    let bars = vec![
        bar_with(dec("100.5")),
        bar_with(Decimal::MAX),
        bar_with(Decimal::ZERO),
    ];
    let out = rows_of(&bars);
    assert_eq!(out.rows.len(), 3);
    assert_eq!(out.dropped, 0);
}

#[test]
fn zero_and_negative_legs_convert() {
    assert_eq!(to_display_float(Decimal::ZERO), Some(0.0));
    assert_eq!(to_display_float(dec("-12.25")), Some(-12.25));
}
