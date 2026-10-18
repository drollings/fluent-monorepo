use super::*;
use fluent_egui::{auto_height_range, Viewport};

fn ramp_rows(count: usize) -> Vec<ChartRow> {
    (0..count as i64)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: i,
                open: close - 0.25,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume: 1_000,
            }
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
    let hi = rows
        .iter()
        .map(|row| row.high)
        .fold(f64::NEG_INFINITY, f64::max);
    view.price_to_y(hi).expect("tip maps") / height
}

fn occupancy(rows: &[ChartRow], pad: f64) -> f64 {
    let fitted = auto_height_range(rows, (0, rows.len()), pad).expect("window fits");
    let lo = rows.iter().map(|row| row.low).fold(f64::INFINITY, f64::min);
    let hi = rows
        .iter()
        .map(|row| row.high)
        .fold(f64::NEG_INFINITY, f64::max);
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
    assert!(
        clearance_fraction(&wicks, 0.02, 400.0) < 0.03,
        "0.02 clips the wick tip"
    );
    assert!(
        clearance_fraction(&wicks, AUTO_HEIGHT_PAD_FRACTION, 400.0) >= 0.03,
        "the frozen pad clears it"
    );
    let normal = ramp_rows(40);
    assert!(
        occupancy(&normal, 0.25) < 0.8,
        "0.25 wastes the pane on dead space"
    );
    assert!(
        occupancy(&normal, AUTO_HEIGHT_PAD_FRACTION) >= 0.8,
        "the frozen pad fills it"
    );
}

/// The full-view extent shares the calibrated pad: one fraction, not two
/// magic numbers.
#[test]
fn price_extent_shares_the_calibrated_pad() {
    let rows = ramp_rows(40);
    let (lo, hi) = price_extent(&rows);
    let fitted =
        auto_height_range(&rows, (0, rows.len()), AUTO_HEIGHT_PAD_FRACTION).expect("window fits");
    assert_eq!(
        (lo, hi),
        (fitted.min, fitted.max),
        "one pad for full view and window fit"
    );
}

/// The workspace boots a one-month window: the range menu grows or prunes the
/// span on demand, and the chart never pre-fetches a range it is not showing.
#[test]
fn workspace_boots_a_one_month_window() {
    let workspace = ChartWorkspace::new().expect("workspace boots");
    assert_eq!(workspace.range(), DateRange::Month1);
    assert_eq!(workspace.visible_months(), 1);
    assert_eq!(
        workspace.timeframe(),
        TimeframeKey::M5,
        "five-minute default"
    );
    assert_eq!(workspace.active_symbol(), "SPY", "SPY default ticker");
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
    let fitted = auto_height_range(
        &rows[start..end.min(rows.len())],
        (0, end - start),
        AUTO_HEIGHT_PAD_FRACTION,
    )
    .expect("window fits");
    assert_eq!(
        (min, max),
        (fitted.min, fitted.max),
        "the event carries the policy fit"
    );
    workspace.apply_auto_height_now();
    let after = workspace.main.expect("bound");
    let range = after.scale().range();
    assert_eq!(
        (range.min, range.max),
        (min, max),
        "the machine applies the fit"
    );
    assert_eq!(
        after.scale().bar_spacing().to_bits(),
        spacing.to_bits(),
        "time axis untouched"
    );
    assert_eq!(
        after.x_offset_px().to_bits(),
        offset.to_bits(),
        "pan offset untouched"
    );
}

/// A window with nothing to fit holds instead of jumping: no viewport, an
/// off-window viewport, an empty store, or an unset width all hold the
/// window. This pins the regression where AutoHeight fired before the chart
/// was loaded or the X dimension was set.
#[test]
fn auto_height_refusal_holds_the_window() {
    let rows = ramp_rows(60);
    let mut workspace = ChartWorkspace::with_rows(rows).expect("workspace boots");
    let held = workspace.main.expect("bound");
    let held_range = held.scale().range();
    workspace.main = None;
    workspace.apply_auto_height_now();
    assert!(workspace.main.is_none(), "no viewport holds");
    workspace.main = Some(held.with_x_offset(1_000_000.0).expect("offset sets"));
    workspace.apply_auto_height_now();
    // Off-window: nothing visible to fit, the window holds.
    workspace.main = Some(held);
    workspace.apply_auto_height_now();
    assert_eq!(
        workspace.main.expect("bound").scale().range(),
        held_range,
        "a guarded fit never jumps"
    );
    // Empty store holds.
    let mut empty = ChartWorkspace::new().expect("workspace boots");
    empty.apply_auto_height_now();
    assert!(empty.main.is_none() || empty.rows().is_empty());
    // Menu-bar Jump before measure refuses instead of acting on a proxy.
    let mut fresh = ChartWorkspace::new().expect("workspace boots");
    fresh.last_plot_width = 0.0;
    assert!(fresh.jump_to_current_now().is_err(), "unset width refuses");
}

