use super::*;
use crate::adjust_fetch_bounds;
use chrono::TimeZone as _;
use rust_decimal::Decimal;

fn dec(s: &str) -> Decimal {
    s.parse().expect("valid test decimal")
}

fn ts(minute: u32) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(2026, 1, 2, 9, minute, 0)
        .single()
        .expect("valid test timestamp")
}

fn bar_at(minute: u32) -> Bar {
    Bar::new()
        .ts(ts(minute))
        .o(dec("100.00"))
        .h(dec("101.00"))
        .l(dec("99.00"))
        .c(dec("100.50"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn series_placeholder() {
    // Scaffolding placeholder: replaced by the real series suites below.
    assert_eq!(PriceSource::all().len(), 7);
}

#[test]
fn price_source_covers_all_seven_legs() {
    assert_eq!(PriceSource::all().len(), 7);
    let (o, h, l, c) = (dec("10"), dec("12"), dec("8"), dec("11"));
    assert_eq!(PriceSource::Open.compute(o, h, l, c), o);
    assert_eq!(PriceSource::High.compute(o, h, l, c), h);
    assert_eq!(PriceSource::Low.compute(o, h, l, c), l);
    assert_eq!(PriceSource::Close.compute(o, h, l, c), c);
    assert_eq!(PriceSource::Hl2.compute(o, h, l, c), dec("10"));
    assert_eq!(PriceSource::Hlc3.compute(o, h, l, c), dec("31") / dec("3"));
    assert_eq!(PriceSource::Ohlc4.compute(o, h, l, c), dec("41") / dec("4"));
    assert_eq!(PriceSource::Close.label(), "Close");
    assert_eq!(PriceSource::default(), PriceSource::Close);
}

#[test]
fn invalid_bars_are_refused() {
    // High below the close.
    let bad_high = Bar::new()
        .ts(ts(0))
        .o(dec("100"))
        .h(dec("99"))
        .l(dec("98"))
        .c(dec("100"))
        .volume(1_u64)
        .maybe_adj(None)
        .build();
    assert!(bad_high.validate().is_err());
    // Low above the open.
    let bad_low = Bar::new()
        .ts(ts(0))
        .o(dec("100"))
        .h(dec("102"))
        .l(dec("101"))
        .c(dec("100"))
        .volume(1_u64)
        .maybe_adj(None)
        .build();
    assert!(bad_low.validate().is_err());
    // Flat bar (all equal) is structurally fine.
    let flat = Bar::new()
        .ts(ts(0))
        .o(dec("100"))
        .h(dec("100"))
        .l(dec("100"))
        .c(dec("100"))
        .volume(0_u64)
        .maybe_adj(None)
        .build();
    assert!(flat.validate().is_ok());
    // The window refuses invalid bars on push, not just on inspection.
    let mut window = BarWindow::new(Timeframe::M15);
    assert!(window.push(bad_high).is_err());
    assert!(window.is_empty());
}

#[test]
fn incremental_validation_refuses_duplicates() {
    let first = bar_at(0);
    assert!(validate_new_bar(None, &first).is_ok());
    assert!(validate_new_bar(Some(&first), &first).is_err());
    let earlier = bar_at(0);
    let later = Bar::new()
        .ts(ts(0) - chrono::Duration::minutes(15))
        .o(dec("100.00"))
        .h(dec("101.00"))
        .l(dec("99.00"))
        .c(dec("100.50"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build();
    assert!(validate_new_bar(Some(&earlier), &later).is_err());
}

#[test]
fn gap_detection_ms_jitter_and_zero_missing_edges() {
    // Sub-second jitter around exact spacing reports no gap.
    let mut window = BarWindow::new(Timeframe::M15);
    let t0 = chrono::Utc
        .with_ymd_and_hms(2026, 1, 2, 9, 0, 0)
        .single()
        .expect("valid test timestamp")
        + chrono::Duration::milliseconds(500);
    for step in 0..3 {
        let bar = Bar::new()
            .ts(t0 + chrono::Duration::minutes(15 * step))
            .o(dec("100.00"))
            .h(dec("101.00"))
            .l(dec("99.00"))
            .c(dec("100.50"))
            .volume(1_000_u64)
            .maybe_adj(None)
            .build();
        window.push(bar).expect("in-order push");
    }
    assert!(!window.has_gaps());
    assert!(window.gaps().is_empty());
    // A sub-step slip reports a gap with zero whole bars missing.
    let mut slipped = BarWindow::new(Timeframe::M15);
    slipped.push(bar_at(0)).expect("first bar");
    let mut second = bar_at(15);
    second.ts = ts(15) + chrono::Duration::seconds(1);
    slipped.push(second).expect("second bar");
    let gaps = slipped.gaps();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].missing_bars, 0);
}

#[test]
fn warmup_fetch_bounds_overflow_saturates() {
    // Extreme warmups saturate instead of overflowing.
    let bounds = adjust_fetch_bounds(Some(1_000_000), Some(100), Timeframe::M15, usize::MAX);
    assert_eq!(bounds.fetch_start_ms, Some(0));
    assert!(bounds.fetch_limit.expect("limit present") >= 100);
    assert_eq!(bounds.original_start_ms, Some(1_000_000));
    assert_eq!(bounds.original_limit, Some(100));
    // Ordinary inputs are untouched by the saturation.
    let bounds = adjust_fetch_bounds(Some(100_000_000), Some(100), Timeframe::M15, 14);
    assert_eq!(bounds.fetch_start_ms, Some(100_000_000 - 13 * 900_000));
    assert_eq!(bounds.fetch_limit, Some(113));
}

#[test]
fn timeframe_durations_and_resolutions() {
    assert_eq!(Timeframe::M15.duration_ms(), 900_000);
    assert_eq!(Timeframe::H1.total_seconds(), 3_600);
    assert_eq!(Timeframe::D1.duration(), chrono::Duration::hours(24));
    assert_eq!(Timeframe::from_resolution("15"), Some(Timeframe::M15));
    assert_eq!(Timeframe::from_resolution("1H"), Some(Timeframe::H1));
    assert_eq!(Timeframe::from_resolution("1D"), Some(Timeframe::D1));
    assert_eq!(Timeframe::from_resolution("nope"), None);
}

#[test]
fn series_context_enforces_gap_policy() {
    let mut sparse = SeriesContext::new("SPY", Timeframe::M15, GapPolicy::Sparse);
    sparse.push(bar_at(0)).expect("first bar");
    sparse.push(bar_at(30)).expect("sparse tolerates the hole");
    assert_eq!(sparse.len(), 2);
    assert_eq!(sparse.symbol(), "SPY");
    let closes = sparse.source_series(PriceSource::Close);
    assert_eq!(closes, vec![dec("100.50"), dec("100.50")]);

    let mut dense = SeriesContext::new("SPY", Timeframe::M15, GapPolicy::Dense);
    dense.push(bar_at(0)).expect("first bar");
    assert!(dense.push(bar_at(30)).is_err(), "dense refuses the hole");
    dense.push(bar_at(15)).expect("contiguous bar accepted");
    let view = dense.ltf_view(Timeframe::H1);
    assert_eq!(view.timeframe(), Timeframe::H1);
    assert_eq!(view.len(), 2);
    assert_eq!(view.source_series(PriceSource::Hl2).len(), 2);
}
