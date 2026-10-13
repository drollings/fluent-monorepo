use super::*;
use crate::view::{delta_candle_view, pivot_marks, ribbon_view};
use protopyne_chart_model::ChartRow;
use protopyne_chart_scale::PriceScaleMode;

fn price_pane() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid price pane")
}

fn zero_pane() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, -300.0, 300.0, 200.0, 8.0, 0.0, false)
        .expect("valid zero pane")
}

fn row(price: f64, volume: u64) -> ChartRow {
    ChartRow { time: 0, open: price, high: price + 1.0, low: price - 1.0, close: price, volume }
}

#[test]
fn candles_line_up_with_rows_and_trend_keys() {
    let pane = price_pane();
    let rows = vec![row(100.0, 10), row(101.0, 20), row(99.0, 30)];
    let trend = vec![true, false, true];
    let geoms = candle_geoms(&rows, &trend, &pane).expect("aligned inputs paint");
    assert_eq!(geoms.len(), rows.len(), "converted rows are conserved, never dropped here");
    for (index, geom) in geoms.iter().enumerate() {
        assert_eq!(geom.index, index);
        assert_eq!(geom.x.to_bits(), pane.index_to_x(index).to_bits());
        assert_eq!(geom.trend_bullish, trend[index]);
    }
    assert!(geoms[0].up, "close == open reads up");
    assert!(candle_geoms(&rows, &[true], &pane).is_err(), "key mismatch refuses");
}

#[test]
fn ribbon_fill_keeps_upper_above_lower_or_absent() {
    let pane = price_pane();
    let ribbon = ribbon_view(
        &[Some(102.0), None, Some(98.0)],
        &[Some(100.0), Some(100.0), Some(99.0)],
        0.5,
    )
    .expect("valid ribbon");
    let bands = ribbon_bands(&ribbon, &pane);
    assert_eq!(bands.len(), 3);
    let first = bands[0].expect("both edges present");
    assert!(first.top_y <= first.bottom_y, "fill runs downward: {first:?}");
    assert_eq!(bands[1], None, "holes stay absent, never fabricated");
    let third = bands[2].expect("present");
    assert!(third.top_y <= third.bottom_y + f32::EPSILON);
}

#[test]
fn delta_bars_hang_from_the_zero_line() {
    let pane = zero_pane();
    let zero_y = pane.price_to_y(0.0).expect("zero spans");
    let rows = vec![
        ChartRow { time: 0, open: 100.0, high: 101.0, low: 99.0, close: 100.0, volume: 1000 },
        ChartRow { time: 1, open: 100.0, high: 101.0, low: 99.0, close: 100.0, volume: 4000 },
    ];
    let delta = delta_candle_view(
        &rows,
        &[Some(600.0), Some(1000.0)],
        &[Some(400.0), Some(3000.0)],
        300.0,
    )
    .expect("valid delta");
    let bars = delta_bar_geoms(&delta, &pane).expect("spanning pane paints");
    assert_eq!(bars.len(), 2);
    for bar in &bars {
        assert_eq!(bar.zero_y.to_bits(), zero_y.to_bits());
        if let Some(buy_y) = bar.buy_y {
            assert!(buy_y <= zero_y, "buys rise from zero: {bar:?}");
        }
        if let Some(sell_y) = bar.sell_y {
            assert!(sell_y >= zero_y, "sells extend downward only: {bar:?}");
        }
    }
    assert!(bars[0].buy_y.expect("present") < zero_y);
    assert!(bars[0].sell_y.expect("present") > zero_y);
}

#[test]
fn delta_refuses_unspanned_panes_and_mismatched_splits() {
    let narrow = Viewport::new(PriceScaleMode::Normal, 10.0, 20.0, 200.0, 8.0, 10.0, false)
        .expect("valid viewport");
    let delta = DeltaView { buy_top: vec![Some(5.0)], sell_bottom: vec![Some(5.0)] };
    assert!(delta_bar_geoms(&delta, &narrow).is_err(), "bars need a zero line");
    let pane = zero_pane();
    let split = DeltaView { buy_top: vec![Some(5.0)], sell_bottom: Vec::new() };
    assert!(delta_bar_geoms(&split, &pane).is_err(), "split mismatch refuses");
}

#[test]
fn oscillator_points_skip_holes_and_match_present_count() {
    let pane = zero_pane();
    let values = vec![None, Some(150.0), Some(f64::NAN), Some(-80.0), None];
    let points = oscillator_points(&values, &pane);
    assert_eq!(points.len(), values.len());
    assert_eq!(points[0], None);
    assert_eq!(points[2], None, "non-finite readings lift the pen");
    assert_eq!(points[4], None);
    let present = points.iter().flatten().count();
    assert_eq!(present, 2, "only finite holed-free points emit");
    let (x, y) = points[1].expect("present");
    assert_eq!(x.to_bits(), pane.index_to_x(1).to_bits());
    assert_eq!(y.to_bits(), pane.price_to_y(150.0).expect("in range").to_bits());
}

#[test]
fn pivot_dots_decorate_crossings_only() {
    let pane = zero_pane();
    let fast = vec![Some(150.0), Some(90.0), Some(-120.0), Some(-80.0)];
    let marks = pivot_marks(&fast, 100.0).expect("valid threshold");
    assert_eq!(marks.len(), 2);
    let dots = pivot_dots(&marks, &fast, &pane);
    assert_eq!(dots.len(), 2, "every crossing dots, nothing else");
    assert_eq!(dots[0].index, 1);
    assert_eq!(dots[0].side, PivotSide::High);
    assert_eq!(dots[1].side, PivotSide::Low);
    for dot in &dots {
        let value = fast[dot.index].expect("crossings carry values");
        assert_eq!(dot.y.to_bits(), pane.price_to_y(value).expect("mapped").to_bits());
        assert_eq!(dot.x.to_bits(), pane.index_to_x(dot.index).to_bits());
    }
    assert!(pivot_dots(&marks, &[None, None, None, None], &pane).is_empty());
}

#[test]
fn threshold_rules_mirror_about_zero() {
    let pane = zero_pane();
    let zero_y = pane.price_to_y(0.0).expect("zero spans");
    let plus = threshold_line(100.0, &pane).expect("rule maps");
    let minus = pane.price_to_y(-100.0).expect("mirror maps");
    assert!((plus + minus - 2.0 * zero_y).abs() < 1e-3, "rules mirror about zero");
    assert!(plus < zero_y, "positive rules sit above zero on screen");
    for bad in [0.0, -50.0, f64::NAN] {
        assert_eq!(threshold_line(bad, &pane), None, "threshold {bad} draws nothing");
    }
}

#[test]
fn theme_colors_resolve_and_fall_back_softly() {
    let theme = ThemeConfig::default();
    assert_eq!(theme_color(&theme, "bull"), egui::Color32::from_rgb(0x20, 0xB4, 0x6C));
    assert_eq!(theme_color(&theme, "bear"), egui::Color32::from_rgb(0xB4, 0x20, 0x20));
    assert_eq!(theme_color(&theme, "no_such_key"), egui::Color32::GRAY);
    let broken = ThemeConfig::new().bull("red".to_owned()).build();
    assert_eq!(theme_color(&broken, "bull"), egui::Color32::GRAY, "paint never panics");
}
