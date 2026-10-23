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
fn merging_to_chart_keeps_computation_identical() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("macd").expect("macd slots");
    workspace.recompute().expect("evaluations build");
    let plan_before = workspace.plan_order().expect("plan orders");
    let keys_before: Vec<u64> = workspace
        .evaluations
        .get("macd")
        .expect("macd evaluates")
        .series
        .iter()
        .map(|series| series.key)
        .collect();
    let nodes_before: Vec<String> = workspace
        .evaluations
        .get("macd")
        .expect("macd evaluates")
        .series
        .iter()
        .map(|series| series.node.clone())
        .collect();
    let catalog_before: Vec<Option<u64>> = nodes_before
        .iter()
        .map(|node| workspace.series_catalog.series_key(node))
        .collect();

    workspace
        .set_pane_target("macd", PaneTarget::Main)
        .expect("merging to chart succeeds");
    workspace
        .rebuild_derived_panes()
        .expect("derived rebuild prunes");

    assert!(
        !workspace
            .custom_pane_ids()
            .contains(&"kernel:macd".to_owned()),
        "the merged pane no longer derives"
    );
    let evaluation = workspace.evaluations.get("macd").expect("macd evaluates");
    let for_custom = workspace.evaluation_for_pane("macd", evaluation, "kernel:macd");
    assert!(
        for_custom.series.is_empty(),
        "the custom pane slice is empty after the merge"
    );
    let for_main = workspace.evaluation_for_pane("macd", evaluation, "main");
    assert_eq!(
        for_main.series.len(),
        evaluation.series.len(),
        "the chart slice carries every series"
    );
    assert_eq!(
        workspace.plan_order().expect("plan orders"),
        plan_before,
        "plan nodes never move on merge"
    );
    let keys_after: Vec<u64> = workspace
        .evaluations
        .get("macd")
        .expect("macd evaluates")
        .series
        .iter()
        .map(|series| series.key)
        .collect();
    assert_eq!(keys_after, keys_before, "cache keys never move on merge");
    let catalog_after: Vec<Option<u64>> = nodes_before
        .iter()
        .map(|node| workspace.series_catalog.series_key(node))
        .collect();
    assert_eq!(
        catalog_after, catalog_before,
        "catalog entries never move on merge"
    );
}

#[test]
fn removal_clears_the_routing_row() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(60)).expect("workspace boots");
    workspace.slot_indicator("macd").expect("macd slots");
    workspace.recompute().expect("evaluations build");
    workspace
        .set_pane_target("macd", PaneTarget::Main)
        .expect("merging to chart succeeds");
    assert_eq!(
        workspace.pane_target("macd"),
        PaneTarget::Main,
        "the row is live"
    );
    workspace.remove_indicator("macd").expect("removal unslots");
    assert_eq!(
        workspace.pane_target("macd"),
        PaneTarget::Default,
        "a removed slot reads as default, never as an error"
    );
    workspace.slot_indicator("macd").expect("macd re-slots");
    workspace.recompute().expect("evaluations build");
    assert_eq!(
        workspace.pane_target("macd"),
        PaneTarget::Default,
        "a re-slotted indicator starts unmerged"
    );
}

#[test]
fn merging_a_band_into_the_shared_pane_refuses_naming_the_plugin() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("bbands").expect("bbands slots");
    workspace.recompute().expect("evaluations build");
    let refused = workspace.set_pane_target("bbands", PaneTarget::SharedOscillator);
    let error = refused.expect_err("a band cannot merge into the shared oscillator");
    let message = format!("{error:?}");
    assert!(
        message.contains("bbands"),
        "the refusal names the plugin: {message}"
    );
}
