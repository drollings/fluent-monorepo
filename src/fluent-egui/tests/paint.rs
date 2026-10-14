use super::*;
use crate::view::{delta_candle_view, pivot_marks, ribbon_view};
use protopyne_chart_model::{AnnotationStyle, ChartRow};
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

/// Inverse-map a painted pixel back to its price: the builder must have
/// mapped `price` through the shared viewport, never a second math. The
/// bound is the viewport round-trip's own pixel quantum (range over height
/// with float headroom), so this discriminates neighboring test prices but
/// never the mapping's own rounding.
fn assert_mapped_price(pane: &Viewport, y: f32, price: f64, quantum: f64) {
    let back = pane.y_to_price(y).expect("painted pixels sit in-pane");
    assert!(
        (back - price).abs() <= quantum,
        "pixel {y} decodes to {back}, not the mapped price {price}"
    );
}

/// Pixel quantum for the zero-spanning test pane (-300..300 over 200 px),
/// in the same range-over-height form as the viewport round-trip.
fn zero_quantum() -> f64 {
    (300.0 - -300.0) / 200.0 * 8.0
}

#[test]
fn candles_line_up_with_rows_and_trend_keys() {
    let pane = price_pane();
    let rows = vec![row(100.0, 10), row(101.0, 20), row(99.0, 30)];
    let trend = vec![true, false, true];
    let geoms = candle_geoms(&rows, &trend, &pane).expect("aligned inputs paint");
    assert_eq!(geoms.len(), rows.len(), "converted rows are conserved, never dropped here");
    // One pixel quantum in price units, as in the viewport round-trip.
    let quantum = (110.0 - 90.0) / 400.0 * 8.0;
    for (index, geom) in geoms.iter().enumerate() {
        assert_eq!(geom.index, index);
        // Bar slots start at bar zero on the 8 px spacing: a literal pin on
        // the documented contract, not a second call to the mapping.
        assert_eq!(geom.x.to_bits(), (index as f32 * 8.0).to_bits());
        assert_eq!(geom.trend_bullish, trend[index]);
        // The wick spans high to low on screen, and each leg decodes back
        // to the row field it was mapped from.
        assert!(geom.high_y <= geom.low_y, "wicks run downward");
        assert_mapped_price(&pane, geom.open_y, rows[index].open, quantum);
        assert_mapped_price(&pane, geom.high_y, rows[index].high, quantum);
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
    // One shared zero line across bars, never a per-bar guess: both bars
    // hang from the same pixel.
    assert_eq!(bars[0].zero_y.to_bits(), bars[1].zero_y.to_bits());
    for bar in &bars {
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
    // Slot 1 sits on the 8 px spacing: the documented bar-zero contract as
    // a literal, not a second mapping call.
    assert_eq!(x.to_bits(), 8.0f32.to_bits());
    // The painted pixel decodes back to the value it was mapped from, and
    // the higher reading paints above the lower one.
    assert_mapped_price(&pane, y, 150.0, zero_quantum());
    let (_, low_y) = points[3].expect("present");
    assert!(y < low_y, "higher readings paint above lower ones");
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
        // Each dot decodes back to the oscillator value at its own crossing
        // — the builder mapped the crossing, never a neighboring value —
        // and sits on its bar's slot.
        assert_mapped_price(&pane, dot.y, fast[dot.index].expect("crossings carry values"), zero_quantum());
        let slot = pane.x_to_index(dot.x).expect("painted slots invert");
        assert!((slot - dot.index as f64).abs() <= 1e-9, "dot {} sits on bar {}", dot.index, dot.index);
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
fn theme_colors_resolve_to_their_slots() {
    // The silent gray fallback is gone (declared behavior change): known keys
    // resolve to their parsed slots, and the refusal pins live in
    // `resolve_color_errors_visibly` below.
    let theme = ThemeConfig::default();
    assert_eq!(
        resolve_color(&theme, "bull").expect("known key resolves"),
        egui::Color32::from_rgb(0x20, 0xB4, 0x6C)
    );
    assert_eq!(
        resolve_color(&theme, "bear").expect("known key resolves"),
        egui::Color32::from_rgb(0xB4, 0x20, 0x20)
    );
}

#[test]
fn ribbon_polygons_fill_runs_not_bars() {
    let pane = price_pane();
    let ribbon = ribbon_view(
        &[Some(102.0), Some(103.0), Some(98.0), None, Some(101.0)],
        &[Some(100.0), Some(101.0), Some(100.0), Some(100.0), Some(100.0)],
        0.5,
    )
    .expect("valid ribbon");
    let polygons = ribbon_polygons(&ribbon, &pane);
    assert_eq!(polygons.len(), 3, "bull run, bear run, hole, lone bar");
    assert_eq!(polygons[0].indices, vec![0, 1]);
    assert!(polygons[0].bullish);
    assert_eq!(polygons[0].points.len(), 4, "upper forward plus lower reversed");
    assert_eq!(polygons[1].indices, vec![2]);
    assert!(!polygons[1].bullish, "trend flip starts a new run");
    assert_eq!(polygons[2].indices, vec![4], "holes break runs, never bridge");
    for polygon in &polygons {
        assert!(polygon.points.len() >= 4 || polygon.indices.len() == 1);
    }
    let empty = ribbon_view(&[None, None], &[None, None], 0.5).expect("holed ribbon");
    assert!(ribbon_polygons(&empty, &pane).is_empty(), "fully holed input fills nothing");
}

#[test]
fn area_polygons_match_ribbon_polygons_on_constant_legs() {
    // One run-closing core: an area fill over a constant baseline closes
    // the same runs and points as a ribbon with the same legs — no second
    // polygon code to drift. The trend flag is the documented difference
    // (the ribbon keys each run, the area carries none), so the pin
    // compares runs and points, never flags.
    let pane = price_pane();
    let upper = vec![Some(102.0), Some(103.0), Some(104.0), None, Some(101.0)];
    let lower = vec![Some(100.0); upper.len()];
    let ribbon = ribbon_view(&upper, &lower, 0.5).expect("valid ribbon");
    assert!(
        ribbon.bullish.iter().zip(upper.iter()).all(|(trend, leg)| leg.is_none() || *trend),
        "one-sided legs keep one trend key, so runs can only break on holes"
    );
    let area = area_polygons(&upper, 100.0, &pane);
    let ribbons = ribbon_polygons(&ribbon, &pane);
    assert_eq!(area.len(), ribbons.len(), "holes break the same runs");
    for (closed, keyed) in area.iter().zip(ribbons.iter()) {
        assert_eq!(closed.indices, keyed.indices, "same run extents");
        assert_eq!(closed.points, keyed.points, "same closed points");
        assert!(!closed.bullish, "area runs carry no trend key");
    }
    assert!(
        area_polygons(&[None, None], 100.0, &pane).is_empty(),
        "fully holed area fills nothing"
    );
}

#[test]
fn pivot_price_dots_sit_on_bar_extremes() {
    let pane = price_pane();
    let rows = vec![row(100.0, 10), row(101.0, 20), row(99.0, 30)];
    let fast = vec![Some(150.0), Some(90.0), Some(-80.0)];
    let marks = pivot_marks(&fast, 100.0).expect("valid threshold");
    assert_eq!(marks.len(), 1);
    let dots = pivot_price_dots(&marks, &rows, &pane);
    assert_eq!(dots.len(), 1);
    assert_eq!(dots[0].index, 1);
    // High-side dots sit on the bar high (102), not the oscillator value
    // (90): the painted pixel decodes back to the extreme.
    assert_mapped_price(&pane, dots[0].y, rows[1].high, (110.0 - 90.0) / 400.0 * 8.0);
    assert!(pivot_price_dots(&marks, &rows[..1], &pane).is_empty(), "past-the-rows marks skip");
}

#[test]
fn off_window_zero_line_is_refused() {
    // A pane that does not span zero has no zero line to hang delta bars
    // from: the oscillator/delta zero path refuses rather than extrapolating.
    let positive = Viewport::new(PriceScaleMode::Normal, 10.0, 20.0, 200.0, 8.0, 10.0, false)
        .expect("valid viewport");
    let delta = DeltaView { buy_top: vec![Some(5.0)], sell_bottom: vec![Some(5.0)] };
    assert!(delta_bar_geoms(&delta, &positive).is_err(), "zero off-window refuses");
    // The inverse direction agrees: an off-pane pixel has no price.
    assert_eq!(positive.y_to_price(-1.0), None, "off-pane pixel refuses");
    assert_eq!(positive.y_to_price(201.0), None, "off-pane pixel refuses");
}

#[test]
fn threshold_rules_refuse_off_window_values() {
    let pane = price_pane(); // window 90..110
    assert!(threshold_line(100.0, &pane).is_some(), "in-window rule maps");
    assert_eq!(threshold_line(500.0, &pane), None, "above-window rule draws nothing");
    assert_eq!(threshold_line(1.0, &pane), None, "below-window rule draws nothing");
}

#[test]
fn pivot_price_dots_skip_off_window_extremes() {
    let pane = price_pane(); // window 90..110
    let marks =
        vec![PivotMark { index: 0, side: PivotSide::High }, PivotMark { index: 1, side: PivotSide::High }];
    // Bar 0's high (120) runs off the window; bar 1's high (101) is in-window.
    let rows = vec![ChartRow { high: 120.0, ..row(100.0, 10) }, ChartRow { high: 101.0, ..row(100.0, 10) }];
    let dots = pivot_price_dots(&marks, &rows, &pane);
    assert_eq!(dots.len(), 1, "only the in-window extreme decorates");
    assert_eq!(dots[0].index, 1);
}

#[test]
fn pivots_lower_to_markers_without_moving() {
    // The retired pivot emitter's shape lives on in the marker emitter:
    // lowering carries geometry bit-for-bit and maps the side through the
    // one shared polarity (highs bearish, lows bullish).
    for (side, style) in [(PivotSide::High, AnnotationStyle::Bear), (PivotSide::Low, AnnotationStyle::Bull)] {
        let marker = MarkerDot::from_pivot(&PivotDot { index: 3, x: 32.0, y: 200.0, side });
        assert_eq!(marker.index, 3, "the bar does not move");
        assert_eq!(marker.x.to_bits(), 32.0f32.to_bits(), "x does not move");
        assert_eq!(marker.y.to_bits(), 200.0f32.to_bits(), "y does not move");
        assert_eq!(marker.style, style, "{side:?} lowers to {style:?}");
    }
}

#[test]
fn transparency_maps_percent_to_byte_opacity() {
    assert_eq!(opacity_from_transparency(0.0), 255, "zero transparency is opaque");
    assert_eq!(opacity_from_transparency(100.0), 0, "full transparency is invisible");
    assert_eq!(
        opacity_from_transparency(60.0),
        102,
        "the specified dimming lands at forty percent opacity"
    );
    assert_eq!(opacity_from_transparency(140.0), 0, "overshoot clamps to invisible");
    assert_eq!(opacity_from_transparency(-20.0), 255, "undershoot clamps to opaque");
    assert_eq!(
        opacity_from_transparency(f64::NAN),
        255,
        "non-finite transparency stays visible, never silently vanishes"
    );
}

#[test]
fn crosshair_spans_snap_to_the_readout_pixel() {
    use crate::hittest::Crosshair;
    let pane = price_pane();
    let readout = Crosshair { index: 3, price: 100.0 };
    let lines = crosshair_lines(&readout, &pane, 320.0, 400.0).expect("readout maps");
    // The snapped readout pixel, hand-derived from the documented mapping
    // (bar slots start at bar zero on the 8 px spacing; y runs
    // range-top-to-bottom over the 90..110 window on 400 px): slot 3 sits at
    // x = 24, and 100.0 sits halfway down at y = 200.
    assert_eq!(lines.x.to_bits(), 24.0f32.to_bits(), "span x is the snapped bar pixel");
    assert_eq!(lines.y.to_bits(), 200.0f32.to_bits(), "span y is the snapped price pixel");
    assert_eq!((lines.left, lines.right, lines.top, lines.bottom), (0.0, 320.0, 0.0, 400.0));
    assert!(
        crosshair_lines(&Crosshair { index: 0, price: f64::NAN }, &pane, 320.0, 400.0).is_none(),
        "unmappable price yields no spans"
    );
    assert!(
        crosshair_lines(&readout, &pane, 0.0, 400.0).is_none(),
        "degenerate extents yield no spans"
    );
}

#[test]
fn resolve_color_errors_visibly() {    // Known keys resolve to their parsed `#RRGGBB` slot; nothing here falls
    // back to a silent default.
    let theme = ThemeConfig::new().bull("#112233".to_owned()).build();
    let color = resolve_color(&theme, "bull").expect("known key resolves");
    assert_eq!((color.r(), color.g(), color.b(), color.a()), (0x11, 0x22, 0x33, 255));
    let missing = resolve_color(&theme, "nope").expect_err("missing key refuses");
    assert!(
        missing.to_string().contains("nope"),
        "missing-key reason names the key: {missing}"
    );
    let bad = ThemeConfig::new().bull("red".to_owned()).build();
    let malformed = resolve_color(&bad, "bull").expect_err("malformed slot refuses");
    assert!(
        malformed.to_string().contains("bull"),
        "malformed-slot reason names the key: {malformed}"
    );
}

/// Headless frame: a real `Painter` over a test `Context`, so emitter tests
/// exercise the drawing path instead of only the geometry builders.
fn test_painter() -> (egui::Context, egui::Painter) {
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput::default());
    let painter =
        egui::Painter::new(ctx.clone(), egui::LayerId::background(), egui::Rect::EVERYTHING);
    (ctx, painter)
}

/// Drain one frame: the font atlas leaves a texture delta that aborts on
/// drop unless handled, so clear it — the tests read shapes, not textures.
fn end_frame(ctx: &egui::Context) -> egui::FullOutput {
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    output
}

/// Tessellated vertex colors of one frame: the pixel truth for polarity and
/// presence assertions (which color painted, whether anything painted).
fn frame_colors(ctx: &egui::Context, output: egui::FullOutput) -> Vec<egui::Color32> {
    ctx.tessellate(output.shapes, 1.0)
        .into_iter()
        .flat_map(|primitive| match primitive.primitive {
            egui::epaint::Primitive::Mesh(mesh) => {
                mesh.vertices.into_iter().map(|vertex| vertex.color).collect()
            }
            egui::epaint::Primitive::Callback(_) => Vec::new(),
        })
        .collect()
}

fn candle_geom(index: usize) -> CandleGeom {
    CandleGeom {
        index,
        x: index as f32 * 8.0,
        open_y: 200.0,
        high_y: 180.0,
        low_y: 220.0,
        close_y: 210.0,
        up: true,
        trend_bullish: true,
    }
}

fn ribbon_run() -> RibbonPolygon {
    RibbonPolygon {
        indices: vec![0, 1],
        points: vec![(0.0, 10.0), (8.0, 12.0), (8.0, 40.0), (0.0, 42.0)],
        bullish: true,
    }
}

/// Sweep property: `NaN`, `±inf`, and off-window legs refuse or skip across
/// every mapping emitter — never a substituted value.
#[test]
fn non_finite_inputs_refuse_or_skip_across_emitters() {
    let pane = price_pane(); // window 90..110
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        // Geometry builders skip the bad leg, keeping every other output.
        let rows = vec![row(100.0, 10), ChartRow { high: bad, ..row(100.0, 10) }];
        let geoms = candle_geoms(&rows, &[true, true], &pane).expect("lengths align");
        assert_eq!(geoms.len(), 1, "the non-finite bar skips, its neighbor paints ({bad})");
        let ribbon = ribbon_view(&[Some(102.0), Some(bad)], &[Some(100.0), Some(100.0)], 0.5)
            .expect("ribbon builds");
        let bands = ribbon_bands(&ribbon, &pane);
        assert_eq!(bands[1], None, "non-finite edge stays absent ({bad})");
        let points = oscillator_points(&[Some(100.0), Some(bad)], &pane);
        assert!(points[0].is_some(), "the finite leg still maps ({bad})");
        assert_eq!(points[1], None, "non-finite readings lift the pen ({bad})");
        // The window predicate refuses alongside the finite checks.
        assert_eq!(pane.windowed_y(bad), None, "windowed mapping refuses {bad}");
        assert_eq!(threshold_line(bad, &pane), None, "threshold refuses {bad}");
        // Readout pixels refuse with the price.
        assert_eq!(
            crate::hittest::crosshair_pixel(
                &crate::hittest::Crosshair { index: 0, price: bad },
                &pane
            ),
            None,
            "readout pixel refuses {bad}"
        );
        assert!(
            crosshair_lines(&crate::hittest::Crosshair { index: 0, price: bad }, &pane, 320.0, 400.0)
                .is_none(),
            "crosshair spans refuse {bad}"
        );
    }
    // Off-window legs skip exactly like non-finite ones: the shared
    // predicate, not a second decision.
    assert_eq!(pane.windowed_y(500.0), None, "above-window refuses");
    assert_eq!(pane.windowed_y(10.0), None, "below-window refuses");
    assert_eq!(
        oscillator_points(&[Some(500.0)], &pane),
        vec![None],
        "off-window series lift the pen"
    );
    assert_eq!(threshold_line(500.0, &pane), None, "off-window rule draws nothing");
    assert!(
        crosshair_lines(&crate::hittest::Crosshair { index: 0, price: 500.0 }, &pane, 320.0, 400.0)
            .is_none(),
        "off-window readout yields no spans"
    );
    assert!(
        oscillator_points(&[Some(100.0)], &pane)[0].is_some(),
        "in-window legs still map"
    );
}

fn master_top() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(31, 119, 180, 128)
}

