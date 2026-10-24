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

#[test]
fn moving_averages_paint_on_the_chart_without_a_pane() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    for id in ["sma", "hma", "ema"] {
        workspace.slot_indicator(id).expect("average slots");
    }
    workspace.recompute().expect("evaluations build");
    assert!(
        workspace.custom_pane_ids().is_empty(),
        "moving averages own no pane: {:?}",
        workspace.custom_pane_ids()
    );
    assert!(
        workspace.sub_panes.is_empty(),
        "no average binds a viewport"
    );
    for id in ["sma", "hma", "ema"] {
        let evaluation = workspace.evaluations.get(id).expect("slot evaluates");
        assert!(
            evaluation.series.iter().all(|series| series.attachment
                == fluent_egui::SlotAttachment::MainOverlay
                && series.pane.is_none()),
            "{id} paints on the chart with no pane"
        );
    }
}

#[test]
fn removed_multi_output_plugin_prunes_its_pane_without_recompute() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("macd").expect("macd slots");
    workspace.recompute().expect("evaluations build");
    let ids = workspace.custom_pane_ids();
    assert!(
        ids.contains(&"kernel:macd".to_owned()),
        "multi-output slot publishes its pane: {ids:?}"
    );
    assert!(
        workspace
            .sub_panes
            .iter()
            .any(|pane| pane.id == "kernel:macd"),
        "its viewport is bound"
    );
    let builds = workspace.derived_builds();
    workspace.remove_indicator("macd").expect("removal unslots");
    assert!(
        !workspace
            .custom_pane_ids()
            .contains(&"kernel:macd".to_owned()),
        "the pane id is gone"
    );
    assert!(
        !workspace
            .sub_panes
            .iter()
            .any(|pane| pane.id == "kernel:macd"),
        "its viewport is gone with no intervening recompute"
    );
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "the removal itself counts the derived build"
    );
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "the post-removal replay holds the gate"
    );
}

#[test]
fn removing_one_shared_plugin_keeps_the_shared_pane() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(60)).expect("workspace boots");
    assert!(workspace.sub.is_some(), "the shared pane is bound at boot");
    workspace
        .remove_indicator("fast_mas")
        .expect("overlay removes");
    assert!(
        workspace.sub.is_some(),
        "removing an overlay never nukes the shared pane"
    );
    workspace
        .remove_indicator("integration_lite")
        .expect("lite removes");
    assert!(
        workspace.sub.is_some(),
        "removing the lite slot keeps the shared viewport bound"
    );
}

#[test]
fn removing_the_lite_slot_clears_its_derived_state() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(60)).expect("workspace boots");
    assert!(
        workspace.sub_pane.is_some(),
        "the lite pane is live at boot"
    );
    assert!(!workspace.momentum_keys.is_empty(), "momentum keys live");
    assert!(!workspace.tints.is_empty(), "tints live");
    workspace
        .remove_indicator("integration_lite")
        .expect("lite removes");
    assert!(workspace.sub_pane.is_none(), "the lite pane clears");
    assert!(workspace.momentum_keys.is_empty(), "momentum keys clear");
    assert!(workspace.tints.is_empty(), "tints clear");
}
