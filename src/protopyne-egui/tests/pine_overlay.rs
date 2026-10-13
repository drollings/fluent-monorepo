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
    assert_eq!(points[0].map(|(_, y)| y), Some(0.0), "range top paints y = 0");
    assert_eq!(points[1], None, "holes stay holes");
    assert_eq!(points[2].map(|(_, y)| y), Some(200.0), "range bottom paints y = height");
    let pane = viewport();
    assert_eq!(points[0].map(|(x, _)| x), Some(pane.index_to_x(0)));
    assert_eq!(points[2].map(|(x, _)| x), Some(pane.index_to_x(2)));
}

#[test]
fn levels_markers_labels_and_areas_place() {
    // Every primitive kind lands on the viewport mapping; text rides along.
    let pane = viewport();
    let geoms = overlay_geoms(
        &[
            PlotPrimitive::Level { title: "L".to_owned(), price: 100.0 },
            PlotPrimitive::Marker { title: "M".to_owned(), bar: 1, price: 105.0 },
            PlotPrimitive::Label { bar: 2, price: 95.0, text: "hi".to_owned() },
            PlotPrimitive::Area { left: 0, top: 108.0, right: 3, bottom: 92.0 },
        ],
        &pane,
    );
    assert_eq!(geoms.levels, vec![pane.price_to_y(100.0).expect("100 maps")]);
    assert_eq!(
        geoms.markers,
        vec![(
            pane.index_to_x(1),
            pane.price_to_y(105.0).expect("105 maps"),
        )]
    );
    assert_eq!(geoms.labels.len(), 1);
    assert_eq!(geoms.labels[0].text, "hi");
    assert_eq!(
        (geoms.labels[0].x, geoms.labels[0].y),
        (pane.index_to_x(2), pane.price_to_y(95.0).expect("95 maps")),
    );
    assert_eq!(geoms.areas.len(), 1);
    let (x0, y0, x1, y1) = geoms.areas[0];
    assert_eq!((x0, y0), (pane.index_to_x(0), pane.price_to_y(108.0).expect("108 maps")));
    assert_eq!((x1, y1), (pane.index_to_x(3), pane.price_to_y(92.0).expect("92 maps")));
}

#[test]
fn unmappable_prices_stay_unpainted() {
    // Non-finite prices map to nothing: the overlay skips them instead of
    // substituting a pixel.
    let geoms = overlay_geoms(
        &[
            PlotPrimitive::Series { title: "S".to_owned(), values: vec![Some(f64::NAN)] },
            PlotPrimitive::Level { title: "L".to_owned(), price: f64::INFINITY },
            PlotPrimitive::Marker { title: "M".to_owned(), bar: 0, price: f64::NAN },
            PlotPrimitive::Label { bar: 0, price: f64::NAN, text: "x".to_owned() },
            PlotPrimitive::Area { left: 0, top: f64::NAN, right: 1, bottom: 90.0 },
        ],
        &viewport(),
    );
    assert_eq!(geoms.series[0].points, vec![None]);
    assert!(geoms.levels.is_empty(), "no level for an unmappable price");
    assert!(geoms.markers.is_empty(), "no marker for an unmappable price");
    assert!(geoms.labels.is_empty(), "no label for an unmappable price");
    assert!(geoms.areas.is_empty(), "no area for an unmappable price");
}
