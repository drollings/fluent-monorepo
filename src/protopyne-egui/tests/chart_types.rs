//! Chart-type wiring goldens: the terminal's price picture delegates to the
//! headless transforms with no second math — Heikin-Ashi equals
//! `transform.rs::heikin_ashi` bar for bar, the line series equals the row
//! closes, and area-fill vertex colors run the full pipeline
//! (`area_view` → `area_polygons` → `area_vertex_color`) with endpoints
//! exact and every channel in range.
//!
//! Exact decimal comparisons below are over exact small integral prices,
//! never measured quantities.

use super::*;
use chrono::TimeZone as _;
use fluent_egui::{Viewport, aggregate_column, area_polygons, area_vertex_color, crosshair_data};
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
    // Candles and Heikin-Ashi wire on the bars path (line and area on the
    // rows path): anything else refuses with the parameter named instead
    // of rendering a guess.
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
    // Heikin-Ashi converts over the full history first, then windows: the
    // painted rows are the converted tail, while converting a trailing
    // slice alone re-seeds the recursive open from the slice — windowing
    // first would miscompute. The range is deliberately NOT `All`: with
    // `All` both orders paint every row and the test proves nothing. A day
    // window over 12-hour bars keeps the last two of five, so the window
    // really cuts.
    let bars = five_bars();
    let painted =
        picture_rows(&bars, DateRange::Day1, 43_200, ChartType::Heikin).expect("heikin picture");
    let full = rows_of(&heikin_ashi(&bars)).rows;
    assert_eq!(painted.len(), 2, "the day window cuts five bars to two");
    assert_eq!(painted, full[3..], "convert-then-window paints the converted tail");
    let sliced = rows_of(&heikin_ashi(&bars[3..])).rows;
    assert!(
        (sliced[0].open - full[3].open).abs() > 1e-9,
        "a window-first open re-seeds from the slice, not history"
    );
}

#[test]
fn heikin_aggregates_after_transforming_never_before() {
    // The recursion forbids the reverse: Heikin-Ashi opens recurse bar over
    // bar, so folding columns first and transforming the folds re-seeds the
    // open from the fold instead of history. The column dispatcher therefore
    // only ever consumes rows transformed at native resolution first.
    let bars = five_bars();
    let native = rows_of(&heikin_ashi(&bars)).rows;
    // The forbidden order's input, hand-folded in the test (never a helper):
    // one bar spanning the run, transformed alone.
    let folded = Bar::new()
        .ts(bars[0].ts)
        .o(bars.first().expect("non-empty").o)
        .h(bars.iter().map(|bar| bar.h).max().expect("non-empty"))
        .l(bars.iter().map(|bar| bar.l).min().expect("non-empty"))
        .c(bars.last().expect("non-empty").c)
        .volume(bars.iter().map(|bar| bar.volume).sum())
        .maybe_adj(None)
        .build();
    let reversed = rows_of(&heikin_ashi(std::slice::from_ref(&folded))).rows;
    let native_fold = aggregate_column(&native).expect("native rows fold");
    assert_eq!(reversed.len(), 1, "one folded bar transforms to one row");
    assert!(
        (native_fold.open - reversed[0].open).abs() > 1e-9,
        "aggregate-first re-seeds the open ({} vs {})",
        native_fold.open,
        reversed[0].open
    );
    // And the frame path feeds the dispatcher the native order: the painted
    // Heikin picture is the transformed tail, whose fold is the native fold.
    let painted =
        picture_rows(&bars, DateRange::All, 60, ChartType::Heikin).expect("heikin picture");
    assert_eq!(painted, native, "the dispatcher input is transform-first");
    assert_eq!(aggregate_column(&painted), Some(native_fold), "its fold is the native fold");
}

#[test]
fn unwired_picture_refuses() {    // The row dispatch refuses unwired types with the parameter named, like
    // the bar dispatch.
    let refused =
        picture_rows(&five_bars(), DateRange::All, 60, ChartType::Renko).expect_err("renko refuses");
    assert!(refused.to_string().contains("chart_type"), "reason names the parameter");
}

/// Outcome of one dispatcher call, reduced to the two facts a table needs:
/// did it produce a picture, and did it name the parameter when it refused.
#[derive(Debug, PartialEq, Eq)]
enum Dispatch {
    Painted(usize),
    Refused(String),
}