/// The gradient blender refuses non-finite legs instead of saturating a
/// black vertex; finite endpoints stay exact.
#[test]
fn area_vertex_color_refuses_non_finite() {
    let (top, bottom) = (master_top(), egui::Color32::from_rgba_unmultiplied(31, 119, 180, 0));
    assert_eq!(
        area_vertex_color(top, bottom, 0.0, 0.0, 200.0).expect("top blends"),
        top,
        "gradient starts exactly on the top color"
    );
    assert_eq!(
        area_vertex_color(top, bottom, 200.0, 0.0, 200.0).expect("foot blends"),
        bottom,
        "gradient lands exactly on the bottom color"
    );
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(area_vertex_color(top, bottom, bad, 0.0, 200.0), None, "y refuses {bad}");
        assert_eq!(area_vertex_color(top, bottom, 100.0, bad, 200.0), None, "top refuses {bad}");
        assert_eq!(area_vertex_color(top, bottom, 100.0, 0.0, bad), None, "bottom refuses {bad}");
    }
}

/// All three candle emitters paint through one body: healthy inputs paint,
/// a broken theme refuses with the key named, and no emitter substitutes.
#[test]
fn candle_emitters_share_one_body() {
    let (ctx, painter) = test_painter();
    let theme = ThemeConfig::default();
    let geoms = vec![candle_geom(0), candle_geom(1)];
    paint_candles(&painter, &geoms, 5.0, &theme).expect("trend-keyed candles paint");
    paint_candles_as(&painter, &geoms, "bull", 5.0, &theme).expect("single-key candles paint");
    let tinted: Vec<TintedCandle> =
        geoms.iter().map(|geom| TintedCandle { geom: *geom, color: theme_bull() }).collect();
    paint_candles_tinted(&painter, &tinted, 5.0);
    let output = end_frame(&ctx);
    assert!(!output.shapes.is_empty(), "candles emit shapes");
    // A theme that cannot resolve the trend key refuses instead of painting
    // a default color.
    let broken = ThemeConfig::new().bull("red".to_owned()).build();
    let refused = paint_candles(&painter, &geoms, 5.0, &broken).expect_err("malformed key refuses");
    assert!(refused.to_string().contains("bull"), "refusal names the key: {refused}");
}

