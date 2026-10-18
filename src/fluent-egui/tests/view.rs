use super::*;
use chrono::TimeZone as _;
use protopyne_chart_model::{rows_of, to_display_float};
use protopyne_types::Bar;
use rust_decimal::prelude::FromPrimitive as _;
use rust_decimal::Decimal;

#[test]
fn pane_series_names_pane_and_style_key() {
    let series = PaneSeries::new("fast", PaneKind::Oscillator, vec![None, Some(1.0)], "fast")
        .expect("valid");
    assert_eq!(series.len(), 2);
    assert!(!series.is_empty());
    assert!(PaneSeries::new("x", PaneKind::Price, Vec::new(), "bull")
        .expect("valid")
        .is_empty());
    assert!(
        PaneSeries::new("", PaneKind::Price, Vec::new(), "bull").is_err(),
        "empty name refuses"
    );
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
        assert!(
            ribbon_view(&[Some(1.0)], &[Some(1.0)], bad).is_err(),
            "alpha {bad} refuses"
        );
    }
}

fn chart_row(volume: u64) -> ChartRow {
    ChartRow {
        time: 0,
        open: 100.0,
        high: 101.0,
        low: 99.0,
        close: 100.0,
        volume,
    }
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
    for slot in view
        .buy_top
        .iter()
        .flatten()
        .chain(view.sell_bottom.iter().flatten())
    {
        assert!(
            *slot >= 0.0 && *slot <= 300.0 + 1e-9,
            "bars stay within [0, unit]"
        );
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
    assert!(
        delta_candle_view(&rows, &[], &[Some(1.0)], 300.0).is_err(),
        "length mismatch refuses"
    );
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
        .ts(chrono::Utc
            .with_ymd_and_hms(2026, 1, day, 12, 0, 0)
            .single()
            .expect("valid time"))
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
            assert_eq!(
                to_display_float(px_decimal(leg)),
                Some(leg),
                "legs round-trip exactly"
            );
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
    let fast = vec![
        Some(150.0),
        Some(90.0),
        Some(-120.0),
        Some(-80.0),
        Some(140.0),
        Some(60.0),
    ];
    let marks = pivot_marks(&fast, 100.0).expect("valid threshold");
    assert_eq!(
        marks,
        vec![
            PivotMark {
                index: 1,
                side: PivotSide::High
            },
            PivotMark {
                index: 3,
                side: PivotSide::Low
            },
            PivotMark {
                index: 5,
                side: PivotSide::High
            },
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
        assert!(
            pivot_marks(&[Some(1.0)], bad).is_err(),
            "threshold {bad} refuses"
        );
    }
    assert!(pivot_marks(&[], 100.0).expect("empty is valid").is_empty());
}

#[test]
fn unit_squash_is_odd_monotonic_and_bounded() {
    assert_eq!(
        unit_squash(0.0).to_bits(),
        0.0_f64.to_bits(),
        "zero stays centered"
    );
    for x in [0.001, 0.5, 1.0, 2.0, 100.0, 1.0e9] {
        assert_eq!(
            unit_squash(-x).to_bits(),
            (-unit_squash(x)).to_bits(),
            "odd at {x}"
        );
        assert!(
            unit_squash(x) > 0.0 && unit_squash(x) < 1.0,
            "bounded at {x}"
        );
    }
    let mut prev = 0.0;
    for i in 1..=1000 {
        let x = f64::from(i) / 10.0;
        let got = unit_squash(x);
        assert!(got > prev, "strictly monotonic at {x}");
        prev = got;
    }
    assert!((unit_squash(1.0) - 0.5).abs() < 1e-15, "unit input halves");
}

#[test]
fn affine_squash_matches_shift_scale_squash_widen_order() {
    // Synthetic calibration (no domain table): the primitive must equal the
    // restated op order bit-for-bit on finite inputs, and map 0 through the
    // shift honestly rather than centering it.
    let (offset, slope, scale) = (-50.0_f64, 0.0084_f64, 740.0_f64);
    for raw in [-300.0, -100.0, -50.0, -1.0, 0.0, 1.0, 50.0, 100.0, 300.0] {
        let want = ((raw + offset) * slope) / (1.0 + ((raw + offset) * slope).abs()) * scale;
        assert_eq!(
            affine_squash(raw, offset, slope, scale).to_bits(),
            want.to_bits(),
            "op order at {raw}"
        );
    }
    assert_eq!(
        affine_squash(50.0, offset, slope, scale).to_bits(),
        0.0_f64.to_bits(),
        "+offset zeroes the shift"
    );
    for raw in [-1.0e9, 1.0e9] {
        assert!(
            affine_squash(raw, offset, slope, scale).abs() < scale,
            "band holds at {raw}"
        );
    }
}

#[test]
fn band_squash_centers_zero_and_halves_at_divisor() {
    assert_eq!(
        band_squash(0.0, 100.0, 100.0).to_bits(),
        0.0_f64.to_bits(),
        "zero stays centered"
    );
    assert_eq!(
        band_squash(100.0, 100.0, 100.0).to_bits(),
        50.0_f64.to_bits(),
        "divisor halves"
    );
    assert_eq!(
        band_squash(-100.0, 100.0, 100.0).to_bits(),
        (-50.0_f64).to_bits(),
        "rail mirrors"
    );
    for value in [-1.0e9, -1.0, -0.5, 0.5, 1.0, 1.0e9] {
        assert!(
            band_squash(value, 100.0, 100.0).abs() < 100.0,
            "finite {value} stays inside"
        );
    }
    assert!(
        band_squash(f64::NAN, 100.0, 100.0).is_nan(),
        "non-finite yields NaN for the lift"
    );
    assert!(
        band_squash(f64::INFINITY, 100.0, 100.0).is_nan(),
        "infinite yields NaN for the lift"
    );
}

#[test]
fn map_series_optional_preserves_holes_and_filters_non_finite() {
    let series = vec![None, Some(1.0), Some(2.0), None, Some(3.0)];
    let out = map_series_optional(&series, |v| v * 2.0);
    assert_eq!(
        out,
        vec![None, Some(2.0), Some(4.0), None, Some(6.0)],
        "holes survive by position"
    );
    assert!(
        map_series_optional(&[], |v| v).is_empty(),
        "empty in, empty out"
    );
    assert_eq!(
        map_series_optional(&[None], |v| v),
        vec![None],
        "single hole"
    );
    assert_eq!(
        map_series_optional(&[Some(f64::NAN)], |v| v),
        vec![None],
        "NaN becomes a hole, never a substitute"
    );
    assert_eq!(
        map_series_optional(&[Some(1.0)], |_| f64::INFINITY),
        vec![None],
        "non-finite output becomes a hole"
    );
    // Composition is slot-for-slot: lifting the composed scalar equals
    // composing the lifts, so callers may fuse or stage freely.
    let staged = map_series_optional(&map_series_optional(&series, |v| v + 1.0), |v| v * 10.0);
    let fused = map_series_optional(&series, |v| (v + 1.0) * 10.0);
    assert_eq!(staged, fused, "stage or fuse, same slots");
}