fn bar_dispatch(kind: ChartType) -> Dispatch {
    match chart_price_bars(&five_bars(), kind) {
        Ok(bars) => Dispatch::Painted(bars.len()),
        Err(e) => Dispatch::Refused(e.to_string()),
    }
}

fn row_dispatch(kind: ChartType) -> Dispatch {
    match picture_rows(&five_bars(), DateRange::All, 60, kind) {
        Ok(rows) => Dispatch::Painted(rows.len()),
        Err(e) => Dispatch::Refused(e.to_string()),
    }
}

#[test]
fn every_chart_type_reaches_both_dispatchers() {
    // Coverage: every taxonomy variant is handed to both dispatchers, so no
    // variant can grow a third answer by never being called. What the two
    // answers *are* is the separate characterization below.
    let mut painted_by_both = Vec::new();
    let mut refused_by_both = Vec::new();
    let mut split = Vec::new();
    for kind in ChartType::all() {
        let bars = bar_dispatch(*kind);
        let rows = row_dispatch(*kind);
        // A refusal from either side always names the parameter it refused.
        for dispatch in [&bars, &rows] {
            if let Dispatch::Refused(reason) = dispatch {
                assert!(
                    reason.contains("chart_type"),
                    "{kind:?} must name the parameter in its refusal: {reason}"
                );
            }
        }
        match (bars, rows) {
            (Dispatch::Painted(n), Dispatch::Painted(m)) => {
                assert_eq!(n, m, "{kind:?} paints {n} bars but {m} rows");
                painted_by_both.push(*kind);
            }
            (Dispatch::Refused(_), Dispatch::Refused(_)) => refused_by_both.push(*kind),
            (Dispatch::Painted(_), Dispatch::Refused(_))
            | (Dispatch::Refused(_), Dispatch::Painted(_)) => split.push(*kind),
        }
    }
    let classified = painted_by_both.len() + refused_by_both.len() + split.len();
    assert_eq!(classified, ChartType::all().len(), "every variant is classified exactly once");
    // The outcome split is the rows-backed pair, exactly: splitting by
    // outcome is expected where the classification splits (the bars path
    // refuses what the rows path paints), and the agreement table below
    // pins the classification behind it.
    assert_eq!(
        split,
        vec![ChartType::Line, ChartType::Area],
        "only the rows-backed kinds split by outcome: painted={painted_by_both:?} / refused={refused_by_both:?} / split={split:?}"
    );
    assert!(
        painted_by_both.contains(&ChartType::Candles)
            && painted_by_both.contains(&ChartType::Heikin)
            && refused_by_both.contains(&ChartType::Renko),
        "candles and heikin paint, renko refuses in both columns: {painted_by_both:?} / {refused_by_both:?} / split={split:?}"
    );
}

#[test]
fn picture_rows_and_chart_price_bars_agree_for_every_chart_type() {
    // One terminal picture decision: the `picture_source` classification
    // predicts both dispatchers' outcomes, so the three answers (decision,
    // bars path, rows path) cannot contradict.
    for kind in ChartType::all() {
        let bars = bar_dispatch(*kind);
        let rows = row_dispatch(*kind);
        match picture_source(*kind) {
            Ok(PictureSource::Bars) => {
                let (Dispatch::Painted(bar_count), Dispatch::Painted(row_count)) = (&bars, &rows)
                else {
                    panic!("{kind:?} is bar-backed but misses a path: bars={bars:?}, rows={rows:?}");
                };
                assert_eq!(
                    bar_count, row_count,
                    "{kind:?} paints both paths over the same bars"
                );
            }
            Ok(PictureSource::Rows) => {
                let Dispatch::Refused(reason) = &bars else {
                    panic!("{kind:?} is rows-backed but the bars path paints: bars={bars:?}");
                };
                assert!(
                    reason.contains("chart_type") && reason.contains("rows"),
                    "{kind:?} bars refusal names the parameter and the rows reason: {reason}"
                );
                assert!(
                    matches!(rows, Dispatch::Painted(_)),
                    "{kind:?} is rows-backed so the rows path paints: {rows:?}"
                );
            }
            Err(decision) => {
                let (Dispatch::Refused(left), Dispatch::Refused(right)) = (&bars, &rows) else {
                    panic!("{kind:?} is unwired but paints a path: bars={bars:?}, rows={rows:?}");
                };
                // Both paths refuse with the one shared refusal — the same
                // construction, not two wordings kept in sync by hand.
                assert_eq!(
                    left,
                    &decision.to_string(),
                    "{kind:?} bars refusal is the shared decision"
                );
                assert_eq!(
                    right,
                    &decision.to_string(),
                    "{kind:?} rows refusal is the shared decision"
                );
            }
        }
    }
}

