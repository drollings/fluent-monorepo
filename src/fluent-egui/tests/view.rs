use super::*;
use chrono::TimeZone as _;
use protopyne_chart_model::{rows_of, to_display_float};
use protopyne_types::Bar;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive as _;

#[test]
fn pane_series_names_pane_and_style_key() {
    let series =
        PaneSeries::new("fast", PaneKind::Oscillator, vec![None, Some(1.0)], "fast").expect("valid");
    assert_eq!(series.len(), 2);
    assert!(!series.is_empty());
    assert!(PaneSeries::new("x", PaneKind::Price, Vec::new(), "bull").expect("valid").is_empty());
    assert!(PaneSeries::new("", PaneKind::Price, Vec::new(), "bull").is_err(), "empty name refuses");
    assert!(
        PaneSeries::new("x", PaneKind::Price, Vec::new(), "  ").is_err(),
        "empty style key refuses"
    );
}

#[test]
fn ribbon_edges_order_and_keys() {
    let fast = vec![None, Some(10.0), Some(4.0), Some(7.0)];
    let slow = vec![Some(9.0), Some(8.0), Some(6.0), None];
    let view = ribbon_view(&fast, &slow, 0.5).expect("valid ribbon");
    assert_eq!(view.upper, vec![None, Some(10.0), Some(6.0), None]);
    assert_eq!(view.lower, vec![None, Some(8.0), Some(4.0), None]);
    assert_eq!(view.bullish, vec![false, true, false, false]);
    assert!((view.alpha - 0.5).abs() < 1e-12, "alpha carries through");
    for index in 0..view.upper.len() {
        if let (Some(up), Some(lo)) = (view.upper[index], view.lower[index]) {
            assert!(up >= lo, "upper must cover lower at {index}");
        }
    }
    assert_eq!(ribbon_style_key(true), "bull");
    assert_eq!(ribbon_style_key(false), "bear");
}

#[test]
fn ribbon_fill_style_keys_map_to_teal_and_purple_slots() {
    // The shaded band resolves to its own theme slots, teal on bull runs and
    // purple on bear, while candle polarity keeps the bull/bear slots.
    assert_eq!(ribbon_fill_style_key(true), "ribbon_bull");
    assert_eq!(ribbon_fill_style_key(false), "ribbon_bear");
    assert_eq!(ribbon_style_key(true), "bull");
    assert_eq!(ribbon_style_key(false), "bear");
}

#[test]
fn ribbon_refuses_mismatched_lengths_and_bad_alpha() {
    assert!(ribbon_view(&[Some(1.0)], &[Some(1.0), Some(2.0)], 0.5).is_err());
    for bad in [f64::NAN, -0.1, 1.1] {
        assert!(ribbon_view(&[Some(1.0)], &[Some(1.0)], bad).is_err(), "alpha {bad} refuses");
    }
}

fn chart_row(volume: u64) -> ChartRow {
    ChartRow { time: 0, open: 100.0, high: 101.0, low: 99.0, close: 100.0, volume }
}

#[test]
fn delta_scales_by_causal_peak() {
    let rows = vec![chart_row(1_000), chart_row(4_000), chart_row(2_000)];
    let buy = vec![Some(600.0), Some(1_000.0), None];
    let sell = vec![Some(400.0), Some(3_000.0), Some(500.0)];
    let view = delta_candle_view(&rows, &buy, &sell, 300.0).expect("valid delta");
    assert_eq!(view.buy_top.len(), 3);
    // First bar peaks at its own volume: 600/1000 * 300.
    assert!((view.buy_top[0].expect("present") - 180.0).abs() < 1e-9);
    assert!((view.sell_bottom[0].expect("present") - 120.0).abs() < 1e-9);
    // Second bar peaks at 4000: 1000/4000 * 300.
    assert!((view.buy_top[1].expect("present") - 75.0).abs() < 1e-9);
    // Peak never shrinks, so replaying a prefix reproduces heights.
    let prefix = delta_candle_view(&rows[..1], &buy[..1], &sell[..1], 300.0).expect("prefix");
    assert_eq!(prefix.buy_top[0], view.buy_top[0]);
    assert_eq!(view.buy_top[2], None, "holes stay holes");
    for slot in view.buy_top.iter().flatten().chain(view.sell_bottom.iter().flatten()) {
        assert!(*slot >= 0.0 && *slot <= 300.0 + 1e-9, "bars stay within [0, unit]");
    }
}