fn theme_bull() -> egui::Color32 {
    resolve_color(&ThemeConfig::default(), "bull").expect("default theme carries bull")
}

fn theme_bear() -> egui::Color32 {
    resolve_color(&ThemeConfig::default(), "bear").expect("default theme carries bear")
}

fn theme_grid() -> egui::Color32 {
    resolve_color(&ThemeConfig::default(), "grid").expect("default theme carries grid")
}

/// Both fill emitters read one polygon path: healthy runs paint, degenerate
/// polygons (< 3 points) paint nothing, and a broken theme refuses.
#[test]
fn fill_emitters_share_one_polygon_path() {
    let (ctx, painter) = test_painter();
    let theme = ThemeConfig::default();
    let run = ribbon_run();
    let mut bear = run.clone();
    bear.bullish = false;
    bear.points = vec![(0.0, 60.0), (8.0, 62.0), (8.0, 90.0), (0.0, 92.0)];
    let degenerate = RibbonPolygon { indices: vec![0], points: vec![(0.0, 1.0)], bullish: true };
    // Fully opaque so the tessellated vertex colors carry the resolved slot
    // exactly; the shaded band resolves its own teal/purple slots.
    paint_ribbon_fill(&painter, &[run.clone(), bear, degenerate], &theme, 1.0)
        .expect("flat fill paints");
    paint_area_fill(&painter, &[run], master_top(), theme_grid());
    let output = end_frame(&ctx);
    assert!(!output.shapes.is_empty(), "fills emit shapes");
    let colors = frame_colors(&ctx, output);
    assert!(
        colors.contains(&egui::Color32::from_rgb(0x00, 0xBC, 0xD4)),
        "ribbon_bull paints teal"
    );
    assert!(
        colors.contains(&egui::Color32::from_rgb(0x7B, 0x1F, 0xA2)),
        "ribbon_bear paints purple"
    );
    let broken = ThemeConfig::new().ribbon_bull("red".to_owned()).build();
    // `ribbon_fill` resolves through the dedicated slot: a malformed
    // `ribbon_bull` slot refuses the flat fill with the key named.
    let refused =
        paint_ribbon_fill(&painter, &[ribbon_run()], &broken, 0.5).expect_err("refuses");
    assert!(refused.to_string().contains("ribbon_bull"), "refusal names the key: {refused}");
}