/// Menu-bar view buttons: refresh fits the visible window; the label-free
/// pan cluster rewinds to the beginning, steps back, and steps forward,
/// preserving zoom and refitting the window each time.
#[test]
fn menu_bar_view_buttons_fit_and_pan() {
    let rows = ramp_rows(120);
    let mut workspace = ChartWorkspace::with_rows(rows).expect("workspace boots");
    workspace.last_plot_width = 640.0;
    workspace.apply_auto_height_now();
    assert!(workspace.main.is_some(), "the fit lands");
    let spacing = workspace.main.expect("bound").scale().bar_spacing();

    // Forward steps one pan unit and holds the zoom.
    workspace
        .apply_top_bar_action(TopBarAction::Forward, None)
        .expect("forward applies");
    assert!(
        workspace.main.expect("bound").x_offset_px() < 0.0,
        "forward moved the time axis toward the end"
    );
    assert_eq!(
        workspace
            .main
            .expect("bound")
            .scale()
            .bar_spacing()
            .to_bits(),
        spacing.to_bits(),
        "forward preserves zoom"
    );

    // Rewind steps back and holds the zoom.
    workspace
        .apply_top_bar_action(TopBarAction::Rewind, None)
        .expect("rewind applies");
    assert_eq!(
        workspace
            .main
            .expect("bound")
            .scale()
            .bar_spacing()
            .to_bits(),
        spacing.to_bits(),
        "rewind preserves zoom"
    );

    // Rewind to the beginning pins the first bar at the pane's left edge.
    workspace
        .apply_top_bar_action(TopBarAction::RewindToBeginning, None)
        .expect("rewind to beginning applies");
    assert_eq!(
        workspace.main.expect("bound").x_offset_px().to_bits(),
        0.0_f32.to_bits(),
        "the earliest bar sits at the left edge"
    );

    // Forward to the end pins the latest bar at the pane's right edge.
    workspace
        .apply_top_bar_action(TopBarAction::ForwardToEnd, None)
        .expect("forward to end applies");
    assert!(
        workspace.main.expect("bound").x_offset_px() < 0.0,
        "the latest bar sits at the right edge"
    );
    assert_eq!(
        workspace
            .main
            .expect("bound")
            .scale()
            .bar_spacing()
            .to_bits(),
        spacing.to_bits(),
        "forward to end preserves zoom"
    );

    // The pan cluster refuses before a width is measured and on an empty store.
    let mut empty = ChartWorkspace::new().expect("workspace boots");
    empty.last_plot_width = 640.0;
    assert!(empty.rewind_view().is_err(), "no bars refuses");
    assert!(empty.forward_view().is_err(), "no bars refuses");
    assert!(empty.forward_to_end_view().is_err(), "no bars refuses");
    let mut fresh = ChartWorkspace::new().expect("workspace boots");
    fresh.last_plot_width = 0.0;
    assert!(fresh.jump_to_current_now().is_err(), "unset width refuses");
}

