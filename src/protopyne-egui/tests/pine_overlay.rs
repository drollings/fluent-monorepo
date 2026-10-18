//! Overlay geometry goldens: `PlotPrimitive` values become pane-local pixels
//! through the shared viewport — no second bar-to-pixel math lives here.
//!
//! Boundary pixels pin the mapping (range top paints `y = 0`, range bottom
//! paints `y = height`); holes and unmappable prices stay holes, so the
//! overlay can never invent a point.

use super::*;
use fluent_egui::Viewport;
use protopyne_chart_scale::PriceScaleMode;
use protopyne_kernels::PlotPrimitive;

fn viewport() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 200.0, 8.0, 90.0, false)
        .expect("test viewport builds")
}

#[test]
fn series_points_map_through_the_viewport() {
    // Prices hit the range edges exactly; the hole stays a hole.
    let geoms = overlay_geoms(
        &[PlotPrimitive::Series {
            title: "S".to_owned(),
            values: vec![Some(110.0), None, Some(90.0)],
        }],
        &viewport(),
    );
    assert_eq!(geoms.series.len(), 1);
    let points = &geoms.series[0].points;
    assert_eq!(points.len(), 3);
    assert_eq!(
        points[0].map(|(_, y)| y),
        Some(0.0),
        "range top paints y = 0"
    );
    assert_eq!(points[1], None, "holes stay holes");
    assert_eq!(
        points[2].map(|(_, y)| y),
        Some(200.0),
        "range bottom paints y = height"
    );
    // Bar slots start at bar zero on the 8 px spacing: literals on the
    // documented contract, never a second mapping call.
    assert_eq!(points[0].map(|(x, _)| x), Some(0.0));
    assert_eq!(points[2].map(|(x, _)| x), Some(16.0));
}

#[test]
fn levels_markers_labels_and_areas_place() {
    // Every primitive kind lands on the viewport mapping; text rides along.
    // The window is 90..110 over 200 px on the 8 px bar spacing, so prices
    // map as y = (110 - price) * 10 and bars as x = 8 * bar — hand-derived
    // literals, never a second mapping call.
    let pane = viewport();
    let geoms = overlay_geoms(
        &[
            PlotPrimitive::Level {
                title: "L".to_owned(),
                price: 100.0,
            },
            PlotPrimitive::Marker {
                title: "M".to_owned(),
                bar: 1,
                price: 105.0,
            },
            PlotPrimitive::Label {
                bar: 2,
                price: 95.0,
                text: "hi".to_owned(),
            },
            PlotPrimitive::Area {
                left: 0,
                top: 108.0,
                right: 3,
                bottom: 92.0,
            },
        ],
        &pane,
    );
    assert_eq!(geoms.levels, vec![100.0]);
    assert_eq!(geoms.markers, vec![(8.0, 50.0)]);
    assert_eq!(geoms.labels.len(), 1);
    assert_eq!(geoms.labels[0].text, "hi");
    assert_eq!((geoms.labels[0].x, geoms.labels[0].y), (16.0, 150.0));
    assert_eq!(geoms.areas.len(), 1);
    assert_eq!(geoms.areas[0], (0.0, 20.0, 24.0, 180.0));
}

#[test]
fn unmappable_prices_stay_unpainted() {
    // Non-finite prices map to nothing: the overlay skips them instead of
    // substituting a pixel.
    let geoms = overlay_geoms(
        &[
            PlotPrimitive::Series {
                title: "S".to_owned(),
                values: vec![Some(f64::NAN)],
            },
            PlotPrimitive::Level {
                title: "L".to_owned(),
                price: f64::INFINITY,
            },
            PlotPrimitive::Marker {
                title: "M".to_owned(),
                bar: 0,
                price: f64::NAN,
            },
            PlotPrimitive::Label {
                bar: 0,
                price: f64::NAN,
                text: "x".to_owned(),
            },
            PlotPrimitive::Area {
                left: 0,
                top: f64::NAN,
                right: 1,
                bottom: 90.0,
            },
        ],
        &viewport(),
    );
    assert_eq!(geoms.series[0].points, vec![None]);
    assert!(geoms.levels.is_empty(), "no level for an unmappable price");
    assert!(
        geoms.markers.is_empty(),
        "no marker for an unmappable price"
    );
    assert!(geoms.labels.is_empty(), "no label for an unmappable price");
    assert!(geoms.areas.is_empty(), "no area for an unmappable price");
}

#[test]
fn a_warmup_padded_series_aligns_with_its_labels() {
    // The projection pads warmup bars as leading `None` holes so every
    // primitive shares the cursor-space index: the holes stay unpainted,
    // and a label on bar 2 sits over the series slot 2 — the overlay never
    // shifts one against the other.
    let pane = viewport();
    let geoms = overlay_geoms(
        &[
            PlotPrimitive::Series {
                title: "S".to_owned(),
                values: vec![None, None, Some(100.0)],
            },
            PlotPrimitive::Label {
                bar: 2,
                price: 100.0,
                text: "w".to_owned(),
            },
        ],
        &pane,
    );
    let points = &geoms.series[0].points;
    assert_eq!(points.len(), 3);
    assert_eq!(&points[..2], &[None, None], "warmup holes stay unpainted");
    // Slot 2 on the 8 px spacing; 100.0 sits halfway down the 90..110
    // window over 200 px.
    assert_eq!(points[2], Some((16.0, 100.0)), "slot 2 maps to bar 2");
    assert_eq!(geoms.labels.len(), 1);
    assert_eq!(
        (geoms.labels[0].x, geoms.labels[0].y),
        (16.0, 100.0),
        "the label sits over its series slot"
    );
}