/// Every pane-local translator shifts exactly the axes its shape owns: both
/// axes for closed/filled/placed geometry, x only for gridlines, and y only
/// implied for legs; holes stay holes.
#[test]
fn translate_helpers_shift_pane_local_geometry_by_origin() {
    let origin = (10.0_f32, 20.0_f32);
    let shift_x = |x: f32| (x + origin.0).to_bits();
    let shift_y = |y: f32| (y + origin.1).to_bits();
    let candle = translate_candles(vec![candle_geom(1)], origin);
    assert_eq!(candle[0].x.to_bits(), shift_x(8.0));
    assert_eq!(candle[0].open_y.to_bits(), shift_y(200.0));
    assert_eq!(candle[0].high_y.to_bits(), shift_y(180.0));
    assert_eq!(candle[0].low_y.to_bits(), shift_y(220.0));
    assert_eq!(candle[0].close_y.to_bits(), shift_y(210.0));
    assert_eq!(candle[0].index, 1, "translation never reindexes");

    let polygon = translate_polygons(vec![ribbon_run()], origin);
    assert_eq!(polygon[0].points[0].0.to_bits(), shift_x(0.0));
    assert_eq!(polygon[0].points[0].1.to_bits(), shift_y(10.0));
    assert_eq!(polygon[0].points[2].0.to_bits(), shift_x(8.0));
    assert_eq!(polygon[0].points[2].1.to_bits(), shift_y(40.0));

    let breaks = translate_breaks(vec![BreakLine { index: 2, x: 16.0 }], origin.0);
    assert_eq!(breaks[0].x.to_bits(), shift_x(16.0));
    assert_eq!(breaks[0].index, 2);

    let points = translate_points(vec![Some((0.0, 100.0)), None], origin);
    let present = points[0].expect("present");
    assert_eq!(present.0.to_bits(), shift_x(0.0));
    assert_eq!(present.1.to_bits(), shift_y(100.0));
    assert_eq!(points[1], None, "holes stay holed");

    let dots =
        translate_dots(vec![PivotDot { index: 0, x: 8.0, y: 100.0, side: PivotSide::High }], origin);
    assert_eq!(dots[0].x.to_bits(), shift_x(8.0));
    assert_eq!(dots[0].y.to_bits(), shift_y(100.0));

    let markers = translate_markers(
        vec![MarkerDot { index: 0, x: 8.0, y: 100.0, style: AnnotationStyle::Bull }],
        origin,
    );
    assert_eq!(markers[0].x.to_bits(), shift_x(8.0));
    assert_eq!(markers[0].y.to_bits(), shift_y(100.0));

    let bars = translate_delta_bars(
        vec![DeltaBarGeom { index: 0, x: 8.0, buy_y: Some(10.0), sell_y: None, zero_y: 20.0 }],
        origin,
    );
    assert_eq!(bars[0].x.to_bits(), shift_x(8.0));
    assert_eq!(bars[0].buy_y.expect("buy present").to_bits(), shift_y(10.0));
    assert_eq!(bars[0].sell_y, None, "holed legs stay holed");
    assert_eq!(bars[0].zero_y.to_bits(), shift_y(20.0));
}

