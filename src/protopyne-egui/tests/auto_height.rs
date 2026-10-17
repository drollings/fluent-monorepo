use super::*;
use fluent_egui::{Viewport, auto_height_range};

fn ramp_rows(count: usize) -> Vec<ChartRow> {
    (0..count as i64)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow { time: i, open: close - 0.25, high: close + 0.5, low: close - 0.5, close, volume: 1_000 }
        })
        .collect()
}

/// Long-wick window: bodies on `[100, 110]`, one bar spiking to 140.
fn wick_rows() -> Vec<ChartRow> {
    (0..10)
        .map(|i| ChartRow {
            time: i,
            open: 100.0,
            high: if i == 5 { 140.0 } else { 110.0 },
            low: 100.0,
            close: 105.0,
            volume: 1_000,
        })
        .collect()
}

fn clearance_fraction(rows: &[ChartRow], pad: f64, height: f32) -> f32 {
    let fitted = auto_height_range(rows, (0, rows.len()), pad).expect("window fits");
    let view = Viewport::new(
        PriceScaleMode::Normal,
        fitted.min,
        fitted.max,
        height,
        8.0,
        fitted.min,
        false,
    )
    .expect("fitted viewport builds");
    let hi = rows.iter().map(|row| row.high).fold(f64::NEG_INFINITY, f64::max);
    view.price_to_y(hi).expect("tip maps") / height
}

fn occupancy(rows: &[ChartRow], pad: f64) -> f64 {
    let fitted = auto_height_range(rows, (0, rows.len()), pad).expect("window fits");
    let lo = rows.iter().map(|row| row.low).fold(f64::INFINITY, f64::min);
    let hi = rows.iter().map(|row| row.high).fold(f64::NEG_INFINITY, f64::max);
    (hi - lo) / (fitted.max - fitted.min)
}

/// Padding calibration (task value: legibility): the clipped-wick control
/// needs tip clearance for a 40%-of-span wick, the dead-space control needs
/// the data band to fill the pane. The bounds discriminate — 0.02 clips,
/// 0.25 wastes — and the frozen fraction lands both with margin.
#[test]
fn pad_fraction_is_calibrated() {
    assert_eq!(
        AUTO_HEIGHT_PAD_FRACTION.to_bits(),
        0.05f64.to_bits(),
        "the fraction is frozen; a retune re-runs this calibration"
    );
    let wicks = wick_rows();
    assert!(clearance_fraction(&wicks, 0.02, 400.0) < 0.03, "0.02 clips the wick tip");
    assert!(clearance_fraction(&wicks, AUTO_HEIGHT_PAD_FRACTION, 400.0) >= 0.03, "the frozen pad clears it");
    let normal = ramp_rows(40);
    assert!(occupancy(&normal, 0.25) < 0.8, "0.25 wastes the pane on dead space");
    assert!(occupancy(&normal, AUTO_HEIGHT_PAD_FRACTION) >= 0.8, "the frozen pad fills it");
}

/// The full-view extent shares the calibrated pad: one fraction, not two
/// magic numbers.
#[test]
fn price_extent_shares_the_calibrated_pad() {
    let rows = ramp_rows(40);
    let (lo, hi) = price_extent(&rows);
    let fitted =
        auto_height_range(&rows, (0, rows.len()), AUTO_HEIGHT_PAD_FRACTION).expect("window fits");
    assert_eq!((lo, hi), (fitted.min, fitted.max), "one pad for full view and window fit");
}

/// The price-axis menu lists AutoHeight first; future entries reuse the same
/// action enum.
#[test]
fn menu_lists_auto_height_for_the_price_axis() {
    assert_eq!(chart_menu_items(), [(ChartMenuAction::AutoHeight, "AutoHeight")]);
}

