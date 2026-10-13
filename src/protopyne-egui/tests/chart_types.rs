//! Chart-type wiring goldens: the terminal's price picture delegates to the
//! headless transforms with no second math — Heikin-Ashi equals
//! `transform.rs::heikin_ashi` bar for bar, the line series equals the row
//! closes, and area-fill vertex colors run the full pipeline
//! (`area_view` → `ribbon_polygons` → `area_vertex_color`) with endpoints
//! exact and every channel in range.
//!
//! Exact decimal comparisons below are over exact small integral prices,
//! never measured quantities.

use super::*;
use chrono::TimeZone as _;
use fluent_egui::{area_vertex_color, ribbon_polygons};
use protopyne_chart_model::{ChartRow, ChartType, DateRange, heikin_ashi, rows_of};
use protopyne_types::Bar;

fn bar_at(index: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::Utc
                .timestamp_opt(1_700_000_000 + index * 60, 0)
                .single()
                .expect("valid test timestamp"),
        )
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(format!("{}.00", 100 + index).parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn five_bars() -> Vec<Bar> {
    (0..5).map(bar_at).collect()
}

fn row(price: f64) -> ChartRow {
    ChartRow { time: 0, open: price, high: price + 1.0, low: price - 1.0, close: price, volume: 10 }
}

#[test]
fn heikin_ashi_wiring_equals_the_transform() {
    // The wired Heikin-Ashi picture IS the headless transform: bar-for-bar
    // equality, so the terminal can never drift from it.
    let bars = five_bars();
    let wired = chart_price_bars(&bars, ChartType::Heikin).expect("heikin-ashi wires");
    assert_eq!(wired, heikin_ashi(&bars));
}

#[test]
fn candles_wiring_preserves_bars() {
    // Classic candles pass bars through untouched.
    let bars = five_bars();
    let wired = chart_price_bars(&bars, ChartType::Candles).expect("candles wire");
    assert_eq!(wired, bars);
}

#[test]
fn unwired_chart_type_refuses() {
    // Only candles, Heikin-Ashi, and line wire end-to-end: anything else
    // refuses with the parameter named instead of rendering a guess.
    let refused =
        chart_price_bars(&five_bars(), ChartType::Renko).expect_err("renko stays unwired");
    assert!(refused.to_string().contains("chart_type"), "reason names the parameter");
}

#[test]
fn picture_rows_convert_then_window() {
    // Candles share the converted window rows, with no drops on clean bars.
    let bars = five_bars();
    let painted =
        picture_rows(&bars, DateRange::All, 60, ChartType::Candles).expect("candles picture");
    assert_eq!(painted, rows_of(&bars).rows);
}

#[test]
fn heikin_picture_converts_before_windowing() {
    // Heikin-Ashi converts over the full history first: the painted rows
    // equal the transform output, while converting a trailing slice alone
    // seeds a different recursive open — windowing first would miscompute.
    let bars = five_bars();
    let painted =
        picture_rows(&bars, DateRange::All, 60, ChartType::Heikin).expect("heikin picture");
    let full = rows_of(&heikin_ashi(&bars)).rows;
    assert_eq!(painted, full);
    let sliced = rows_of(&heikin_ashi(&bars[3..])).rows;
    assert!(
        (sliced[0].open - full[3].open).abs() > 1e-9,
        "a window-first open seeds from the slice, not history"
    );
}

#[test]
fn unwired_picture_refuses() {
    // The row dispatch refuses unwired types with the parameter named, like
    // the bar dispatch.
    let refused =
        picture_rows(&five_bars(), DateRange::All, 60, ChartType::Renko).expect_err("renko refuses");
    assert!(refused.to_string().contains("chart_type"), "reason names the parameter");
}

#[test]
fn line_wiring_equals_closes() {
    // The line series is the row closes, holes preserved.
    let rows = vec![row(100.0), row(101.5), row(99.25)];
    assert_eq!(line_values(&rows), vec![Some(100.0), Some(101.5), Some(99.25)]);
}

#[test]
fn area_fill_vertex_colors_run_golden() {
    // The full area pipeline: values over a baseline become one polygon run,
    // and its vertex colors gradient top-to-bottom with endpoints exact and
    // every channel inside `[0, 255]`.
    let values = vec![Some(100.0), Some(102.0), Some(101.0), Some(103.0)];
    let view = area_view(&values, 99.0, 0.5).expect("area view builds");
    let pane = fluent_egui::Viewport::new(
        protopyne_chart_scale::PriceScaleMode::Normal,
        98.0,
        104.0,
        200.0,
        8.0,
        98.0,
        false,
    )
    .expect("test viewport builds");
    let polygons = ribbon_polygons(&view, &pane);
    assert_eq!(polygons.len(), 1, "one unbroken run over present values");
    let top = egui::Color32::from_rgba_unmultiplied(31, 119, 180, 128);
    let bottom = egui::Color32::from_rgba_unmultiplied(31, 119, 180, 0);
    let points = &polygons[0].points;
    assert!(points.len() >= 4, "area closes around the run");
    let (top_y, bottom_y) = (0.0_f32, 200.0_f32);
    let peak = area_vertex_color(top, bottom, top_y, top_y, bottom_y);
    let foot = area_vertex_color(top, bottom, bottom_y, top_y, bottom_y);
    assert_eq!(peak, top, "gradient starts exactly on the top color");
    assert_eq!(foot, bottom, "gradient lands exactly on the bottom color");
    for (x, y) in points {
        // Channels are `u8` by construction (the range guarantee); against
        // a transparent foot the pins are a monotone fade: premultiplied
        // channels scale down with alpha, never up, never past the foot.
        let color = area_vertex_color(top, bottom, *y, top_y, bottom_y);
        assert!(
            color.r() <= top.r() && color.g() <= top.g() && color.b() <= top.b(),
            "channels fade downward at ({x}, {y})"
        );
        assert!(
            bottom.a() <= color.a() && color.a() <= top.a(),
            "alpha stays between foot and top at ({x}, {y})"
        );
    }
    let mid = area_vertex_color(top, bottom, 100.0, top_y, bottom_y);
    assert_eq!(mid.a(), 64, "midpoint alpha lerps exactly");
}