/// Threshold rules mirror through the shared predicate: in-window legs
/// draw, an off-window threshold draws nothing (asserted on pixels: no
/// line shapes), and a broken theme refuses.
#[test]
fn threshold_emitter_draws_mirrored_rules() {
    let pane = zero_pane();
    let (ctx, painter) = test_painter();
    let theme = ThemeConfig::default();
    paint_thresholds(&painter, 0.0, 320.0, 100.0, &pane, &theme).expect("in-window rules paint");
    let output = end_frame(&ctx);
    assert!(!output.shapes.is_empty(), "mirrored rules emit shapes");
    let (ctx, painter) = test_painter();
    paint_thresholds(&painter, 0.0, 320.0, 500.0, &pane, &theme)
        .expect("off-window rules are Ok");
    let output = end_frame(&ctx);
    assert!(output.shapes.is_empty(), "off-window rules draw nothing");
}

/// Dots agree on polarity through one mapping, asserted on pixels: pivot
/// highs lower to bearish markers, lows to bullish, and named annotations
/// keep their literal key — all through the one marker emitter.
#[test]
fn dot_emitters_agree_on_polarity() {
    let theme = ThemeConfig::default();
    let (bull, bear, grid) = (theme_bull(), theme_bear(), theme_grid());
    let pivot = |side| {
        MarkerDot::from_pivot(&PivotDot { index: 0, x: 8.0, y: 100.0, side })
    };
    for (name, markers, present, absent) in [
        ("pivot-high", vec![pivot(PivotSide::High)], bear, bull),
        ("pivot-low", vec![pivot(PivotSide::Low)], bull, bear),
        (
            "marker-bull",
            vec![MarkerDot {
                index: 0,
                x: 8.0,
                y: 100.0,
                style: AnnotationStyle::Bull,
            }],
            bull,
            bear,
        ),
        (
            "marker-bear",
            vec![MarkerDot {
                index: 0,
                x: 8.0,
                y: 100.0,
                style: AnnotationStyle::Bear,
            }],
            bear,
            bull,
        ),
        (
            "marker-note",
            vec![MarkerDot {
                index: 0,
                x: 8.0,
                y: 100.0,
                style: AnnotationStyle::Note,
            }],
            grid,
            bull,
        ),
    ] {
        let (ctx, painter) = test_painter();
        paint_markers(&painter, &markers, &theme).expect("markers paint");
        let colors = frame_colors(&ctx, end_frame(&ctx));
        assert!(
            colors.contains(&present),
            "{name} paints its polarity color"
        );
        assert!(
            colors.iter().all(|color| *color != absent),
            "{name} paints no opposing polarity color"
        );
    }
}