/// UI contract (interaction-level): a secondary click on the price axis
/// strip raises the trigger; a click on the plot, an unclicked frame, a
/// missing viewport, and empty rows raise nothing.
#[test]
fn context_request_answers_only_the_price_axis() {
    let rows = ramp_rows(60);
    let view = Viewport::new(PriceScaleMode::Normal, 90.0, 170.0, 400.0, 8.0, 90.0, false)
        .expect("viewport builds");
    let plot = egui::Rect::from_min_max(egui::Pos2::new(0.0, 0.0), egui::Pos2::new(572.0, 382.0));
    let axis = egui::Rect::from_min_max(egui::Pos2::new(572.0, 0.0), egui::Pos2::new(640.0, 382.0));
    let on_axis = egui::Pos2::new(600.0, 120.0);
    let on_plot = egui::Pos2::new(120.0, 200.0);
    let (trigger, at) =
        context_menu_request(true, Some(on_axis), plot, axis, Some(&view), &rows)
            .expect("the axis strip answers");
    assert_eq!(at, on_axis, "the menu anchors at the pointer");
    assert_eq!(trigger.pane, PRICE_PANE_ZONE, "the price pane owns the menu");
    assert!(context_menu_request(true, Some(on_plot), plot, axis, Some(&view), &rows).is_none());
    assert!(context_menu_request(false, Some(on_axis), plot, axis, Some(&view), &rows).is_none());
    assert!(context_menu_request(true, Some(on_axis), plot, axis, None, &rows).is_none());
    assert!(context_menu_request(true, Some(on_axis), plot, axis, Some(&view), &[]).is_none());
    assert!(context_menu_request(true, None, plot, axis, Some(&view), &rows).is_none());
}

/// End to end through the machine: the action fits the culled window's
/// extremes with the calibrated pad, stops the vertical coast, and leaves
/// the time axis untouched.
#[test]
fn applying_auto_height_fits_the_visible_window() {
    let rows = ramp_rows(120);
    let mut workspace = ChartWorkspace::with_rows(rows.clone()).expect("workspace boots");
    assert!(workspace.main.is_some(), "seeded rows bind viewports");
    let before = workspace.main.expect("bound");
    let spacing = before.scale().bar_spacing();
    let offset = before.x_offset_px();
    let event = workspace.auto_height_event(640.0).expect("the window fits");
    let PointerInput::AutoHeight { min, max } = event else {
        panic!("the action rides an AutoHeight event: {event:?}");
    };
    let (start, end) = before.culled_bar_range(640.0, rows.len());
    let fitted =
        auto_height_range(&rows[start..end.min(rows.len())], (0, end - start), AUTO_HEIGHT_PAD_FRACTION)
            .expect("window fits");
    assert_eq!((min, max), (fitted.min, fitted.max), "the event carries the policy fit");
    workspace.apply_menu_action(ChartMenuAction::AutoHeight, 640.0).expect("applies");
    let after = workspace.main.expect("bound");
    let range = after.scale().range();
    assert_eq!((range.min, range.max), (min, max), "the machine applies the fit");
    assert_eq!(after.scale().bar_spacing().to_bits(), spacing.to_bits(), "time axis untouched");
    assert_eq!(after.x_offset_px().to_bits(), offset.to_bits(), "pan offset untouched");
}

/// A window with nothing to fit refuses instead of jumping: no viewport, or
/// a viewport panned fully off the data.
#[test]
fn auto_height_refusal_holds_the_window() {
    let rows = ramp_rows(60);
    let mut workspace = ChartWorkspace::with_rows(rows).expect("workspace boots");
    let held = workspace.main.expect("bound");
    workspace.main = None;
    assert!(workspace.auto_height_event(640.0).is_err(), "no viewport refuses");
    assert!(workspace.apply_menu_action(ChartMenuAction::AutoHeight, 640.0).is_err());
    assert!(workspace.main.is_none(), "a refusal binds nothing");
    workspace.main = Some(held.with_x_offset(1_000_000.0).expect("offset sets"));
    assert!(workspace.auto_height_event(640.0).is_err(), "an off-window viewport refuses");
}
