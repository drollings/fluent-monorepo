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

fn test_layouts(pane_count: usize) -> Vec<PaneLayout> {
    let canvas = egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(640.0, NOMINAL_CANVAS_HEIGHT),
    );
    let rects = split_canvas_many(canvas, DEFAULT_PANE_SHARE, pane_count);
    std::iter::once(true)
        .chain(std::iter::repeat_n(false, pane_count))
        .zip(rects.iter())
        .map(|(time_strip, rect)| PaneLayout::split(*rect, time_strip))
        .collect()
}

#[test]
fn pane_resolution_drives_merge_rows() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");
    assert!(
        workspace
            .custom_pane_ids()
            .contains(&"kernel:rsi".to_owned()),
        "the fixture owns a custom pane"
    );
    let layouts = test_layouts(2);
    assert_eq!(layouts.len(), 3, "price plus shared plus one custom");

    let main_pos = layouts[0].plot.center();
    let shared_pos = layouts[1].plot.center();
    let custom_pos = layouts[2].plot.center();

    let main_pane = workspace
        .resolve_context_pane(&layouts, main_pos)
        .expect("main resolves");
    assert_eq!(main_pane, ContextPane::Main, "main rect resolves to main");
    workspace.context_pane = Some(main_pane);
    let main_labels = workspace.context_menu_labels();
    assert!(
        !main_labels.iter().any(|label| label.contains("Merge")),
        "main offers no merge rows: {main_labels:?}"
    );

    let shared_pane = workspace
        .resolve_context_pane(&layouts, shared_pos)
        .expect("shared resolves");
    assert_eq!(shared_pane, ContextPane::Shared, "shared rect resolves");
    workspace.context_pane = Some(shared_pane);
    let shared_labels = workspace.context_menu_labels();
    assert!(
        !shared_labels.iter().any(|label| label.contains("Merge")),
        "shared offers no merge rows: {shared_labels:?}"
    );

    let custom_pane = workspace
        .resolve_context_pane(&layouts, custom_pos)
        .expect("custom resolves");
    assert_eq!(
        custom_pane,
        ContextPane::Custom("kernel:rsi".to_owned()),
        "custom rect resolves to its pane id"
    );
    workspace.context_pane = Some(custom_pane.clone());
    let custom_labels = workspace.context_menu_labels();
    assert!(
        custom_labels.iter().any(|label| label.contains("Merge")
            && label.contains("Chart")
            && label.contains("kernel:rsi")),
        "custom offers a chart merge naming the pane: {custom_labels:?}"
    );
    assert!(
        custom_labels.iter().any(|label| label.contains("Merge")
            && label.contains("Oscillator")
            && label.contains("kernel:rsi")),
        "custom offers an oscillator merge naming the pane: {custom_labels:?}"
    );
}

#[test]
fn invoking_merge_to_chart_prunes_the_custom_pane() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("macd").expect("macd slots");
    workspace.recompute().expect("evaluations build");
    workspace.context_pane = Some(ContextPane::Custom("kernel:macd".to_owned()));
    workspace
        .apply_context_action(ChartContextAction::MergeToChart)
        .expect("merge to chart applies");
    assert_eq!(
        workspace.pane_target("macd"),
        PaneTarget::Main,
        "the row writes the routing table"
    );
    workspace
        .rebuild_derived_panes()
        .expect("derived rebuild prunes");
    assert!(
        !workspace
            .custom_pane_ids()
            .contains(&"kernel:macd".to_owned()),
        "the custom pane disappears on the next derived rebuild"
    );
}

#[test]
fn oscillator_merge_row_only_offered_for_mergeable_kernels() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("macd").expect("macd slots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");

    workspace.context_pane = Some(ContextPane::Custom("kernel:macd".to_owned()));
    let macd_labels = workspace.context_menu_labels();
    assert!(
        macd_labels
            .iter()
            .any(|label| label.contains("Chart") && label.contains("kernel:macd")),
        "unmapped kernels still offer the chart merge: {macd_labels:?}"
    );
    assert!(
        !macd_labels.iter().any(|label| label.contains("Oscillator")),
        "unmapped kernels offer no oscillator merge: {macd_labels:?}"
    );
    let refused = workspace
        .apply_context_action(ChartContextAction::MergeToOscillator)
        .expect_err("the refused merge fails closed");
    assert!(
        refused.to_string().contains("macd"),
        "the refusal names the plugin: {refused}"
    );

    workspace.context_pane = Some(ContextPane::Custom("kernel:rsi".to_owned()));
    let rsi_labels = workspace.context_menu_labels();
    assert!(
        rsi_labels
            .iter()
            .any(|label| label.contains("Oscillator") && label.contains("kernel:rsi")),
        "mapped oscillators offer the oscillator merge: {rsi_labels:?}"
    );
}

#[test]
fn custom_panes_lock_to_the_main_time_axis_on_interaction() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("macd").expect("macd slots");
    workspace.recompute().expect("evaluations build");
    assert!(
        !workspace.sub_panes.is_empty(),
        "the fixture owns a custom pane"
    );
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::Pan2D {
            dx_px: 30.0,
            dy_px: -6.0,
        }],
    );
    workspace.step_interaction(
        1.0 / 60.0,
        &[PointerInput::WheelTime {
            delta_px: 40.0,
            anchor_x: 160.0,
        }],
    );
    let main = workspace.main_viewport().expect("main viewport");
    for pane in &workspace.sub_panes {
        assert!(
            (pane.viewport.x_offset_px() - main.x_offset_px()).abs() <= f32::EPSILON,
            "a custom pane pans with the main chart area"
        );
        assert!(
            (pane.viewport.scale().bar_spacing() - main.scale().bar_spacing()).abs()
                <= f32::EPSILON,
            "a custom pane zooms with the main chart area"
        );
    }
}