/// The remaining emitters paint healthy inputs and refuse broken themes:
/// every exported `paint_*` is named by at least one test in this file.
#[test]
fn every_other_emitter_paints_or_refuses() {
    let theme = ThemeConfig::default();
    let pane = price_pane();
    // `paint_oscillator`: holes lift the pen, present legs paint.
    let (ctx, painter) = test_painter();
    paint_oscillator(
        &painter,
        &[Some((0.0, 200.0)), Some((8.0, 195.0)), None, Some((24.0, 190.0)), Some((32.0, 185.0))],
        "fast",
        &theme,
        1.0,
    )
    .expect("oscillator paints");
    assert!(!end_frame(&ctx).shapes.is_empty(), "oscillator emits shapes");
    // `paint_crosshair`: snapped spans paint.
    let (ctx, painter) = test_painter();
    let lines = crosshair_lines(&crate::hittest::Crosshair { index: 2, price: 100.0 }, &pane, 320.0, 400.0)
        .expect("readout maps");
    paint_crosshair(&painter, &lines, &theme).expect("crosshair paints");
    assert!(!end_frame(&ctx).shapes.is_empty(), "crosshair emits shapes");
    // `paint_delta_bars`: both legs paint on a spanning pane.
    let (ctx, painter) = test_painter();
    let zero = zero_pane();
    let delta = crate::view::DeltaView {
        buy_top: vec![Some(50.0)],
        sell_bottom: vec![Some(60.0)],
    };
    let bars = delta_bar_geoms(&delta, &zero).expect("spanning pane builds");
    paint_delta_bars(&painter, &bars, 3.0, &theme).expect("delta bars paint");
    assert!(!end_frame(&ctx).shapes.is_empty(), "delta bars emit shapes");
    // `paint_ribbon`: strips paint under both trend keys.
    let (ctx, painter) = test_painter();
    paint_ribbon(
        &painter,
        &[Some(RibbonBand { index: 0, x: 8.0, top_y: 10.0, bottom_y: 20.0 }), None],
        3.0,
        &theme,
        &[true, false],
        0.5,
    )
    .expect("ribbon paints");
    assert!(!end_frame(&ctx).shapes.is_empty(), "ribbon emits shapes");
    // `paint_session_breaks`: gridlines paint.
    let (ctx, painter) = test_painter();
    paint_session_breaks(&painter, &[BreakLine { index: 2, x: 16.0 }], 0.0, 400.0, &theme)
        .expect("breaks paint");
    assert!(!end_frame(&ctx).shapes.is_empty(), "breaks emit shapes");
    // `paint_error_chip`: renders the reason without panicking.
    egui::__run_test_ui(|ui| {
        paint_error_chip(ui, &EguiError::InvalidParam {
            what: "probe".to_owned(),
            reason: "chip renders".to_owned(),
        });
    });
    // Broken themes refuse with the key named, on every keyed emitter.
    let broken = ThemeConfig::new().grid("red".to_owned()).build();
    let (_, painter) = test_painter();
    for refused in [
        paint_oscillator(&painter, &[Some((0.0, 1.0))], "grid", &broken, 1.0),
        paint_thresholds(&painter, 0.0, 1.0, 100.0, &zero, &broken),
        paint_crosshair(&painter, &lines, &broken),
        paint_session_breaks(&painter, &[BreakLine { index: 0, x: 0.0 }], 0.0, 1.0, &broken),
    ] {
        let refused = refused.expect_err("malformed grid slot refuses");
        assert!(refused.to_string().contains("grid"), "refusal names the key: {refused}");
    }
}