/// Bar selection at an exact half-offset rounds up, deterministically: a
/// pointer exactly between two bars reads the higher index, every time.
/// This records the shared mapping's rounding as a decision — a future
/// rounding drift fails here rather than moving one pixel class silently.
#[test]
fn crosshair_tie_at_half_offset_rounds_up() {
    // Eight-pixel bars: bar 3 centers on x = 24, bar 4 on x = 32, so the
    // tie sits exactly at x = 28 (slot 3.5). The price leg stays mid-pane
    // so only the slot decides.
    let pane = Viewport::new(
        protopyne_chart_scale::PriceScaleMode::Normal,
        90.0,
        110.0,
        400.0,
        8.0,
        90.0,
        false,
    )
    .expect("test viewport builds");
    let y = pane.price_to_y(100.0).expect("mid-pane price maps");
    let cases: [(f32, usize); 5] = [
        (27.0, 3),  // clearly inside bar 3's half
        (27.9, 3),  // a tenth below the tie still reads low
        (28.0, 4),  // the tie itself reads up
        (28.1, 4),  // a tenth above the tie reads high
        (29.0, 4),  // clearly inside bar 4's half
    ];
    for (x, expected) in cases {
        let data = crosshair_data((x, y), &pane, 10).expect("in-pane pointer reads");
        assert_eq!(data.index, expected, "pointer at x = {x} names bar {expected}");
    }
    // And the tie decides the same way twice — determinism, not luck.
    let first = crosshair_data((28.0, y), &pane, 10).expect("tie reads");
    let second = crosshair_data((28.0, y), &pane, 10).expect("tie reads again");
    assert_eq!(first.index, second.index, "the tie is stable across reads");
}

#[test]
fn area_baseline_is_the_window_floor_not_the_data_floor() {
    // The fill foot sits at the window floor: rows bottom at 99.0, but a
    // 2x-zoomed window opens at 99.48, and the view the terminal fills
    // carries the window floor — provably not the data floor. A baseline
    // from the data extent would fade the zoomed area to a price off the
    // pane.
    let rows = vec![row(100.0), row(101.5), row(99.25)];
    let data_floor = rows.iter().map(|row| row.low).fold(f64::INFINITY, f64::min);
    let window = Viewport::new(
        protopyne_chart_scale::PriceScaleMode::Normal,
        99.48,
        100.52,
        420.0,
        8.0,
        99.0,
        false,
    )
    .expect("zoomed window builds");
    let floor = window.visible_range().min;
    assert!(floor > data_floor, "the zoomed floor sits above the data floor: {floor} vs {data_floor}");
    let view = area_view(line_values(&rows), floor, 0.5).expect("window-floored area builds");
    assert_eq!(
        view.baseline.to_bits(),
        floor.to_bits(),
        "the view carries the window floor, never the data floor"
    );
    // Every closed run foots at the mapped floor: each polygon's second
    // half is its lower edge, so every foot pixel is the baseline's pixel
    // whatever the data extent holds (out-of-window values extrapolate,
    // never clamp to the foot). The floor is the window minimum, so it maps
    // to the pane bottom: y = height, as a literal on the documented
    // mapping, never a second mapping call.
    let foot_y = 420.0;
    let polygons = area_polygons(&view.values, view.baseline, &window);
    assert!(!polygons.is_empty(), "present values close at least one run");
    for polygon in &polygons {
        let foot = &polygon.points[polygon.indices.len()..];
        assert_eq!(foot.len(), polygon.indices.len(), "each run closes its own foot");
        for (x, y) in foot {
            assert!(
                (y - foot_y).abs() < 1e-3,
                "the fill foot sits at the window floor at x = {x}: {y} vs {foot_y}"
            );
        }
    }
}

