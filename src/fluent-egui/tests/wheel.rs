//! Region-aware wheel routing: a wheel acts on the zone under the pointer,
//! and each target carries a pane-local anchor.

use super::*;

use crate::interaction::{WHEEL_STEP_PX, WHEEL_ZOOM_BASE, wheel_zoom_factor, zoom_range_around};
use protopyne_chart_scale::PriceRange;

/// A price pane over `[0, 300] x [0, 400]` with a 68 px axis strip on the
/// right, an 18 px time strip at the bottom.
fn main_pane() -> PaneZones {
    PaneZones {
        plot: Rect::new(Pos2::new(0.0, 0.0), Pos2::new(300.0, 382.0)),
        axis: Rect::new(Pos2::new(300.0, 0.0), Pos2::new(368.0, 382.0)),
    }
}

/// An oscillator pane below it, sharing the same left edge and axis width.
fn sub_pane() -> PaneZones {
    PaneZones {
        plot: Rect::new(Pos2::new(0.0, 400.0), Pos2::new(300.0, 540.0)),
        axis: Rect::new(Pos2::new(300.0, 400.0), Pos2::new(368.0, 540.0)),
    }
}

fn chart_zones() -> WheelZones {
    WheelZones::new(vec![main_pane(), sub_pane()], None)
}

fn chart_with_list() -> WheelZones {
    WheelZones::new(vec![main_pane(), sub_pane()], Some(Rect::new(
        Pos2::new(180.0, 10.0),
        Pos2::new(290.0, 60.0),
    )))
}

#[test]
fn plot_wheel_zooms_the_time_axis() {
    let at = Pos2::new(120.0, 200.0);
    assert_eq!(
        chart_zones().route(at),
        Some(WheelTarget::Time { pane: 0, anchor_x: 120.0 }),
        "a wheel over the candles zooms x, anchored pane-local"
    );
}

#[test]
fn oscillator_plot_zooms_the_same_time_axis() {
    let at = Pos2::new(120.0, 480.0);
    assert_eq!(
        chart_zones().route(at),
        Some(WheelTarget::Time { pane: 1, anchor_x: 120.0 }),
        "the sub-pane shares the time axis, so it routes to x too — and it \
         names pane 1, so a gesture that also wants a y anchor takes it from \
         the oscillator rather than from the pane above"
    );
}

#[test]
fn price_axis_zooms_that_panes_price() {
    let zones = chart_zones();
    assert_eq!(
        zones.route(Pos2::new(320.0, 200.0)),
        Some(WheelTarget::Price { pane: 0, anchor_y: 200.0 }),
        "the price strip zooms the price pane"
    );
    assert_eq!(
        zones.route(Pos2::new(320.0, 480.0)),
        Some(WheelTarget::Price { pane: 1, anchor_y: 80.0 }),
        "the oscillator's own strip zooms the oscillator, anchored pane-local"
    );
}

#[test]
fn legend_wheel_scrolls_instead_of_zooming() {
    let zones = chart_with_list();
    assert_eq!(
        zones.route(Pos2::new(200.0, 20.0)),
        Some(WheelTarget::List),
        "a wheel over the floating list scrolls it"
    );
}

#[test]
fn list_wins_over_the_plot_beneath_it() {
    // The legend floats over the candles; the overlay, not the pane, takes
    // the gesture.
    let zones = chart_with_list();
    assert_eq!(zones.route(Pos2::new(200.0, 20.0)), Some(WheelTarget::List));
    assert_eq!(
        zones.route(Pos2::new(40.0, 20.0)),
        Some(WheelTarget::Time { pane: 0, anchor_x: 40.0 }),
        "beside the legend the plot still zooms x"
    );
}

#[test]
fn outside_every_zone_routes_nowhere() {
    let zones = chart_with_list();
    assert_eq!(zones.route(Pos2::new(200.0, 600.0)), None, "below the chart");
    assert_eq!(zones.route(Pos2::new(-5.0, 200.0)), None, "left of the chart");
    assert_eq!(zones.route(Pos2::new(400.0, 200.0)), None, "right of the axis");
}

#[test]
fn a_single_pane_chart_still_routes() {
    let zones = WheelZones::new(vec![main_pane()], None);
    assert_eq!(zones.route(Pos2::new(320.0, 200.0)), Some(WheelTarget::Price { pane: 0, anchor_y: 200.0 }));
    assert_eq!(zones.route(Pos2::new(10.0, 10.0)), Some(WheelTarget::Time { pane: 0, anchor_x: 10.0 }));
}