/// Procedural control icons are deterministic headless geometry: an eye with
/// lid arcs and a pupil (plus a slash exactly when hidden), a six-sided bolt
/// with a bore, and a search lens with a handle. Bad radii refuse.
#[test]
fn procedural_icons_build_deterministic_geometry_and_refuse_bad_radii() {
    let center = (100.0f32, 200.0f32);
    // The shown eye: lids above and below center, pupil on center, no slash.
    let shown = eye_icon_geom(center, 12.0, true).expect("a valid eye builds");
    assert_eq!(shown.center.0.to_bits(), center.0.to_bits());
    assert_eq!(shown.center.1.to_bits(), center.1.to_bits());
    assert_eq!(shown.radius.to_bits(), 12.0f32.to_bits());
    assert!(!shown.upper_arc.is_empty() && !shown.lower_arc.is_empty(), "lids arc");
    for point in &shown.upper_arc {
        assert!(point.1 <= center.1, "the upper lid stays above center: {point:?}");
    }
    for point in &shown.lower_arc {
        assert!(point.1 >= center.1, "the lower lid stays below center: {point:?}");
    }
    assert_eq!(shown.pupil_center.0.to_bits(), center.0.to_bits());
    assert_eq!(shown.pupil_center.1.to_bits(), center.1.to_bits());
    assert!(
        shown.pupil_radius > 0.0 && shown.pupil_radius < shown.radius,
        "the pupil sits inside the lids: {}",
        shown.pupil_radius
    );
    assert_eq!(shown.slash, None, "a shown eye carries no slash");
    // The hidden eye adds exactly one slash across the lids.
    let hidden = eye_icon_geom(center, 12.0, false).expect("a hidden eye builds");
    let (from, to) = hidden.slash.expect("a hidden eye carries a slash");
    assert_ne!(from, to, "the slash has two distinct ends");
    // Same inputs build byte-identical geometry twice.
    assert_eq!(shown, eye_icon_geom(center, 12.0, true).expect("rebuilds"));
    // The six-sided bolt: exactly six vertices on the rim plus a bore.
    let bolt = hex_bolt_icon_geom(center, 12.0).expect("a valid bolt builds");
    assert_eq!(bolt.vertices.len(), 6, "a hex bolt has six rim vertices");
    for vertex in &bolt.vertices {
        let dx = f64::from(vertex.0 - center.0);
        let dy = f64::from(vertex.1 - center.1);
        let distance = (dx * dx + dy * dy).sqrt();
        assert!(
            (distance - 12.0).abs() <= 1e-4,
            "rim vertex sits at radius distance: {vertex:?}"
        );
    }
    assert_eq!(bolt.bore_center.0.to_bits(), center.0.to_bits());
    assert!(
        bolt.bore_radius > 0.0 && bolt.bore_radius < bolt.radius,
        "the bore sits inside the rim: {}",
        bolt.bore_radius
    );
    // The search lens: a lens circle plus a handle leaving the rim.
    let lens = search_icon_geom(center, 12.0).expect("a valid lens builds");
    assert!(
        lens.lens_radius > 0.0 && lens.lens_radius < lens.radius,
        "the lens sits inside the icon bounds: {}",
        lens.lens_radius
    );
    let (heel, tip) = lens.handle;
    assert_ne!(heel, tip, "the handle has two distinct ends");
    let rim_dx = f64::from(heel.0 - center.0);
    let rim_dy = f64::from(heel.1 - center.1);
    let rim_distance = (rim_dx * rim_dx + rim_dy * rim_dy).sqrt();
    assert!(
        (rim_distance - f64::from(lens.lens_radius)).abs() <= 1e-4,
        "the handle starts at the lens rim: {heel:?}"
    );
    // Bad radii refuse on every builder, as do non-finite centers.
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -4.0] {
        assert_eq!(eye_icon_geom(center, bad, true), None, "the eye refuses radius {bad}");
        assert_eq!(eye_icon_geom(center, bad, false), None, "the slashed eye refuses {bad}");
        assert_eq!(hex_bolt_icon_geom(center, bad), None, "the bolt refuses radius {bad}");
        assert_eq!(search_icon_geom(center, bad), None, "the lens refuses radius {bad}");
    }
    assert_eq!(eye_icon_geom((f32::NAN, 0.0), 12.0, true), None, "the eye refuses a bad center");
    assert_eq!(hex_bolt_icon_geom((0.0, f32::INFINITY), 12.0), None, "the bolt refuses a bad center");
}