#[test]
fn area_view_moves_the_caller_series_without_copying() {
    // The caller's buffer becomes the view's buffer: pointer equality
    // proves the per-frame handoff allocates nothing.
    let values = line_values(&[row(100.0), row(101.0)]);
    let before = values.as_ptr();
    let view = area_view(values, 99.0, 0.5).expect("area view builds");
    assert_eq!(view.values.as_ptr(), before, "the series moves, never copies");
}

#[test]
fn area_view_refuses_beyond_alpha() {
    // Baseline, values, and emptiness refuse with their own parameter
    // named; holes stay legal and preserved.
    let values = vec![Some(100.0), Some(101.0)];
    for baseline in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let refused =
            area_view(values.clone(), baseline, 0.5).expect_err("non-finite baseline refuses");
        assert!(
            refused.to_string().contains("baseline"),
            "baseline refusal names baseline: {refused}"
        );
    }
    for bad in [vec![Some(100.0), Some(f64::NAN)], vec![None, Some(f64::INFINITY)]] {
        let refused = area_view(bad, 99.0, 0.5).expect_err("non-finite leg refuses");
        assert!(
            refused.to_string().contains("values"),
            "leg refusal names values: {refused}"
        );
    }
    let holed =
        area_view(vec![Some(100.0), None, Some(101.0)], 99.0, 0.5).expect("holes build");
    assert_eq!(holed.values, vec![Some(100.0), None, Some(101.0)], "holes are preserved");
    let refused = area_view(Vec::new(), 99.0, 0.5).expect_err("empty series refuses");
    assert!(refused.to_string().contains("values"), "empty refusal names values: {refused}");
    let refused = area_view(values, 99.0, 2.0).expect_err("alpha discipline unchanged");
    assert!(refused.to_string().contains("alpha"), "alpha refusal names alpha: {refused}");
}

#[test]
fn line_wiring_equals_closes() {    // The line series is the row closes, holes preserved.
    let rows = vec![row(100.0), row(101.5), row(99.25)];
    assert_eq!(line_values(&rows), vec![Some(100.0), Some(101.5), Some(99.25)]);
}

#[test]
fn area_fill_vertex_colors_run_golden() {
    // The full area pipeline: values over a baseline become one polygon run,
    // and its vertex colors gradient top-to-bottom with endpoints exact and
    // every channel inside `[0, 255]`.
    let values = vec![Some(100.0), Some(102.0), Some(101.0), Some(103.0)];
    let view = area_view(values, 99.0, 0.5).expect("area view builds");
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
    let polygons = area_polygons(&view.values, view.baseline, &pane);
    assert_eq!(polygons.len(), 1, "one unbroken run over present values");
    let top = egui::Color32::from_rgba_unmultiplied(31, 119, 180, 128);
    let bottom = egui::Color32::from_rgba_unmultiplied(31, 119, 180, 0);
    let points = &polygons[0].points;
    assert!(points.len() >= 4, "area closes around the run");
    let (top_y, bottom_y) = (0.0_f32, 200.0_f32);
    let peak = area_vertex_color(top, bottom, top_y, top_y, bottom_y).expect("finite legs blend");
    let foot =
        area_vertex_color(top, bottom, bottom_y, top_y, bottom_y).expect("finite legs blend");
    assert_eq!(peak, top, "gradient starts exactly on the top color");
    assert_eq!(foot, bottom, "gradient lands exactly on the bottom color");
    for (x, y) in points {
        // Channels are `u8` by construction (the range guarantee); against
        // a transparent foot the pins are a monotone fade: premultiplied
        // channels scale down with alpha, never up, never past the foot.
        let color =
            area_vertex_color(top, bottom, *y, top_y, bottom_y).expect("pane pixels blend");
        assert!(
            color.r() <= top.r() && color.g() <= top.g() && color.b() <= top.b(),
            "channels fade downward at ({x}, {y})"
        );
        assert!(
            bottom.a() <= color.a() && color.a() <= top.a(),
            "alpha stays between foot and top at ({x}, {y})"
        );
    }
    let mid = area_vertex_color(top, bottom, 100.0, top_y, bottom_y).expect("midpoint blends");
    assert_eq!(mid.a(), 64, "midpoint alpha lerps exactly");
}