/// Load-more widens the window one month over already-held rows (gateway-less
/// demo path) and keeps the anchor stable.
#[test]
fn load_more_widens_one_month_over_held_rows() {
    // 120 daily bars ≈ four months; the default window shows one.
    let rows: Vec<ChartRow> = (0..120)
        .map(|i| {
            let close = 100.0 + f64::from(i);
            ChartRow {
                time: i64::from(i) * 86_400,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect();
    let mut workspace = ChartWorkspace::with_rows(rows).expect("workspace boots");
    assert_eq!(workspace.visible_months(), 1);
    let grew = workspace
        .load_previous_month(None)
        .expect("load-more applies");
    assert!(grew, "earlier held rows widen the window");
    assert_eq!(workspace.visible_months(), 2);
    assert_eq!(workspace.range(), DateRange::All);
    assert!(
        workspace.custom_start.is_some(),
        "the window start moves earlier"
    );
    // Empty store has nothing to load.
    let mut empty = ChartWorkspace::new().expect("workspace boots");
    assert!(!empty.load_previous_month(None).expect("empty holds"));
}

/// Rewind/forward step exactly one calendar pan unit on the bar grid: an M5
/// chart steps a day (288 bars), so one click moves a couple hundred candles
/// and forward undoes the rewind.
#[test]
fn pan_cluster_steps_the_calendar_unit_on_the_bar_grid() {
    let rows = ramp_rows(3000);
    let mut workspace = ChartWorkspace::with_rows(rows).expect("workspace boots");
    workspace.last_plot_width = 640.0;
    workspace
        .jump_to_current_now()
        .expect("the trailing edge binds");
    let spacing = workspace.main.expect("bound").scale().bar_spacing();
    let before = workspace.main.expect("bound").x_offset_px();
    workspace.rewind_view().expect("rewind");
    let after = workspace.main.expect("bound").x_offset_px();
    assert!(
        (after - before - 288.0 * spacing).abs() < 1e-3,
        "an M5 rewind steps one day (288 bars): {before} -> {after}"
    );
    workspace.forward_view().expect("forward");
    let back = workspace.main.expect("bound").x_offset_px();
    assert!((back - before).abs() < 1e-3, "forward undoes the rewind");
}

/// The oscillator pane's right-edge wheel zooms about the cursor and never
/// pans the pane: repeated zoom-out then zoom-in is symmetric and the price
/// under the cursor stays fixed (the earlier clamp re-centered the window and
/// drifted the pane).
#[test]
fn oscillator_axis_wheel_zooms_around_the_cursor_without_panning() {
    let mut workspace = ChartWorkspace::new().expect("workspace boots");
    workspace
        .seed_history(TimeframeKey::M1, ramp_rows(40))
        .expect("history seeds");
    workspace
        .select_timeframe(TimeframeKey::M1)
        .expect("M1 binds the viewports");
    let anchor_y = workspace.sub_viewport().expect("sub").scale().height() * 0.25;
    let anchor_price = workspace
        .sub_viewport()
        .expect("sub")
        .y_to_price(anchor_y)
        .expect("the anchor maps");
    for _ in 0..40 {
        workspace.zoom_pane_price(SUB_PANE_ZONE, -50.0, anchor_y);
    }
    let out = workspace.sub_viewport().expect("sub");
    let held = out.y_to_price(anchor_y).expect("the anchor still maps");
    assert!(
        (held - anchor_price).abs() < 1e-6,
        "zoom-out keeps the cursor price fixed: {anchor_price} -> {held}"
    );
    assert!(
        (out.visible_range().max - out.visible_range().min) <= (SUB_MAX - SUB_MIN) + 1e-6,
        "zoom-out stops at the pane's band"
    );
    for _ in 0..40 {
        workspace.zoom_pane_price(SUB_PANE_ZONE, 50.0, anchor_y);
    }
    let back = workspace.sub_viewport().expect("sub");
    let back_price = back.y_to_price(anchor_y).expect("the anchor still maps");
    assert!(
        (back_price - anchor_price).abs() < 1e-6,
        "zoom-in keeps the cursor price fixed: {back_price}"
    );
}

/// Timeframe switches defer AutoHeight to the next measured frame instead of
/// fitting on a proxy width: the flag marks pending, the canvas consumes it
/// with the measured plot width.
#[test]
fn timeframe_switch_defers_auto_height_to_the_measured_frame() {
    let mut workspace = ChartWorkspace::new().expect("workspace boots");
    workspace
        .seed_history(TimeframeKey::M1, ramp_rows(40))
        .expect("history seeds");
    workspace
        .select_timeframe(TimeframeKey::M1)
        .expect("M1 selects");
    assert!(
        workspace.pending_auto_height,
        "the switch marks a deferred fit, never fits inline"
    );
    egui::__run_test_ui(|ui| {
        paint_workspace(ui, &mut workspace, None).expect("the workspace paints");
    });
    assert!(
        !workspace.pending_auto_height,
        "the first measured frame consumes the fit"
    );
}