/// The icon emitters blit their geometry through one theme key each: healthy
/// inputs paint, and a broken theme refuses with the key named.
#[test]
fn procedural_icon_emitters_paint_and_refuse_broken_themes() {
    let (ctx, painter) = test_painter();
    let theme = ThemeConfig::default();
    let eye = eye_icon_geom((100.0, 100.0), 12.0, true).expect("eye builds");
    let hidden = eye_icon_geom((140.0, 100.0), 12.0, false).expect("slashed eye builds");
    let bolt = hex_bolt_icon_geom((180.0, 100.0), 12.0).expect("bolt builds");
    let lens = search_icon_geom((220.0, 100.0), 12.0).expect("lens builds");
    paint_eye_icon(&painter, &eye, "bull", &theme).expect("the shown eye paints");
    paint_eye_icon(&painter, &hidden, "bear", &theme).expect("the slashed eye paints");
    paint_hex_bolt_icon(&painter, &bolt, "grid", &theme).expect("the bolt paints");
    paint_search_icon(&painter, &lens, "fast", &theme).expect("the lens paints");
    let output = end_frame(&ctx);
    assert!(!output.shapes.is_empty(), "icons emit shapes");
    // A theme that cannot resolve the icon key refuses instead of painting a
    // default color.
    let broken = ThemeConfig::new().bull("red".to_owned()).build();
    let refused =
        paint_eye_icon(&painter, &eye, "bull", &broken).expect_err("malformed slot refuses");
    assert!(refused.to_string().contains("bull"), "refusal names the key: {refused}");
    let unknown =
        paint_hex_bolt_icon(&painter, &bolt, "nope", &theme).expect_err("unknown key refuses");
    assert!(unknown.to_string().contains("nope"), "refusal names the key: {unknown}");
    let unknown =
        paint_search_icon(&painter, &lens, "nope", &theme).expect_err("unknown key refuses");
    assert!(unknown.to_string().contains("nope"), "refusal names the key: {unknown}");
}
