use super::*;

fn ramp_rows(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: i as i64,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

fn visible_fit(rows: &[ChartRow], main: Viewport) -> (f64, f64) {
    let (start, end) = main.culled_bar_range(640.0, rows.len());
    let fitted = fluent_egui::auto_height_range(
        rows,
        (start, end.min(rows.len())),
        AUTO_HEIGHT_PAD_FRACTION,
    )
    .expect("fits");
    (fitted.min.max(0.0), fitted.max)
}

#[test]
fn adding_an_indicator_keeps_x_and_refits_y_to_the_visible_window() {
    let rows = ramp_rows(200);
    let mut workspace = ChartWorkspace::with_rows(rows.clone()).expect("workspace boots");
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::Pan2D {
            dx_px: 120.0,
            dy_px: 0.0,
        }],
    );
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::WheelTime {
            delta_px: 40.0,
            anchor_x: 160.0,
        }],
    );
    let before = workspace.main_viewport().expect("main");
    let offset_before = before.x_offset_px();
    let spacing_before = before.scale().bar_spacing();
    assert!(
        offset_before.abs() > f32::EPSILON,
        "the pan moved the time axis"
    );
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace
        .recompute_preserving_view()
        .expect("indicator-side recompute");
    let main = workspace.main_viewport().expect("main");
    assert!(
        (main.x_offset_px() - offset_before).abs() <= f32::EPSILON,
        "adding an indicator keeps the X position"
    );
    assert!(
        (main.scale().bar_spacing() - spacing_before).abs() <= f32::EPSILON,
        "adding an indicator keeps the X zoom"
    );
    let (lo, hi) = visible_fit(&rows, main);
    let range = main.scale().range();
    assert!(
        (range.min - lo).abs() <= 1e-6 && (range.max - hi).abs() <= 1e-6,
        "Y refits to the visible window like Refresh, got ({}, {}) want ({lo}, {hi})",
        range.min,
        range.max,
    );
}

#[test]
fn committing_settings_keeps_x_and_refits_y_to_the_visible_window() {
    let rows = ramp_rows(200);
    let mut workspace = ChartWorkspace::with_rows(rows.clone()).expect("workspace boots");
    workspace.slot_indicator("sma").expect("sma slots");
    workspace.recompute().expect("evaluations build");
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::Pan2D {
            dx_px: 120.0,
            dy_px: 0.0,
        }],
    );
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::WheelTime {
            delta_px: 40.0,
            anchor_x: 160.0,
        }],
    );
    let before = workspace.main_viewport().expect("main");
    let offset_before = before.x_offset_px();
    let spacing_before = before.scale().bar_spacing();
    assert!(
        offset_before.abs() > f32::EPSILON,
        "the pan moved the time axis"
    );
    workspace
        .set_slot_field("sma", "length", "20")
        .expect("field edits");
    workspace
        .commit_slot_settings("sma")
        .expect("commit applies");
    let main = workspace.main_viewport().expect("main");
    assert!(
        (main.x_offset_px() - offset_before).abs() <= f32::EPSILON,
        "committing settings keeps the X position"
    );
    assert!(
        (main.scale().bar_spacing() - spacing_before).abs() <= f32::EPSILON,
        "committing settings keeps the X zoom"
    );
    let (lo, hi) = visible_fit(&rows, main);
    let range = main.scale().range();
    assert!(
        (range.min - lo).abs() <= 1e-6 && (range.max - hi).abs() <= 1e-6,
        "Y refits to the visible window like Refresh, got ({}, {}) want ({lo}, {hi})",
        range.min,
        range.max,
    );
}