/// Headless painter over a test context: the overlay blits record shapes
/// into the frame instead of needing a window.
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

fn end_frame(ctx: &egui::Context) -> egui::FullOutput {
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    output
}

fn series_only_geoms() -> OverlayGeoms {
    OverlayGeoms {
        series: vec![SeriesGeom {
            points: vec![Some((0.0, 100.0)), Some((8.0, 90.0)), Some((16.0, 95.0))],
        }],
        ..OverlayGeoms::default()
    }
}

#[test]
fn unused_theme_keys_never_refuse_the_overlay() {
    // Keys resolve inside the branch that needs them: a theme whose unused
    // slot is malformed still paints every branch it can, and only a needed
    // key refuses — with the key named.
    let full = ThemeConfig::default();
    let (_, painter) = test_painter();
    paint_overlay(&painter, &series_only_geoms(), &full, 320.0).expect("healthy overlay paints");
    // Malformed "slow" (labels/areas) and "grid" (levels) with a series-only
    // projection: neither key is needed, so both paint.
    for (key, theme) in [
        (
            "slow",
            ThemeConfig::new().slow("not-a-color".to_owned()).build(),
        ),
        (
            "grid",
            ThemeConfig::new().grid("not-a-color".to_owned()).build(),
        ),
        (
            "bull",
            ThemeConfig::new().bull("not-a-color".to_owned()).build(),
        ),
    ] {
        let (_, painter) = test_painter();
        if let Err(refusal) = paint_overlay(&painter, &series_only_geoms(), &theme, 320.0) {
            panic!("an unused malformed key paints ({key}): {refusal}");
        }
    }
    // The same malformed "grid" with levels present refuses, naming "grid".
    let (_, painter) = test_painter();
    let with_levels = OverlayGeoms {
        levels: vec![100.0],
        ..series_only_geoms()
    };
    let refused = paint_overlay(
        &painter,
        &with_levels,
        &ThemeConfig::new().grid("not-a-color".to_owned()).build(),
        320.0,
    )
    .expect_err("a needed malformed key refuses");
    assert!(
        refused.to_string().contains("grid"),
        "refusal names the key: {refused}"
    );
    // And a needed key paints shapes when healthy.
    let (ctx, painter) = test_painter();
    paint_overlay(&painter, &with_levels, &full, 320.0).expect("levels paint");
    assert!(!end_frame(&ctx).shapes.is_empty(), "levels emit shapes");
}

#[test]
fn translate_overlay_shifts_all_five_primitive_vecs() {
    // Every pane-local primitive kind moves by the pane origin: series points
    // and markers and labels on both axes, level lines and area corners with
    // their respective verticals, and holes never resurrect.
    let origin = (10.0_f32, 20.0_f32);
    let geoms = OverlayGeoms {
        series: vec![SeriesGeom {
            points: vec![Some((0.0, 100.0)), None, Some((8.0, 90.0))],
        }],
        levels: vec![50.0],
        markers: vec![(8.0, 100.0)],
        labels: vec![PlacedLabel {
            x: 16.0,
            y: 150.0,
            text: "hi".to_owned(),
        }],
        areas: vec![(0.0, 20.0, 24.0, 180.0)],
    };
    let shifted = translate_overlay(geoms, origin);
    assert_eq!(
        shifted.series[0].points[0],
        Some((0.0 + origin.0, 100.0 + origin.1))
    );
    assert_eq!(shifted.series[0].points[1], None, "holes stay holed");
    assert_eq!(
        shifted.series[0].points[2],
        Some((8.0 + origin.0, 90.0 + origin.1))
    );
    assert_eq!(shifted.levels, vec![50.0 + origin.1]);
    assert_eq!(shifted.markers, vec![(8.0 + origin.0, 100.0 + origin.1)]);
    assert_eq!(
        (shifted.labels[0].x, shifted.labels[0].y),
        (16.0 + origin.0, 150.0 + origin.1)
    );
    assert_eq!(
        shifted.areas[0],
        (
            0.0 + origin.0,
            20.0 + origin.1,
            24.0 + origin.0,
            180.0 + origin.1
        )
    );
}

#[test]
fn unsupported_projection_renders_a_chip() {
    // A refused projection never paints a partial overlay: the chip names the
    // construct and reason instead.
    let refusal = ProjectionRefusal::Unsupported {
        construct: "table.new".to_owned(),
        reason: "no table primitive",
    };
    egui::__run_test_ui(|ui| {
        paint_unsupported_chip(ui, &refusal);
    });
}
