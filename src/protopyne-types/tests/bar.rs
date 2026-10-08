use super::*;
use chrono::TimeZone as _;

fn sample_ts() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(2026, 1, 2, 14, 30, 0)
        .single()
        .expect("valid test timestamp")
}

fn dec(s: &str) -> rust_decimal::Decimal {
    s.parse().expect("valid test decimal")
}

fn sample_bar() -> Bar {
    Bar::new()
        .ts(sample_ts())
        .o(dec("100.00"))
        .h(dec("101.50"))
        .l(dec("99.25"))
        .c(dec("100.75"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn bar_serde_round_trip() {
    let bar = sample_bar();
    let json = serde_json::to_string(&bar).expect("serialize bar");
    let back: Bar = serde_json::from_str(&json).expect("deserialize bar");
    assert_eq!(back, bar);
}

#[test]
fn bar_with_adjustment_serde_round_trip() {
    let bar = Bar::new()
        .ts(sample_ts())
        .o(dec("100.00"))
        .h(dec("101.50"))
        .l(dec("99.25"))
        .c(dec("100.75"))
        .volume(1_000_u64)
        .maybe_adj(Some(
            Adjustment::new().maybe_split_factor(Some(dec("2"))).build(),
        ))
        .build();
    let json = serde_json::to_string(&bar).expect("serialize");
    let back: Bar = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, bar);
}
