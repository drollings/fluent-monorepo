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

#[test]
fn open_precedes_the_confirmed_close() {
    // Deterministic sweep rather than a measured tape: every generated bar
    // opens before it confirms, and the two accessors name the fields they
    // read.
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state
    };
    for _ in 0..256 {
        let open_ms = (next() % 4_000_000_000) as i64;
        let span_ms = (next() % 3_600_000 + 1) as i64;
        let open = chrono::DateTime::from_timestamp_millis(open_ms).expect("open in range");
        let close =
            chrono::DateTime::from_timestamp_millis(open_ms + span_ms).expect("close in range");
        let bar = Bar::new()
            .ts(open)
            .o(dec("100.00"))
            .h(dec("101.00"))
            .l(dec("99.00"))
            .c(dec("100.50"))
            .volume(1_u64)
            .maybe_adj(None)
            .maybe_confirmed_at(Some(close))
            .build();
        assert_eq!(bar.open_time(), bar.ts, "the open is the stamp");
        assert_eq!(bar.confirmed_at(), Some(close), "the close is recorded");
        assert!(bar.open_time() <= bar.ts);
        assert!(bar.ts < bar.confirmed_at().expect("close present"));
    }
}

#[test]
fn the_confirmed_close_is_not_part_of_the_wire_shape() {
    let plain = sample_bar();
    assert_eq!(plain.confirmed_at(), None, "an unrecorded close is absent");
    let json = serde_json::to_string(&plain).expect("serialize plain bar");
    assert!(!json.contains("confirmed_at"), "the field never serializes");

    let close = sample_ts() + chrono::Duration::minutes(1);
    let confirmed = Bar::new()
        .ts(sample_ts())
        .o(dec("100.00"))
        .h(dec("101.50"))
        .l(dec("99.25"))
        .c(dec("100.75"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .maybe_confirmed_at(Some(close))
        .build();
    let json = serde_json::to_string(&confirmed).expect("serialize confirmed bar");
    assert!(!json.contains("confirmed_at"), "the field never serializes");

    let back: Bar = serde_json::from_str(&json).expect("deserialize close-free bar");
    assert_eq!(back.confirmed_at(), None, "a wire bar carries no close");
}