#[test]
fn delta_with_zero_volume_yields_zero_height() {
    let rows = vec![chart_row(0)];
    let view =
        delta_candle_view(&rows, &[Some(0.0)], &[Some(0.0)], 300.0).expect("zero peak allowed");
    assert_eq!(view.buy_top, vec![Some(0.0)]);
    assert_eq!(view.sell_bottom, vec![Some(0.0)]);
}

#[test]
fn delta_refuses_mismatch_bad_unit_and_negative_splits() {
    let rows = vec![chart_row(100)];
    assert!(delta_candle_view(&rows, &[], &[Some(1.0)], 300.0).is_err(), "length mismatch refuses");
    for bad in [0.0, -1.0, f64::NAN] {
        assert!(
            delta_candle_view(&rows, &[Some(1.0)], &[Some(1.0)], bad).is_err(),
            "unit {bad} refuses"
        );
    }
    assert!(
        delta_candle_view(&rows, &[Some(-1.0)], &[Some(1.0)], 300.0).is_err(),
        "negative buy refuses"
    );
    assert!(
        delta_candle_view(&rows, &[Some(1.0)], &[Some(-1.0)], 300.0).is_err(),
        "negative sell refuses"
    );
}

fn bar_at(day: u32, px: &str) -> Bar {
    Bar::new()
        .ts(chrono::Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0).single().expect("valid time"))
        .o(px.parse::<Decimal>().expect("valid decimal"))
        .h(px.parse::<Decimal>().expect("valid decimal"))
        .l(px.parse::<Decimal>().expect("valid decimal"))
        .c(px.parse::<Decimal>().expect("valid decimal"))
        .volume(100_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn dropped_count_is_conserved_into_views() {
    let bars = vec![bar_at(2, "100"), bar_at(3, "101"), bar_at(4, "102")];
    let converted = rows_of(&bars);
    assert_eq!(
        converted.rows.len() as u64 + converted.dropped,
        bars.len() as u64,
        "conversion conserves bars: kept + dropped == input"
    );
    // Views over converted rows never resurrect dropped bars.
    let closes: Vec<Option<f64>> = converted.rows.iter().map(|row| Some(row.close)).collect();
    assert_eq!(closes.len(), converted.rows.len());
    let pane = PaneSeries::new("close", PaneKind::Price, closes, "price").expect("valid series");
    assert_eq!(pane.len(), converted.rows.len());
    for row in &converted.rows {
        for leg in [row.open, row.high, row.low, row.close] {
            assert!(leg.is_finite(), "converted legs are finite display floats");
            assert_eq!(to_display_float(px_decimal(leg)), Some(leg), "legs round-trip exactly");
        }
    }
}

fn px_decimal(display: f64) -> Decimal {
    Decimal::from_f64(display).expect("display floats convert here")
}

#[test]
fn pivot_marks_fire_only_on_crossings_sign_correct() {
    // Drops below +100 -> High at 1 and 5; rises above -100 -> Low at 3.
    // Index 2 and 4 start inside the band, so no crossing fires there.
    let fast =
        vec![Some(150.0), Some(90.0), Some(-120.0), Some(-80.0), Some(140.0), Some(60.0)];
    let marks = pivot_marks(&fast, 100.0).expect("valid threshold");
    assert_eq!(
        marks,
        vec![
            PivotMark { index: 1, side: PivotSide::High },
            PivotMark { index: 3, side: PivotSide::Low },
            PivotMark { index: 5, side: PivotSide::High },
        ]
    );
}

#[test]
fn pivot_marks_ignore_holes_and_flat_runs() {
    // Hole at 1 breaks the pair that would otherwise cross at 2.
    let with_hole = vec![Some(150.0), None, Some(90.0), Some(95.0)];
    assert!(pivot_marks(&with_hole, 100.0).expect("valid").is_empty());
    // Flat chop under the threshold never fires.
    let flat = vec![Some(10.0), Some(-20.0), Some(30.0), Some(-40.0), Some(50.0)];
    assert!(pivot_marks(&flat, 100.0).expect("valid").is_empty());
    // Touching the threshold without crossing never fires.
    let touch = vec![Some(150.0), Some(100.0), Some(150.0)];
    assert!(pivot_marks(&touch, 100.0).expect("valid").is_empty());
}

#[test]
fn pivot_refuses_bad_threshold() {
    for bad in [0.0, -100.0, f64::NAN, f64::INFINITY] {
        assert!(pivot_marks(&[Some(1.0)], bad).is_err(), "threshold {bad} refuses");
    }
    assert!(pivot_marks(&[], 100.0).expect("empty is valid").is_empty());
}
