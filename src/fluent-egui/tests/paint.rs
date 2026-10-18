use super::*;
use crate::view::{delta_candle_view, pivot_marks, ribbon_view};
use protopyne_chart_model::{AnnotationStyle, ChartRow};
use protopyne_chart_scale::PriceScaleMode;

fn price_pane() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid price pane")
}

fn zero_pane() -> Viewport {
    Viewport::new(
        PriceScaleMode::Normal,
        -300.0,
        300.0,
        200.0,
        8.0,
        0.0,
        false,
    )
    .expect("valid zero pane")
}

fn row(price: f64, volume: u64) -> ChartRow {
    ChartRow {
        time: 0,
        open: price,
        high: price + 1.0,
        low: price - 1.0,
        close: price,
        volume,
    }
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
/// Pixel comparisons in the width rules are exact by construction (the
/// constants are exactly representable), but a tolerance keeps a failing
/// assertion about the rule rather than about float representation.
fn close_to(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() <= 1e-4 * expected.abs().max(1.0)
}

fn zero_quantum() -> f64 {
    (300.0 - -300.0) / 200.0 * 8.0
}

#[test]
fn candles_line_up_with_rows_and_trend_keys() {
    let pane = price_pane();
    let rows = vec![row(100.0, 10), row(101.0, 20), row(99.0, 30)];
    let trend = vec![true, false, true];
    let geoms = candle_geoms(&rows, &trend, &pane).expect("aligned inputs paint");
    assert_eq!(
        geoms.len(),
        rows.len(),
        "converted rows are conserved, never dropped here"
    );
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
    assert!(
        candle_geoms(&rows, &[true], &pane).is_err(),
        "key mismatch refuses"
    );
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
    assert!(
        first.top_y <= first.bottom_y,
        "fill runs downward: {first:?}"
    );
    assert_eq!(bands[1], None, "holes stay absent, never fabricated");
    let third = bands[2].expect("present");
    assert!(third.top_y <= third.bottom_y + f32::EPSILON);
}

#[test]
fn delta_bars_hang_from_the_zero_line() {
    let pane = zero_pane();
    let zero_y = pane.price_to_y(0.0).expect("zero spans");
    let rows = vec![
        ChartRow {
            time: 0,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 1000,
        },
        ChartRow {
            time: 1,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 4000,
        },
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
    let delta = DeltaView {
        buy_top: vec![Some(5.0)],
        sell_bottom: vec![Some(5.0)],
    };
    assert!(
        delta_bar_geoms(&delta, &narrow).is_err(),
        "bars need a zero line"
    );
    let pane = zero_pane();
    let split = DeltaView {
        buy_top: vec![Some(5.0)],
        sell_bottom: Vec::new(),
    };
    assert!(
        delta_bar_geoms(&split, &pane).is_err(),
        "split mismatch refuses"
    );
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
        assert_mapped_price(
            &pane,
            dot.y,
            fast[dot.index].expect("crossings carry values"),
            zero_quantum(),
        );
        let slot = pane.x_to_index(dot.x).expect("painted slots invert");
        assert!(
            (slot - dot.index as f64).abs() <= 1e-9,
            "dot {} sits on bar {}",
            dot.index,
            dot.index
        );
    }
    assert!(pivot_dots(&marks, &[None, None, None, None], &pane).is_empty());
}

#[test]
fn threshold_rules_mirror_about_zero() {
    let pane = zero_pane();
    let zero_y = pane.price_to_y(0.0).expect("zero spans");
    let plus = threshold_line(100.0, &pane).expect("rule maps");
    let minus = pane.price_to_y(-100.0).expect("mirror maps");
    assert!(
        (plus + minus - 2.0 * zero_y).abs() < 1e-3,
        "rules mirror about zero"
    );
    assert!(plus < zero_y, "positive rules sit above zero on screen");
    for bad in [0.0, -50.0, f64::NAN] {
        assert_eq!(
            threshold_line(bad, &pane),
            None,
            "threshold {bad} draws nothing"
        );
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
        &[
            Some(100.0),
            Some(101.0),
            Some(100.0),
            Some(100.0),
            Some(100.0),
        ],
        0.5,
    )
    .expect("valid ribbon");
    let polygons = ribbon_polygons(&ribbon, &pane);
    assert_eq!(polygons.len(), 3, "bull run, bear run, hole, lone bar");
    assert_eq!(polygons[0].indices, vec![0, 1]);
    assert!(polygons[0].bullish);
    assert_eq!(
        polygons[0].points.len(),
        4,
        "upper forward plus lower reversed"
    );
    assert_eq!(polygons[1].indices, vec![2]);
    assert!(!polygons[1].bullish, "trend flip starts a new run");
    assert_eq!(
        polygons[2].indices,
        vec![4],
        "holes break runs, never bridge"
    );
    for polygon in &polygons {
        assert!(polygon.points.len() >= 4 || polygon.indices.len() == 1);
    }
    let empty = ribbon_view(&[None, None], &[None, None], 0.5).expect("holed ribbon");
    assert!(
        ribbon_polygons(&empty, &pane).is_empty(),
        "fully holed input fills nothing"
    );
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
        ribbon
            .bullish
            .iter()
            .zip(upper.iter())
            .all(|(trend, leg)| leg.is_none() || *trend),
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
    assert!(
        pivot_price_dots(&marks, &rows[..1], &pane).is_empty(),
        "past-the-rows marks skip"
    );
}

#[test]
fn off_window_zero_line_is_refused() {
    // A pane that does not span zero has no zero line to hang delta bars
    // from: the oscillator/delta zero path refuses rather than extrapolating.
    let positive = Viewport::new(PriceScaleMode::Normal, 10.0, 20.0, 200.0, 8.0, 10.0, false)
        .expect("valid viewport");
    let delta = DeltaView {
        buy_top: vec![Some(5.0)],
        sell_bottom: vec![Some(5.0)],
    };
    assert!(
        delta_bar_geoms(&delta, &positive).is_err(),
        "zero off-window refuses"
    );
    // The inverse direction agrees: an off-pane pixel has no price.
    assert_eq!(positive.y_to_price(-1.0), None, "off-pane pixel refuses");
    assert_eq!(positive.y_to_price(201.0), None, "off-pane pixel refuses");
}

#[test]
fn threshold_rules_refuse_off_window_values() {
    let pane = price_pane(); // window 90..110
    assert!(
        threshold_line(100.0, &pane).is_some(),
        "in-window rule maps"
    );
    assert_eq!(
        threshold_line(500.0, &pane),
        None,
        "above-window rule draws nothing"
    );
    assert_eq!(
        threshold_line(1.0, &pane),
        None,
        "below-window rule draws nothing"
    );
}

#[test]
fn pivot_price_dots_skip_off_window_extremes() {
    let pane = price_pane(); // window 90..110
    let marks = vec![
        PivotMark {
            index: 0,
            side: PivotSide::High,
        },
        PivotMark {
            index: 1,
            side: PivotSide::High,
        },
    ];
    // Bar 0's high (120) runs off the window; bar 1's high (101) is in-window.
    let rows = vec![
        ChartRow {
            high: 120.0,
            ..row(100.0, 10)
        },
        ChartRow {
            high: 101.0,
            ..row(100.0, 10)
        },
    ];
    let dots = pivot_price_dots(&marks, &rows, &pane);
    assert_eq!(dots.len(), 1, "only the in-window extreme decorates");
    assert_eq!(dots[0].index, 1);
}

#[test]
fn pivots_lower_to_markers_without_moving() {
    // The retired pivot emitter's shape lives on in the marker emitter:
    // lowering carries geometry bit-for-bit and maps the side through the
    // one shared polarity (highs bearish, lows bullish).
    for (side, style) in [
        (PivotSide::High, AnnotationStyle::Bear),
        (PivotSide::Low, AnnotationStyle::Bull),
    ] {
        let marker = MarkerDot::from_pivot(&PivotDot {
            index: 3,
            x: 32.0,
            y: 200.0,
            side,
        });
        assert_eq!(marker.index, 3, "the bar does not move");
        assert_eq!(marker.x.to_bits(), 32.0f32.to_bits(), "x does not move");
        assert_eq!(marker.y.to_bits(), 200.0f32.to_bits(), "y does not move");
        assert_eq!(marker.style, style, "{side:?} lowers to {style:?}");
    }
}

#[test]
fn transparency_maps_percent_to_byte_opacity() {
    assert_eq!(
        opacity_from_transparency(0.0),
        255,
        "zero transparency is opaque"
    );
    assert_eq!(
        opacity_from_transparency(100.0),
        0,
        "full transparency is invisible"
    );
    assert_eq!(
        opacity_from_transparency(60.0),
        102,
        "the specified dimming lands at forty percent opacity"
    );
    assert_eq!(
        opacity_from_transparency(140.0),
        0,
        "overshoot clamps to invisible"
    );
    assert_eq!(
        opacity_from_transparency(-20.0),
        255,
        "undershoot clamps to opaque"
    );
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
    let readout = Crosshair {
        index: 3,
        price: 100.0,
    };
    let lines = crosshair_lines(&readout, &pane, 320.0, 400.0).expect("readout maps");
    // The snapped readout pixel, hand-derived from the documented mapping
    // (bar slots start at bar zero on the 8 px spacing; y runs
    // range-top-to-bottom over the 90..110 window on 400 px): slot 3 sits at
    // x = 24, and 100.0 sits halfway down at y = 200.
    assert_eq!(
        lines.x.to_bits(),
        24.0f32.to_bits(),
        "span x is the snapped bar pixel"
    );
    assert_eq!(
        lines.y.to_bits(),
        200.0f32.to_bits(),
        "span y is the snapped price pixel"
    );
    assert_eq!(
        (lines.left, lines.right, lines.top, lines.bottom),
        (0.0, 320.0, 0.0, 400.0)
    );
    assert!(
        crosshair_lines(
            &Crosshair {
                index: 0,
                price: f64::NAN
            },
            &pane,
            320.0,
            400.0
        )
        .is_none(),
        "unmappable price yields no spans"
    );
    assert!(
        crosshair_lines(&readout, &pane, 0.0, 400.0).is_none(),
        "degenerate extents yield no spans"
    );
}

#[test]
fn resolve_color_errors_visibly() {
    // Known keys resolve to their parsed `#RRGGBB` slot; nothing here falls
    // back to a silent default.
    let theme = ThemeConfig::new().bull("#112233".to_owned()).build();
    let color = resolve_color(&theme, "bull").expect("known key resolves");
    assert_eq!(
        (color.r(), color.g(), color.b(), color.a()),
        (0x11, 0x22, 0x33, 255)
    );
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
    let painter = egui::Painter::new(
        ctx.clone(),
        egui::LayerId::background(),
        egui::Rect::EVERYTHING,
    );
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
            egui::epaint::Primitive::Mesh(mesh) => mesh
                .vertices
                .into_iter()
                .map(|vertex| vertex.color)
                .collect(),
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
        let rows = vec![
            row(100.0, 10),
            ChartRow {
                high: bad,
                ..row(100.0, 10)
            },
        ];
        let geoms = candle_geoms(&rows, &[true, true], &pane).expect("lengths align");
        assert_eq!(
            geoms.len(),
            1,
            "the non-finite bar skips, its neighbor paints ({bad})"
        );
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
                &crate::hittest::Crosshair {
                    index: 0,
                    price: bad
                },
                &pane
            ),
            None,
            "readout pixel refuses {bad}"
        );
        assert!(
            crosshair_lines(
                &crate::hittest::Crosshair {
                    index: 0,
                    price: bad
                },
                &pane,
                320.0,
                400.0
            )
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
    assert_eq!(
        threshold_line(500.0, &pane),
        None,
        "off-window rule draws nothing"
    );
    assert!(
        crosshair_lines(
            &crate::hittest::Crosshair {
                index: 0,
                price: 500.0
            },
            &pane,
            320.0,
            400.0
        )
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
    let (top, bottom) = (
        master_top(),
        egui::Color32::from_rgba_unmultiplied(31, 119, 180, 0),
    );
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
        assert_eq!(
            area_vertex_color(top, bottom, bad, 0.0, 200.0),
            None,
            "y refuses {bad}"
        );
        assert_eq!(
            area_vertex_color(top, bottom, 100.0, bad, 200.0),
            None,
            "top refuses {bad}"
        );
        assert_eq!(
            area_vertex_color(top, bottom, 100.0, 0.0, bad),
            None,
            "bottom refuses {bad}"
        );
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
    let tinted: Vec<TintedCandle> = geoms
        .iter()
        .map(|geom| TintedCandle {
            geom: *geom,
            color: theme_bull(),
        })
        .collect();
    paint_candles_tinted(&painter, &tinted, 5.0);
    let output = end_frame(&ctx);
    assert!(!output.shapes.is_empty(), "candles emit shapes");
    // A theme that cannot resolve the trend key refuses instead of painting
    // a default color.
    let broken = ThemeConfig::new().bull("red".to_owned()).build();
    let refused = paint_candles(&painter, &geoms, 5.0, &broken).expect_err("malformed key refuses");
    assert!(
        refused.to_string().contains("bull"),
        "refusal names the key: {refused}"
    );
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
    let degenerate = RibbonPolygon {
        indices: vec![0],
        points: vec![(0.0, 1.0)],
        bullish: true,
    };
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
    let refused = paint_ribbon_fill(&painter, &[ribbon_run()], &broken, 0.5).expect_err("refuses");
    assert!(
        refused.to_string().contains("ribbon_bull"),
        "refusal names the key: {refused}"
    );
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

    let dots = translate_dots(
        vec![PivotDot {
            index: 0,
            x: 8.0,
            y: 100.0,
            side: PivotSide::High,
        }],
        origin,
    );
    assert_eq!(dots[0].x.to_bits(), shift_x(8.0));
    assert_eq!(dots[0].y.to_bits(), shift_y(100.0));

    let markers = translate_markers(
        vec![MarkerDot {
            index: 0,
            x: 8.0,
            y: 100.0,
            style: AnnotationStyle::Bull,
        }],
        origin,
    );
    assert_eq!(markers[0].x.to_bits(), shift_x(8.0));
    assert_eq!(markers[0].y.to_bits(), shift_y(100.0));

    let bars = translate_delta_bars(
        vec![DeltaBarGeom {
            index: 0,
            x: 8.0,
            buy_y: Some(10.0),
            sell_y: None,
            zero_y: 20.0,
        }],
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
    paint_thresholds(&painter, 0.0, 320.0, 500.0, &pane, &theme).expect("off-window rules are Ok");
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
        MarkerDot::from_pivot(&PivotDot {
            index: 0,
            x: 8.0,
            y: 100.0,
            side,
        })
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
        &[
            Some((0.0, 200.0)),
            Some((8.0, 195.0)),
            None,
            Some((24.0, 190.0)),
            Some((32.0, 185.0)),
        ],
        "fast",
        &theme,
        1.0,
    )
    .expect("oscillator paints");
    assert!(
        !end_frame(&ctx).shapes.is_empty(),
        "oscillator emits shapes"
    );
    // `paint_crosshair`: snapped spans paint.
    let (ctx, painter) = test_painter();
    let lines = crosshair_lines(
        &crate::hittest::Crosshair {
            index: 2,
            price: 100.0,
        },
        &pane,
        320.0,
        400.0,
    )
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
    // `paint_delta_bars_tinted`: both legs paint a caller-resolved tint.
    let (ctx, painter) = test_painter();
    let tinted: Vec<TintedDeltaBar> = bars
        .iter()
        .map(|geom| TintedDeltaBar {
            geom: *geom,
            color: egui::Color32::from_rgb(0, 188, 212),
        })
        .collect();
    paint_delta_bars_tinted(&painter, &tinted, 3.0);
    assert!(
        !end_frame(&ctx).shapes.is_empty(),
        "tinted delta bars emit shapes"
    );
    // `paint_ribbon`: strips paint under both trend keys.
    let (ctx, painter) = test_painter();
    paint_ribbon(
        &painter,
        &[
            Some(RibbonBand {
                index: 0,
                x: 8.0,
                top_y: 10.0,
                bottom_y: 20.0,
            }),
            None,
        ],
        3.0,
        &theme,
        &[true, false],
        0.5,
    )
    .expect("ribbon paints");
    assert!(!end_frame(&ctx).shapes.is_empty(), "ribbon emits shapes");
    // `paint_session_breaks`: gridlines paint.
    let (ctx, painter) = test_painter();
    paint_session_breaks(
        &painter,
        &[BreakLine { index: 2, x: 16.0 }],
        0.0,
        400.0,
        &theme,
    )
    .expect("breaks paint");
    assert!(!end_frame(&ctx).shapes.is_empty(), "breaks emit shapes");
    // `paint_error_chip`: renders the reason without panicking.
    egui::__run_test_ui(|ui| {
        paint_error_chip(
            ui,
            &EguiError::InvalidParam {
                what: "probe".to_owned(),
                reason: "chip renders".to_owned(),
            },
        );
    });
    // Broken themes refuse with the key named, on every keyed emitter.
    let broken = ThemeConfig::new().grid("red".to_owned()).build();
    let (_, painter) = test_painter();
    for refused in [
        paint_oscillator(&painter, &[Some((0.0, 1.0))], "grid", &broken, 1.0),
        paint_thresholds(&painter, 0.0, 1.0, 100.0, &zero, &broken),
        paint_crosshair(&painter, &lines, &broken),
        paint_session_breaks(
            &painter,
            &[BreakLine { index: 0, x: 0.0 }],
            0.0,
            1.0,
            &broken,
        ),
    ] {
        let refused = refused.expect_err("malformed grid slot refuses");
        assert!(
            refused.to_string().contains("grid"),
            "refusal names the key: {refused}"
        );
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
    assert!(
        !shown.upper_arc.is_empty() && !shown.lower_arc.is_empty(),
        "lids arc"
    );
    for point in &shown.upper_arc {
        assert!(
            point.1 <= center.1,
            "the upper lid stays above center: {point:?}"
        );
    }
    for point in &shown.lower_arc {
        assert!(
            point.1 >= center.1,
            "the lower lid stays below center: {point:?}"
        );
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
        assert_eq!(
            eye_icon_geom(center, bad, true),
            None,
            "the eye refuses radius {bad}"
        );
        assert_eq!(
            eye_icon_geom(center, bad, false),
            None,
            "the slashed eye refuses {bad}"
        );
        assert_eq!(
            hex_bolt_icon_geom(center, bad),
            None,
            "the bolt refuses radius {bad}"
        );
        assert_eq!(
            search_icon_geom(center, bad),
            None,
            "the lens refuses radius {bad}"
        );
    }
    assert_eq!(
        eye_icon_geom((f32::NAN, 0.0), 12.0, true),
        None,
        "the eye refuses a bad center"
    );
    assert_eq!(
        hex_bolt_icon_geom((0.0, f32::INFINITY), 12.0),
        None,
        "the bolt refuses a bad center"
    );
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
    assert!(
        refused.to_string().contains("bull"),
        "refusal names the key: {refused}"
    );
    let unknown =
        paint_hex_bolt_icon(&painter, &bolt, "nope", &theme).expect_err("unknown key refuses");
    assert!(
        unknown.to_string().contains("nope"),
        "refusal names the key: {unknown}"
    );
    let unknown =
        paint_search_icon(&painter, &lens, "nope", &theme).expect_err("unknown key refuses");
    assert!(
        unknown.to_string().contains("nope"),
        "refusal names the key: {unknown}"
    );
}

#[test]
fn the_candle_body_follows_the_time_bar_it_draws() {
    // The default: the body *is* the unit of time as displayed, so the bars
    // read as one tape at ordinary zoom.
    for spacing in [1.0_f32, 4.0, 8.0, CANDLE_GAP_THRESHOLD_PX] {
        assert!(
            close_to(candle_body_width(spacing), spacing),
            "a bar at or under the gap threshold fills its own slot: {spacing}"
        );
    }
    // Past the threshold neighbours run together, so the body takes its share
    // and the gap separates them.
    assert!(
        close_to(
            candle_body_width(CANDLE_GAP_THRESHOLD_PX + 1.0),
            (CANDLE_GAP_THRESHOLD_PX + 1.0) * CANDLE_BODY_FRACTION
        ),
        "a wider bar keeps the same share"
    );
    assert!(
        close_to(candle_body_width(100.0), 80.0),
        "at 100 px that share is 80%"
    );
}

#[test]
fn a_zoomed_out_candle_stays_visible_rather_than_vanishing() {
    // Below one pixel the body would stop painting, which hides data by
    // zooming; the floor is the contract.
    for spacing in [0.0_f32, 0.2, 0.9, -5.0] {
        assert!(
            close_to(candle_body_width(spacing), CANDLE_MIN_BODY_PX),
            "spacing {spacing} still paints a visible mark"
        );
    }
    // A non-finite spacing is not a measurement; it reads as the minimum
    // rather than propagating a NaN into a rect.
    for spacing in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(close_to(candle_body_width(spacing), CANDLE_MIN_BODY_PX));
    }
}

#[test]
fn candle_body_width_grows_within_each_zoom_band_and_never_exceeds_its_slot() {
    // Each band is monotone in the bar width, and neither can paint a body
    // wider than the slot it lives in. The one step between them is the rule
    // itself: a bar has to get past the threshold before its body gives up
    // the gap, and that switch is a single notch of zoom.
    for (range, fraction) in [
        (0.0_f32..CANDLE_GAP_THRESHOLD_PX, 1.0_f32),
        (CANDLE_GAP_THRESHOLD_PX + 0.5..64.0, CANDLE_BODY_FRACTION),
    ] {
        let mut previous = 0.0_f32;
        let mut spacing = range.start;
        while spacing <= range.end {
            let width = candle_body_width(spacing);
            assert!(
                width >= previous,
                "{fraction} band shrinks: {spacing} -> {width}"
            );
            assert!(
                width <= spacing.max(CANDLE_MIN_BODY_PX),
                "{fraction} band is wider than its slot: {spacing} -> {width}"
            );
            previous = width;
            spacing += 0.5;
        }
    }
    assert!(
        close_to(
            candle_body_width(CANDLE_GAP_THRESHOLD_PX + 0.5),
            (CANDLE_GAP_THRESHOLD_PX + 0.5) * CANDLE_BODY_FRACTION
        ),
        "and the switch happens exactly at the threshold, not before it"
    );
}

#[test]
fn a_hex_slot_is_read_by_one_reader_for_both_themes_and_overrides() {
    // The theme slot and the per-indicator override are the same bytes, so
    // they go through the same reader and refuse the same way.
    assert_eq!(
        parse_hex_color("fast", "#20C8F0").expect("a theme slot parses"),
        egui::Color32::from_rgb(0x20, 0xC8, 0xF0)
    );
    assert_eq!(
        parse_hex_color("fast", "#20c8f0").expect("hex is case-insensitive"),
        egui::Color32::from_rgb(0x20, 0xC8, 0xF0)
    );
    for bad in [
        "", "#", "#12345", "#1234567", "20C8F0", "#GGGGGG", "#20C8FG",
    ] {
        assert!(
            parse_hex_color("fast", bad).is_err(),
            "{bad:?} is not a color"
        );
    }
}

fn ohlc(time: i64, open: f64, high: f64, low: f64, close: f64, volume: u64) -> ChartRow {
    ChartRow {
        time,
        open,
        high,
        low,
        close,
        volume,
    }
}

#[test]
fn column_runs_groups_consecutive_equal_columns() {
    assert_eq!(
        column_runs(10, |slot| (slot / 3) as i64),
        vec![(0, 0..3), (1, 3..6), (2, 6..9), (3, 9..10)]
    );
    assert!(
        column_runs(0, |slot| slot as i64).is_empty(),
        "no bars, no runs"
    );
    assert_eq!(column_runs(1, |_| 7), vec![(7, 0..1)]);
    // Consecutive-equal grouping even under a non-monotonic mapping: the
    // caller owns monotonicity, this owns the grouping.
    assert_eq!(
        column_runs(4, |slot| [0, 0, 1, 0][slot]),
        vec![(0, 0..2), (1, 2..3), (0, 3..4)]
    );
}

#[test]
fn aggregate_column_folds_first_open_extreme_span_last_close() {
    let rows = vec![
        ohlc(10, 100.0, 102.0, 99.0, 101.0, 1_000),
        ohlc(11, 101.0, 105.0, 100.0, 104.0, 2_000),
        ohlc(12, 104.0, 104.5, 98.0, 99.0, 500),
    ];
    assert_eq!(
        aggregate_column(&rows),
        Some(ohlc(10, 100.0, 105.0, 98.0, 99.0, 3_500)),
        "open rides first, high/low span the run, close rides last, volume sums, time rides first"
    );
    let single = vec![ohlc(3, 50.0, 51.0, 49.0, 50.5, 7)];
    assert_eq!(
        aggregate_column(&single),
        Some(single[0]),
        "one row round-trips"
    );
    assert_eq!(aggregate_column(&[]), None, "an empty column draws nothing");
}

#[test]
fn aggregate_column_volume_saturates_instead_of_wrapping() {
    let rows = vec![
        ohlc(0, 1.0, 1.0, 1.0, 1.0, u64::MAX),
        ohlc(1, 1.0, 1.0, 1.0, 1.0, 1),
    ];
    assert_eq!(
        aggregate_column(&rows).expect("non-empty").volume,
        u64::MAX,
        "a month of heavy bars must clamp, never wrap to a thin column"
    );
}

#[test]
fn partition_then_fold_matches_a_manual_column_fold() {
    let rows = [
        ohlc(0, 10.0, 11.0, 9.0, 10.5, 100),
        ohlc(1, 10.5, 12.0, 10.0, 11.0, 200),
        ohlc(2, 11.0, 11.5, 8.0, 9.0, 300),
        ohlc(3, 9.0, 9.5, 9.0, 9.25, 400),
        ohlc(4, 9.25, 20.0, 9.0, 19.0, 500),
    ];
    let runs = column_runs(rows.len(), |slot| (slot / 2) as i64);
    let folded: Vec<ChartRow> = runs
        .iter()
        .map(|(_, range)| aggregate_column(&rows[range.clone()]).expect("runs are non-empty"))
        .collect();
    assert_eq!(folded.len(), 3);
    assert_eq!(folded[0], ohlc(0, 10.0, 12.0, 9.0, 11.0, 300));
    assert_eq!(folded[1], ohlc(2, 11.0, 11.5, 8.0, 9.25, 700));
    assert_eq!(folded[2], ohlc(4, 9.25, 20.0, 9.0, 19.0, 500));
}

#[test]
fn sample_column_last_falls_back_through_trailing_holes() {
    assert_eq!(sample_column_last(&[None, Some(1.0), None]), Some(1.0));
    assert_eq!(sample_column_last(&[Some(2.0)]), Some(2.0));
    assert_eq!(
        sample_column_last(&[None, None]),
        None,
        "all holes stay a gap"
    );
    assert_eq!(sample_column_last(&[]), None, "empty window has no sample");
}

#[test]
fn sample_column_extremes_spans_present_values_only() {
    assert_eq!(
        sample_column_extremes(&[None, Some(1.0), Some(5.0), None, Some(3.0)]),
        Some((1.0, 5.0))
    );
    assert_eq!(sample_column_extremes(&[Some(4.0)]), Some((4.0, 4.0)));
    assert_eq!(
        sample_column_extremes(&[None]),
        None,
        "holes never zero-fill a band"
    );
    assert_eq!(sample_column_extremes(&[]), None);
}

#[test]
fn aggregates_must_not_feed_indicators() {
    // The fidelity pin behind the native-timeframe rule: one column holding
    // closes [0.0, 10.0] aggregates to close 10.0, while the native series
    // mean is 5.0. Any statistic read off the aggregate (10.0-flavored)
    // aliases the true series value (5.0) — so indicators evaluate on
    // full-resolution bars and only their *output* is column-sampled.
    let rows = vec![
        ohlc(0, 0.0, 10.0, 0.0, 0.0, 1),
        ohlc(1, 0.0, 10.0, 0.0, 10.0, 1),
    ];
    let aggregate = aggregate_column(&rows).expect("non-empty");
    let native_mean = rows.iter().map(|row| row.close).sum::<f64>() / rows.len() as f64;
    assert_eq!(aggregate.close.to_bits(), 10.0f64.to_bits());
    assert_eq!(native_mean.to_bits(), 5.0f64.to_bits());
    assert_ne!(
        aggregate.close.to_bits(),
        native_mean.to_bits(),
        "aggregate display values and native statistics disagree by construction"
    );
    // ...while sampling the native output preserves it exactly.
    let native_sampled = sample_column_last(&[Some(5.0)]);
    assert_eq!(native_sampled, Some(5.0));
}

fn month_rows() -> Vec<ChartRow> {
    (0..20_000i32)
        .map(|i| {
            let close = 100.0 + f64::from(i) * 0.01;
            ChartRow {
                time: 1_700_000_000 + i64::from(i),
                open: close - 0.05,
                high: close + 0.10,
                low: close - 0.10,
                close,
                volume: 1_000 + u64::try_from(i % 100).expect("non-negative remainder"),
            }
        })
        .collect()
}

fn month_viewport() -> Viewport {
    Viewport::new(
        protopyne_chart_scale::PriceScaleMode::Normal,
        90.0,
        310.0,
        400.0,
        8.0,
        90.0,
        false,
    )
    .expect("month viewport builds")
}

#[test]
fn month_geometry_covers_every_bar_without_dropping() {
    let rows = month_rows();
    let pane = month_viewport();
    let trend = vec![true; rows.len()];
    let geoms = candle_geoms(&rows, &trend, &pane).expect("month slice paints");
    assert_eq!(
        geoms.len(),
        rows.len(),
        "geometry conserves every converted row"
    );
    for (index, geom) in geoms.iter().enumerate() {
        assert_eq!(geom.index, index);
        assert_eq!(geom.x.to_bits(), (index as f32 * 8.0).to_bits());
        assert!(geom.up, "fixture opens sit below closes");
        assert!(
            geom.high_y <= geom.close_y && geom.close_y <= geom.open_y && geom.open_y <= geom.low_y,
            "wick spans high to low through close then open at {index}: {geom:?}"
        );
        assert!(geom.trend_bullish);
    }
    let first = &geoms[0];
    let middle = &geoms[10_000];
    let last = &geoms[19_999];
    for (label, geom, open, high, low, close) in [
        (
            "first", first, 381.909f32, 381.636f32, 382.000f32, 381.818f32,
        ),
        (
            "middle", middle, 200.091f32, 199.818f32, 200.182f32, 200.000f32,
        ),
        ("last", last, 18.291f32, 18.018f32, 18.382f32, 18.200f32),
    ] {
        for (leg, actual, expected) in [
            ("open", geom.open_y, open),
            ("high", geom.high_y, high),
            ("low", geom.low_y, low),
            ("close", geom.close_y, close),
        ] {
            assert!(
                (actual - expected).abs() <= 0.002,
                "{label} {leg} drifts: {actual} vs pinned {expected}"
            );
        }
        let _ = label;
    }
    eprintln!(
        "month geometry baseline: bars={} first=({:.3},{:.3},{:.3},{:.3}) middle=({:.3},{:.3},{:.3},{:.3}) last=({:.3},{:.3},{:.3},{:.3})",
        geoms.len(),
        first.open_y,
        first.high_y,
        first.low_y,
        first.close_y,
        middle.open_y,
        middle.high_y,
        middle.low_y,
        middle.close_y,
        last.open_y,
        last.high_y,
        last.low_y,
        last.close_y,
    );
    let checksum: f64 = geoms
        .iter()
        .map(|geom| f64::from(geom.open_y) + f64::from(geom.close_y))
        .sum();
    eprintln!("month geometry checksum(open+close)={checksum:.6}");
    assert!(
        checksum.is_finite(),
        "checksum stays finite over the month slice"
    );
}

#[test]
fn month_shape_count_matches_bar_count() {
    let rows = month_rows();
    let pane = month_viewport();
    let trend = vec![true; rows.len()];
    let geoms = candle_geoms(&rows, &trend, &pane).expect("month slice paints");
    assert_eq!(
        geoms.len(),
        20_000,
        "full slice materializes one shape per bar"
    );
    eprintln!(
        "month shape baseline: rows={} geoms={}",
        rows.len(),
        geoms.len()
    );
}

#[test]
fn culled_geometry_matches_the_window_slice_of_full_geometry() {
    let rows = month_rows();
    let pane = month_viewport();
    let trend = vec![true; rows.len()];
    let full = candle_geoms(&rows, &trend, &pane).expect("month slice paints");
    // 800 px at 8 px spacing: visible (0, 101), culled (0, 105).
    let width = 800.0f32;
    assert_eq!(pane.culled_bar_range(width, rows.len()), (0, 105));
    let culled = culled_candle_geoms(&rows, &trend, &pane, width).expect("culled paints");
    assert_eq!(
        culled.len(),
        105,
        "one shape per bar in the window, nothing outside it"
    );
    assert!(
        culled.len() < full.len(),
        "culling sheds the off-window bars"
    );
    for (offset, geom) in culled.iter().enumerate() {
        assert_eq!(
            *geom, full[offset],
            "culled bar {offset} carries its global index and pixels"
        );
    }
    // Panned ten bars in: the same slice equality holds off-origin, read
    // against full geometry under the identical shifted viewport (x pixels
    // legitimately move with the pan; legs, indices, and keys do not).
    let shifted = pane.with_x_offset(-80.0).expect("finite pan");
    assert_eq!(shifted.culled_bar_range(width, rows.len()), (6, 115));
    let full_shifted = candle_geoms(&rows, &trend, &shifted).expect("shifted slice paints");
    let moved = culled_candle_geoms(&rows, &trend, &shifted, width).expect("panned paints");
    assert_eq!(moved.len(), 115 - 6);
    for (offset, geom) in moved.iter().enumerate() {
        assert_eq!(geom.index, 6 + offset, "indices stay global under pan");
        assert_eq!(
            geom.x.to_bits(),
            shifted.index_to_x(6 + offset).to_bits(),
            "x tracks the global slot, never the window position"
        );
        assert_eq!(
            *geom,
            full_shifted[6 + offset],
            "panned culled bar matches the full slice"
        );
    }
}

#[test]
fn panning_one_bar_keeps_mid_pane_candles_stable() {
    let rows = month_rows();
    let pane = month_viewport();
    let trend = vec![true; rows.len()];
    let width = 800.0f32;
    // One full bar of pan in single-pixel steps at 8 px spacing.
    let mut frames: Vec<Vec<usize>> = Vec::new();
    for step in 0..=8 {
        let shifted = pane.with_x_offset(-(step as f32)).expect("finite pan");
        let geoms = culled_candle_geoms(&rows, &trend, &shifted, width).expect("pan frame paints");
        let indices: Vec<usize> = geoms.iter().map(|geom| geom.index).collect();
        assert!(!indices.is_empty(), "step {step} still sees bars");
        for pair in indices.windows(2) {
            assert_eq!(
                pair[1],
                pair[0] + 1,
                "culled window stays contiguous at step {step}"
            );
        }
        for geom in &geoms {
            assert_eq!(
                geom.x.to_bits(),
                shifted.index_to_x(geom.index).to_bits(),
                "x tracks the global slot at step {step}"
            );
        }
        frames.push(indices);
    }
    // Bars far from both edges never flicker across the whole sweep.
    for index in 30..80 {
        for (step, frame) in frames.iter().enumerate() {
            assert!(
                frame.contains(&index),
                "mid-pane bar {index} survives pan step {step}"
            );
        }
    }
    // Only the window edges may move, and by at most one bar per pixel step.
    for (step, pair) in frames.windows(2).enumerate() {
        let (first, second) = (&pair[0], &pair[1]);
        assert!(
            (first[0] as isize - second[0] as isize).abs() <= 1,
            "leading edge creeps at step {step}"
        );
        let last_first = *first.last().expect("non-empty frame");
        let last_second = *second.last().expect("non-empty frame");
        assert!(
            (last_first as isize - last_second as isize).abs() <= 1,
            "trailing edge creeps at step {step}"
        );
    }
}

#[test]
fn culled_geometry_is_empty_off_window_and_refuses_mismatched_keys() {
    let rows = month_rows();
    let pane = month_viewport();
    let trend = vec![true; rows.len()];
    assert!(
        culled_candle_geoms(&rows, &trend, &pane, 0.0)
            .expect("zero width paints")
            .is_empty(),
        "zero width paints nothing"
    );
    assert!(
        culled_candle_geoms(&rows, &trend, &pane, -10.0)
            .expect("negative width paints")
            .is_empty(),
        "negative width paints nothing"
    );
    assert!(
        culled_candle_geoms(&rows[..0], &trend[..0], &pane, 800.0)
            .expect("no rows paints")
            .is_empty(),
        "no rows paints nothing"
    );
    let past_left = pane.with_x_offset(200_000.0).expect("finite pan");
    assert!(
        culled_candle_geoms(&rows, &trend, &past_left, 800.0)
            .expect("off-window paints")
            .is_empty(),
        "bars shoved past the pane paint nothing"
    );
    let past_right = pane.with_x_offset(-200_000.0).expect("finite pan");
    assert!(
        culled_candle_geoms(&rows, &trend, &past_right, 800.0)
            .expect("off-window paints")
            .is_empty(),
        "bars shoved before the pane paint nothing"
    );
    assert!(
        culled_candle_geoms(&rows[..2], &trend[..1], &pane, 800.0).is_err(),
        "keys that do not line up with rows refuse"
    );
}

fn sub_pixel_pane(spacing: f32) -> Viewport {
    Viewport::new(
        protopyne_chart_scale::PriceScaleMode::Normal,
        90.0,
        310.0,
        400.0,
        spacing,
        90.0,
        false,
    )
    .expect("sub-pixel pane builds")
}

#[test]
fn column_dispatch_switches_strictly_below_one_pixel_per_bar() {
    assert!(!needs_column_aggregation(8.0), "ordinary zoom stays native");
    assert!(
        !needs_column_aggregation(1.0),
        "exactly one pixel per bar stays native"
    );
    assert!(
        needs_column_aggregation(0.999),
        "just below one pixel per bar aggregates"
    );
    assert!(
        !needs_column_aggregation(f32::NAN),
        "non-finite spacing stays native"
    );
    assert!(
        !needs_column_aggregation(f32::INFINITY),
        "infinite spacing stays native"
    );
    // Path-level: at exactly 1.0 the dispatcher is the native path untouched.
    let rows = vec![row(100.0, 10), row(101.0, 20), row(99.0, 30)];
    let trend = vec![true, false, true];
    let native_pane = sub_pixel_pane(1.0);
    let dispatched =
        column_candle_geoms(&rows, &trend, &native_pane, 80.0).expect("boundary paints");
    let full = candle_geoms(&rows, &trend, &native_pane).expect("native paints");
    assert_eq!(
        dispatched, full,
        "the 1.0 boundary draws every native candle"
    );
    // Just below: bars sharing a column fold to one candle.
    let tight_pane = sub_pixel_pane(0.5);
    let folded = column_candle_geoms(&rows, &trend, &tight_pane, 80.0).expect("sub-pixel paints");
    // Floored x pixels over global slots 0..3 at 0.5 px: columns 0, 0, 1.
    assert_eq!(
        folded.len(),
        2,
        "two columns paint two candles, never three"
    );
    assert_eq!(
        (folded[0].index, folded[1].index),
        (0, 2),
        "indices stay global"
    );
    assert_eq!(
        (folded[0].x.to_bits(), folded[1].x.to_bits()),
        (0.0f32.to_bits(), 1.0f32.to_bits())
    );
    // A single-bar column round-trips its native legs exactly.
    let lone = &candle_geoms(&rows[2..3], &trend[2..3], &tight_pane).expect("lone paints")[0];
    for (leg, folded_leg, lone_leg) in [
        ("open", folded[1].open_y, lone.open_y),
        ("high", folded[1].high_y, lone.high_y),
        ("low", folded[1].low_y, lone.low_y),
        ("close", folded[1].close_y, lone.close_y),
    ] {
        assert_eq!(
            folded_leg.to_bits(),
            lone_leg.to_bits(),
            "single-bar column keeps its {leg}"
        );
    }
    assert_eq!(
        folded[1].trend_bullish, trend[2],
        "a column reads its most recent key"
    );
}

#[test]
fn aggregated_columns_reproduce_full_resolution_pixels() {
    let rows = month_rows();
    let pane = sub_pixel_pane(0.2);
    let trend = vec![true; rows.len()];
    let width = 800.0f32;
    let (start, end) = pane.culled_bar_range(width, rows.len());
    let full = candle_geoms(&rows, &trend, &pane).expect("month slice paints");
    assert_eq!(full.len(), rows.len(), "the pixel truth covers every bar");
    let dispatched = column_candle_geoms(&rows, &trend, &pane, width).expect("dispatched paints");
    // Shape count: one candle per painted column, a fraction of the bars.
    let runs = column_runs(end - start, |slot| {
        pane.slot_to_x((start + slot) as f64).floor() as i64
    });
    assert_eq!(dispatched.len(), runs.len(), "one shape per painted column");
    assert!(
        dispatched.len() * 20 < rows.len(),
        "the month view emits O(pixels) shapes: {} vs {} bars",
        dispatched.len(),
        rows.len()
    );
    // Pixel-exactness: every aggregate leg is the fold of its members' legs.
    for (geom, (_, range)) in dispatched.iter().zip(runs.iter()) {
        let members = &rows[start + range.start..start + range.end];
        let first = &members[0];
        let last = &members[members.len() - 1];
        assert_eq!(
            geom.index,
            start + range.start,
            "aggregates carry global indices"
        );
        assert_eq!(
            geom.up,
            last.close >= first.open,
            "direction follows first open, last close"
        );
        assert_eq!(geom.trend_bullish, trend[start + range.end - 1]);
        let y = |price: f64| pane.price_to_y(price).expect("fixture maps");
        assert_eq!(geom.open_y.to_bits(), y(first.open).to_bits());
        assert_eq!(geom.close_y.to_bits(), y(last.close).to_bits());
        let top = members
            .iter()
            .map(|bar| y(bar.high))
            .fold(f32::INFINITY, f32::min);
        let bottom = members
            .iter()
            .map(|bar| y(bar.low))
            .fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(
            geom.high_y.to_bits(),
            top.to_bits(),
            "the wick tip is the highest member"
        );
        assert_eq!(
            geom.low_y.to_bits(),
            bottom.to_bits(),
            "the wick foot is the lowest member"
        );
        // Same pixels the members overdraw: one shared screen column.
        for (member, member_geom) in full
            .iter()
            .enumerate()
            .take(start + range.end)
            .skip(start + range.start)
        {
            assert_eq!(
                (member_geom.x.floor() as i64),
                (geom.x.floor() as i64),
                "member {member} overdraws the aggregate column"
            );
        }
    }
}

#[test]
fn aggregated_month_view_keeps_the_shape_budget_small() {
    // The recorded month-view shape budget: 20k bars at 0.2 px spacing over
    // an 800 px pane must stay at O(pixels) shapes, never O(bars).
    let rows = month_rows();
    let pane = sub_pixel_pane(0.2);
    let trend = vec![true; rows.len()];
    let dispatched = column_candle_geoms(&rows, &trend, &pane, 800.0).expect("dispatched paints");
    eprintln!(
        "dispatched shape budget: rows={} geoms={}",
        rows.len(),
        dispatched.len()
    );
    assert!(
        dispatched.len() <= 810,
        "columns plus margin overscan stay small"
    );
}

fn sampled_fixture() -> (Vec<Option<f64>>, Viewport) {
    // Twenty bars with holes, over a pane whose 0.5 px spacing folds pairs
    // into shared columns: values rise 100.0 by ones; bars 2..3 share a
    // column that is holed throughout, bar 4 holes part of its column, and
    // bar 17 is a trailing hole falling back to bar 16 in its column.
    let values: Vec<Option<f64>> = (0..20)
        .map(|i| match i {
            2 | 3 | 4 | 17 => None,
            _ => Some(100.0 + f64::from(i)),
        })
        .collect();
    let pane = Viewport::new(
        protopyne_chart_scale::PriceScaleMode::Normal,
        90.0,
        130.0,
        400.0,
        0.5,
        90.0,
        false,
    )
    .expect("sampling pane builds");
    (values, pane)
}

#[test]
fn sampled_line_vertices_equal_sampling_the_native_series() {
    // Display-only sampling: the sampled polyline carries exactly what
    // sampling the native series per column run yields — last present value
    // per run at its screen column, holes staying holes.
    let (values, pane) = sampled_fixture();
    let vertices = sampled_line_points(&values, &pane, 0);
    let runs = column_runs(values.len(), |slot| {
        pane.slot_to_x(slot as f64).floor() as i64
    });
    assert_eq!(vertices.len(), runs.len(), "one vertex per painted column");
    for (vertex, (column, range)) in vertices.iter().zip(runs.iter()) {
        let sample = sample_column_last(&values[range.clone()]);
        let expected = sample.and_then(|price| pane.windowed_y(price).map(|y| (*column as f32, y)));
        assert_eq!(*vertex, expected, "column {column} samples its native run");
    }
    // The holed column (bars 2..3 share it) lifts the pen; the trailing hole
    // at 17 falls back to bar 16's value in its column.
    let holed = runs
        .iter()
        .position(|(_, range)| range.start == 2)
        .expect("paired runs");
    assert_eq!(vertices[holed], None, "an all-hole column stays a gap");
    assert!(
        vertices.iter().any(Option::is_some),
        "present values still paint"
    );
}

#[test]
fn sampled_line_vertices_match_native_mapping_at_full_resolution() {
    // At one bar per column the sampler is the identity: singleton runs
    // carry each bar's own value at its own pixel.
    let (values, _) = sampled_fixture();
    let pane = month_viewport();
    let vertices = sampled_line_points(&values, &pane, 0);
    let native = oscillator_points(&values, &pane);
    assert_eq!(vertices.len(), native.len());
    for (sampled, plain) in vertices.iter().zip(native.iter()) {
        assert_eq!(sampled, plain, "singleton runs sample to themselves");
    }
}

#[test]
fn sampled_band_spans_equal_spanning_the_native_series() {
    // Band-like outputs span the present values per column run through the
    // extremes sampler: top reads the run high, bottom the run low, holes
    // stay holes at every stage.
    let (values, pane) = sampled_fixture();
    let spans = sampled_band_spans(&values, &pane, 0);
    let runs = column_runs(values.len(), |slot| {
        pane.slot_to_x(slot as f64).floor() as i64
    });
    assert_eq!(spans.len(), runs.len(), "one span per painted column");
    for (span, (column, range)) in spans.iter().zip(runs.iter()) {
        let expected =
            sample_column_extremes(&values[range.clone()]).and_then(|(low, high)| {
                match (pane.windowed_y(high), pane.windowed_y(low)) {
                    (Some(top), Some(bottom)) => Some((*column as f32, top, bottom)),
                    _ => None,
                }
            });
        assert_eq!(*span, expected, "column {column} spans its native run");
    }
    let holed = runs
        .iter()
        .position(|(_, range)| range.start == 2)
        .expect("paired runs");
    assert_eq!(spans[holed], None, "an all-hole column spans nothing");
}

#[test]
fn samplers_stay_total_over_empty_and_far_inputs() {
    let (values, pane) = sampled_fixture();
    assert!(
        sampled_line_points(&[], &pane, 0).is_empty(),
        "no values paint no vertices"
    );
    assert!(
        sampled_band_spans(&[], &pane, 0).is_empty(),
        "no values span nothing"
    );
    // A base far past the data still maps deterministically: columns shift,
    // the sampler never panics, and every run still yields its vertex.
    let shifted = sampled_line_points(&values, &pane, 1_000_000);
    let runs = column_runs(values.len(), |slot| {
        pane.slot_to_x((1_000_000 + slot) as f64).floor() as i64
    });
    assert_eq!(
        shifted.len(),
        runs.len(),
        "a far base still yields one vertex per run"
    );
}

fn demo_zoom_pane() -> Viewport {
    sub_pixel_pane(0.2)
}

fn demo_zero_pane() -> Viewport {
    Viewport::new(
        PriceScaleMode::Normal,
        -1_300.0,
        1_300.0,
        200.0,
        0.2,
        0.0,
        false,
    )
    .expect("demo zero pane builds")
}

#[test]
fn frame_shape_budget_stays_pixel_bound_at_demo_zoom() {
    // The frame-path budget: a 20k-bar month at 0.2 px over an 800 px pane
    // must emit O(pixels) shapes across every layer — candles, ribbon bands,
    // delta bars, and line vertices — never O(bars).
    let rows = month_rows();
    let pane = demo_zoom_pane();
    let zero = demo_zero_pane();
    let width = 800.0f32;
    let trend = vec![true; rows.len()];
    let candles = column_candle_geoms(&rows, &trend, &pane, width).expect("candles dispatch");
    let upper: Vec<Option<f64>> = rows.iter().map(|row| Some(row.high)).collect();
    let lower: Vec<Option<f64>> = rows.iter().map(|row| Some(row.low)).collect();
    let (bands, _) = sampled_ribbon_bands(&upper, &lower, &trend, &pane, width);
    let ribbon_shapes = bands.iter().flatten().count();
    let buy: Vec<Option<f64>> = rows.iter().map(|row| Some(row.volume as f64)).collect();
    let sell: Vec<Option<f64>> = rows
        .iter()
        .map(|row| Some(row.volume as f64 * 0.5))
        .collect();
    let delta = column_delta_geoms(&buy, &sell, &zero, width).expect("delta dispatches");
    let closes: Vec<Option<f64>> = rows.iter().map(|row| Some(row.close)).collect();
    let line = sampled_line_points(&closes, &pane, 0);
    let line_shapes = line.iter().flatten().count();
    let total = candles.len() + ribbon_shapes + delta.len() + line_shapes;
    eprintln!(
        "frame shape budget: candles={} ribbon={} delta={} line={} total={}",
        candles.len(),
        ribbon_shapes,
        delta.len(),
        line_shapes,
        total
    );
    assert!(
        candles.len() <= 810,
        "candles stay O(pixels): {}",
        candles.len()
    );
    assert!(
        ribbon_shapes <= 810,
        "ribbon stays O(pixels): {ribbon_shapes}"
    );
    assert!(delta.len() <= 810, "delta stays O(pixels): {}", delta.len());
    assert!(
        line_shapes <= 4_005,
        "line samples one vertex per column: {line_shapes}"
    );
    assert!(
        total <= 4 * 810 + 4_005,
        "the frame stays O(pixels): {total}"
    );
    // Negative control: the native full-slice builders materialize O(bars).
    let full = candle_geoms(&rows, &trend, &pane).expect("full slice paints");
    assert_eq!(full.len(), rows.len(), "native geometry is O(bars)");
    assert!(full.len() > total, "sampling sheds the overdraw");
}

#[test]
fn sampled_ribbon_bands_span_their_native_runs() {
    // Pixel-exactness: every sampled band is the union span of its column
    // run's members (top reads the run's highest upper, bottom its lowest
    // lower) at the run's screen column with the run's most recent trend.
    let rows = month_rows();
    let pane = demo_zoom_pane();
    let width = 800.0f32;
    let trend = vec![true; rows.len()];
    let upper: Vec<Option<f64>> = rows.iter().map(|row| Some(row.high)).collect();
    let lower: Vec<Option<f64>> = rows.iter().map(|row| Some(row.low)).collect();
    let (bands, trends) = sampled_ribbon_bands(&upper, &lower, &trend, &pane, width);
    let (start, end) = pane.culled_bar_range(width, rows.len());
    let runs = column_runs(end - start, |slot| {
        pane.slot_to_x((start + slot) as f64).floor() as i64
    });
    assert_eq!(bands.len(), runs.len(), "one band per painted column");
    assert_eq!(trends.len(), runs.len());
    for ((band, column_trend), (column, range)) in bands.iter().zip(trends.iter()).zip(runs.iter())
    {
        let up_run = &upper[start + range.start..start + range.end];
        let lo_run = &lower[start + range.start..start + range.end];
        let expected_top = sample_column_extremes(up_run).map(|(_, high)| high);
        let expected_bottom = sample_column_extremes(lo_run).map(|(low, _)| low);
        match (band, expected_top, expected_bottom) {
            (Some(strip), Some(top), Some(bottom)) => {
                assert_eq!(strip.x.to_bits(), (*column as f32).to_bits());
                assert_eq!(
                    strip.top_y.to_bits(),
                    pane.windowed_y(top).expect("fixture maps").to_bits()
                );
                assert_eq!(
                    strip.bottom_y.to_bits(),
                    pane.windowed_y(bottom).expect("fixture maps").to_bits()
                );
                assert_eq!(*column_trend, trend[start + range.end - 1]);
                assert_eq!(strip.index, start + range.start);
            }
            (None, _, _) => {
                assert!(
                    expected_top.is_none() || expected_bottom.is_none(),
                    "a holed column spans nothing"
                );
            }
            (Some(_), _, _) => panic!("a present column never spans from holes"),
        }
    }
    // Holes stay holes: an all-hole leg spans nothing at any zoom.
    let holed_upper = vec![None; 64];
    let holed_lower = vec![None; 64];
    let holed_trend = vec![false; 64];
    let (holed, _) = sampled_ribbon_bands(&holed_upper, &holed_lower, &holed_trend, &pane, width);
    assert!(
        holed.iter().all(Option::is_none),
        "all-hole legs span nothing"
    );
}

#[test]
fn column_delta_bars_fold_their_native_runs() {
    // Pixel-exactness: an aggregated delta bar is the tallest leg of its
    // column run per side (buy reads the run max, sell the run max depth) at
    // the run's screen column; native geometry above one pixel per bar is
    // the culled slice untouched.
    let pane = demo_zoom_pane();
    let zero = demo_zero_pane();
    let width = 800.0f32;
    let buy: Vec<Option<f64>> = (0..2_000)
        .map(|i| Some(400.0 + f64::from(i % 50)))
        .collect();
    let sell: Vec<Option<f64>> = (0..2_000)
        .map(|i| Some(200.0 + f64::from(i % 30)))
        .collect();
    let bars = column_delta_geoms(&buy, &sell, &zero, width).expect("delta dispatches");
    let (start, end) = zero.culled_bar_range(width, buy.len());
    let runs = column_runs(end - start, |slot| {
        zero.slot_to_x((start + slot) as f64).floor() as i64
    });
    assert_eq!(bars.len(), runs.len(), "one bar per painted column");
    for (bar, (column, range)) in bars.iter().zip(runs.iter()) {
        let buy_run = &buy[start + range.start..start + range.end];
        let sell_run = &sell[start + range.start..start + range.end];
        let expected_buy = sample_column_extremes(buy_run)
            .map(|(_, high)| high)
            .and_then(|top| zero.price_to_y(top));
        let expected_sell = sample_column_extremes(sell_run)
            .map(|(_, high)| high)
            .and_then(|depth| zero.price_to_y(-depth));
        assert_eq!(bar.buy_y, expected_buy, "buy reads the run max");
        assert_eq!(bar.sell_y, expected_sell, "sell reads the run max depth");
        assert_eq!(bar.x.to_bits(), (*column as f32).to_bits());
        assert_eq!(bar.index, start + range.start);
    }
    // Native path: above one pixel per bar the dispatcher is the culled
    // slice untouched (over a zero-spanning pane at native spacing).
    let native_pane = Viewport::new(
        PriceScaleMode::Normal,
        -1_300.0,
        1_300.0,
        200.0,
        8.0,
        0.0,
        false,
    )
    .expect("native zero pane builds");
    let native = column_delta_geoms(&buy, &sell, &native_pane, width).expect("native paints");
    let (nstart, nend) = native_pane.culled_bar_range(width, buy.len());
    let sliced = delta_bar_geoms_sliced(
        &buy[nstart..nend],
        &sell[nstart..nend],
        &native_pane,
        nstart,
    )
    .expect("culled slice paints");
    assert_eq!(native, sliced, "native dispatch is the culled slice");
    // Holes stay holes per leg, never zero-filled.
    let holed_buy = vec![None, Some(400.0), None];
    let holed_sell = vec![Some(200.0), None, None];
    let holed = delta_bar_geoms_sliced(&holed_buy, &holed_sell, &zero, 0).expect("holes map");
    assert_eq!(holed.len(), 3);
    assert_eq!(holed[0].buy_y, None, "an all-hole buy leg lifts");
    assert!(holed[0].sell_y.is_some());
    assert!(holed[1].buy_y.is_some());
    assert_eq!(holed[1].sell_y, None);
    let _ = pane;
}

#[test]
fn sampled_close_lines_carry_their_native_runs_last_close() {
    // The Line/Area price picture samples the close series per column run:
    // closes never hole, so every sampled vertex is its run's last close at
    // the run's screen column — the same pixels fewer shapes.
    let rows = month_rows();
    let pane = demo_zoom_pane();
    let closes: Vec<Option<f64>> = rows.iter().map(|row| Some(row.close)).collect();
    let vertices = sampled_line_points(&closes, &pane, 0);
    let runs = column_runs(closes.len(), |slot| {
        pane.slot_to_x(slot as f64).floor() as i64
    });
    assert_eq!(vertices.len(), runs.len(), "one vertex per painted column");
    assert!(vertices.len() < closes.len(), "sub-pixel zoom sheds shapes");
    for (vertex, (column, range)) in vertices.iter().zip(runs.iter()) {
        let last = closes[range.end - 1].expect("closes never hole");
        let expected = pane.windowed_y(last).map(|y| (*column as f32, y));
        assert_eq!(*vertex, expected, "column {column} carries its last close");
    }
}

#[test]
fn sliced_ribbon_polygons_match_the_owned_entry() {
    // No second polygon code: the borrowed entry closes the same runs and
    // points as the owned one over identical legs.
    let pane = month_viewport();
    let ribbon = ribbon_view(
        &[Some(102.0), Some(103.0), Some(98.0), None, Some(101.0)],
        &[
            Some(100.0),
            Some(101.0),
            Some(100.0),
            Some(100.0),
            Some(100.0),
        ],
        0.5,
    )
    .expect("valid ribbon");
    let owned = ribbon_polygons(&ribbon, &pane);
    let sliced = ribbon_polygons_sliced(&ribbon.upper, &ribbon.lower, &ribbon.bullish, &pane);
    assert_eq!(sliced, owned, "the borrowed entry closes identical runs");
}

#[test]
fn native_overlay_shapes_stay_within_twice_the_visible_count() {
    // At native zoom the full-window mappers materialize O(bars) shapes while
    // the twice-visible native builders stay within twice the visible bar
    // count: 20k bars at 8 px over 800 px shows 101 bars, so twice-visible
    // spans at most 202.
    let rows = month_rows();
    let pane = month_viewport();
    let width = 800.0f32;
    let (visible_start, visible_end) = pane.visible_bar_range(width, rows.len());
    let visible = visible_end - visible_start;
    assert_eq!(visible, 101, "the fixture really shows 101 bars");
    let closes: Vec<Option<f64>> = rows.iter().map(|row| Some(row.close)).collect();
    let full = oscillator_points(&closes, &pane);
    assert_eq!(full.len(), rows.len(), "the native mapper covers every bar");
    let culled = proportional_line_points(&closes, &pane, width);
    assert!(
        culled.len() <= 2 * visible + 1,
        "twice-visible line points stay bounded: {} vs visible {visible}",
        culled.len()
    );
    assert!(
        culled.len() < full.len(),
        "culling sheds the off-window bars"
    );
    let upper: Vec<Option<f64>> = rows.iter().map(|row| Some(row.high)).collect();
    let lower: Vec<Option<f64>> = rows.iter().map(|row| Some(row.low)).collect();
    let trend = vec![true; rows.len()];
    let full_polygons = ribbon_polygons_sliced(&upper, &lower, &trend, &pane);
    let culled_polygons = proportional_ribbon_polygons(&upper, &lower, &trend, &pane, width);
    let culled_bars: usize = culled_polygons
        .iter()
        .map(|polygon| polygon.indices.len())
        .sum();
    assert!(
        culled_bars <= 2 * visible + 1,
        "twice-visible ribbon bars stay bounded: {culled_bars} vs visible {visible}"
    );
    let full_bars: usize = full_polygons
        .iter()
        .map(|polygon| polygon.indices.len())
        .sum();
    assert!(
        culled_bars < full_bars,
        "culling sheds the off-window ribbon bars"
    );
}

#[test]
fn twice_visible_native_output_matches_the_full_window_slice() {
    // Culling-equivalence: twice-visible native output, re-offset, equals the
    // corresponding slice of full-window output — same pixels, fewer shapes.
    let rows = month_rows();
    let pane = month_viewport();
    let width = 800.0f32;
    let closes: Vec<Option<f64>> = rows.iter().map(|row| Some(row.close)).collect();
    let full = oscillator_points(&closes, &pane);
    let culled = proportional_line_points(&closes, &pane, width);
    let (start, end) = pane.proportional_bar_range(width, closes.len());
    assert_eq!(culled.len(), end - start, "one point per twice-visible bar");
    for (offset, point) in culled.iter().enumerate() {
        assert_eq!(
            *point,
            full[start + offset],
            "bar {} keeps its pixels",
            start + offset
        );
    }
    // Panned ten bars in: the same slice equality holds off-origin.
    let shifted = pane.with_x_offset(-80.0).expect("finite pan");
    let full_shifted = oscillator_points(&closes, &shifted);
    let moved = proportional_line_points(&closes, &shifted, width);
    let (moved_start, moved_end) = shifted.proportional_bar_range(width, closes.len());
    assert_eq!(moved.len(), moved_end - moved_start);
    for (offset, point) in moved.iter().enumerate() {
        assert_eq!(
            *point,
            full_shifted[moved_start + offset],
            "panned bar {} keeps its pixels",
            moved_start + offset
        );
    }
    // Ribbon polygons close identical runs over the twice-visible window.
    let upper: Vec<Option<f64>> = rows.iter().map(|row| Some(row.high)).collect();
    let lower: Vec<Option<f64>> = rows.iter().map(|row| Some(row.low)).collect();
    let trend = vec![true; rows.len()];
    let full_polygons = ribbon_polygons_sliced(&upper, &lower, &trend, &pane);
    let culled_polygons = proportional_ribbon_polygons(&upper, &lower, &trend, &pane, width);
    let mut full_points = std::collections::HashMap::new();
    for polygon in &full_polygons {
        for (index, point) in polygon.indices.iter().zip(polygon.points.iter()) {
            full_points.insert(*index, *point);
        }
    }
    for polygon in &culled_polygons {
        for (index, point) in polygon.indices.iter().zip(polygon.points.iter()) {
            assert_eq!(
                full_points.get(index),
                Some(point),
                "ribbon bar {index} keeps its pixels"
            );
        }
    }
    assert!(
        !culled_polygons.is_empty(),
        "the visible window still fills"
    );
}

#[test]
fn twice_visible_native_output_is_empty_off_window() {
    let rows = month_rows();
    let pane = month_viewport();
    let closes: Vec<Option<f64>> = rows.iter().map(|row| Some(row.close)).collect();
    let upper: Vec<Option<f64>> = rows.iter().map(|row| Some(row.high)).collect();
    let lower: Vec<Option<f64>> = rows.iter().map(|row| Some(row.low)).collect();
    let trend = vec![true; rows.len()];
    assert!(
        proportional_line_points(&closes, &pane, 0.0).is_empty(),
        "zero width paints nothing"
    );
    assert!(
        proportional_ribbon_polygons(&upper, &lower, &trend, &pane, 0.0).is_empty(),
        "zero width fills nothing"
    );
    assert!(
        proportional_line_points(&[], &pane, 800.0).is_empty(),
        "no values paint no vertices"
    );
    let past_left = pane.with_x_offset(200_000.0).expect("finite pan");
    assert!(
        proportional_line_points(&closes, &past_left, 800.0).is_empty(),
        "bars shoved past the pane paint nothing"
    );
    assert!(
        proportional_ribbon_polygons(&upper, &lower, &trend, &past_left, 800.0).is_empty(),
        "bars shoved past the pane fill nothing"
    );
    let past_right = pane.with_x_offset(-200_000.0).expect("finite pan");
    assert!(
        proportional_line_points(&closes, &past_right, 800.0).is_empty(),
        "bars shoved before the pane paint nothing"
    );
}