#[test]
fn zero_panes_and_no_list_route_nowhere() {
    let zones = WheelZones::default();
    assert_eq!(zones.route(Pos2::new(10.0, 10.0)), None, "an empty chart claims nothing");
}

#[test]
fn only_zoom_targets_report_as_zoom() {
    assert!(WheelTarget::Time { pane: 0, anchor_x: 1.0 }.is_zoom());
    assert!(WheelTarget::Price { pane: 0, anchor_y: 1.0 }.is_zoom());
    assert!(!WheelTarget::List.is_zoom(), "scrolling a list is not a chart zoom");
}

#[test]
fn rect_normalizes_swapped_corners() {
    let rect = Rect::new(Pos2::new(30.0, 40.0), Pos2::new(10.0, 20.0));
    assert_eq!((rect.left(), rect.top()), (10.0, 20.0), "corners normalize");
    assert!(rect.contains(Pos2::new(20.0, 30.0)), "the swapped rect still contains its interior");
    assert!(!rect.contains(Pos2::new(40.0, 30.0)), "and excludes what is outside");
}

#[test]
fn rect_edges_are_inclusive_so_a_boundary_belongs_to_exactly_one_zone() {
    let rect = Rect::new(Pos2::new(10.0, 10.0), Pos2::new(20.0, 20.0));
    assert!(rect.contains(Pos2::new(10.0, 15.0)), "left edge is in");
    assert!(rect.contains(Pos2::new(20.0, 15.0)), "right edge is in");
    // Priority resolves the shared edge: the axis strip is tested first, so a
    // point on the plot/axis seam belongs to the axis.
    let zones = chart_zones();
    assert_eq!(
        zones.route(Pos2::new(300.0, 200.0)),
        Some(WheelTarget::Price { pane: 0, anchor_y: 200.0 }),
        "the seam goes to the axis strip"
    );
}

#[test]
fn egui_rects_convert_at_the_boundary() {
    let rect = egui::Rect::from_min_max(egui::pos2(5.0, 6.0), egui::pos2(25.0, 26.0));
    let converted: Rect = rect.into();
    assert_eq!((converted.left(), converted.top()), (5.0, 6.0), "min corner converts");
    assert_eq!((converted.max_x, converted.max_y), (25.0, 26.0), "max corner converts");
    let point: Pos2 = egui::pos2(10.0, 10.0).into();
    assert!(converted.contains(point), "a converted point still routes");
}

#[test]
fn wheel_factor_zooms_in_on_scroll_up_and_out_on_scroll_down() {
    let up = wheel_zoom_factor(WHEEL_STEP_PX as f32);
    let down = wheel_zoom_factor(-(WHEEL_STEP_PX as f32));
    // The factor scales the *span*, so a factor under one zooms in: scrolling
    // up narrows the window, scrolling down widens it.
    assert!(
        close_factor(up, 1.0 / WHEEL_ZOOM_BASE),
        "one notch up narrows the span: {up}"
    );
    assert!(
        close_factor(down, WHEEL_ZOOM_BASE),
        "one notch down widens the span by the base: {down}"
    );
    assert!(close_factor(wheel_zoom_factor(0.0), 1.0), "a still wheel is neutral");
}

#[test]
fn range_zoom_holds_the_anchor_price() {
    let range = PriceRange::new(90.0, 110.0).expect("valid range");
    let anchor = 100.0;
    let zoomed = zoom_range_around(range, anchor, wheel_zoom_factor(50.0))
        .expect("a finite positive factor zooms");
    let span = zoomed.max - zoomed.min;
    assert!(
        (span - 20.0 / WHEEL_ZOOM_BASE).abs() < 1e-9,
        "the span scales by one notch: {span}"
    );
    assert!(
        (f64::midpoint(zoomed.min, zoomed.max) - anchor).abs() < 1e-9,
        "the anchor price stays put"
    );
}

#[test]
fn range_zoom_refuses_degenerate_input() {
    let range = PriceRange::new(90.0, 110.0).expect("valid range");
    assert!(zoom_range_around(range, 100.0, f64::NAN).is_none(), "non-finite factor refuses");
    assert!(zoom_range_around(range, 100.0, 0.0).is_none(), "zero factor refuses");
    assert!(zoom_range_around(range, 100.0, -1.0).is_none(), "negative factor refuses");
    assert!(zoom_range_around(range, f64::NAN, 1.1).is_none(), "non-finite anchor refuses");
    assert!(zoom_range_around(range, 100.0, 1.0).is_some(), "a neutral factor is still valid");
}

fn close_factor(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0)
}