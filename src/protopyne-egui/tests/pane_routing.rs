use super::*;
use crate::oscillators::{DISPLAY_EXTREME_BAND, DISPLAY_RULE_BAND};

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

use crate::oscillators::{normalize_series, OscillatorKind};

#[test]
fn mapped_stoch_legs_paint_normalized_display() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("stoch").expect("stoch slots");
    workspace.recompute().expect("evaluations build");
    let evaluation = workspace.evaluations.get("stoch").expect("stoch evaluates");
    assert_eq!(evaluation.series.len(), 2, "stoch carries both legs");
    for (leg, series) in ["%K", "%D"].iter().zip(evaluation.series.iter()) {
        let catalog =
            normalize_series(&series.values, OscillatorKind::Stoch).expect("kind evaluates");
        let expected: Vec<Option<f64>> = catalog
            .iter()
            .map(|slot| {
                slot.map(|value| {
                    fluent_egui::band_squash(value, DISPLAY_RULE_BAND, DISPLAY_EXTREME_BAND)
                })
            })
            .collect();
        assert_eq!(
            series.display_values, expected,
            "{leg} equals the catalog squash constrained into the extremes band"
        );
    }
    assert_ne!(
        evaluation.series[0].values, evaluation.series[0].display_values,
        "%K display differs from raw on a ramp"
    );
}

#[test]
fn stoch_series_publish_raw_under_unchanged_keys() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("stoch").expect("stoch slots");
    workspace.recompute().expect("evaluations build");
    let evaluation = workspace.evaluations.get("stoch").expect("stoch evaluates");
    for series in &evaluation.series {
        let catalog_values = workspace
            .series_catalog
            .resolve(&series.node)
            .expect("node publishes")
            .to_vec();
        assert_eq!(
            catalog_values, series.values,
            "the catalog carries raw values under {}",
            series.node
        );
        assert_eq!(
            workspace.series_catalog.series_key(&series.node),
            Some(series.key),
            "the catalog key matches the stored series key"
        );
    }
}

#[test]
fn stoch_warmup_holes_stay_holes_through_the_mapping() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(30)).expect("workspace boots");
    workspace.slot_indicator("stoch").expect("stoch slots");
    workspace.recompute().expect("evaluations build");
    let evaluation = workspace.evaluations.get("stoch").expect("stoch evaluates");
    for series in &evaluation.series {
        for (raw, display) in series.values.iter().zip(series.display_values.iter()) {
            if raw.is_none() {
                assert!(display.is_none(), "warmup holes stay holes");
            }
        }
        assert_eq!(
            series.values.len(),
            series.display_values.len(),
            "display preserves length"
        );
    }
}

#[test]
fn mapped_multi_output_merges_into_shared_while_bands_refuse() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(60)).expect("workspace boots");
    workspace.slot_indicator("stoch").expect("stoch slots");
    workspace.slot_indicator("macd").expect("macd slots");
    workspace.recompute().expect("evaluations build");
    workspace
        .set_pane_target("stoch", PaneTarget::SharedOscillator)
        .expect("mapped legs merge into the shared pane");
    assert_eq!(
        workspace.pane_target("stoch"),
        PaneTarget::SharedOscillator,
        "the routing row sticks"
    );
    let refused = workspace
        .set_pane_target("macd", PaneTarget::SharedOscillator)
        .expect_err("bands refuse the shared pane");
    assert!(
        refused.to_string().contains("macd"),
        "the refusal names the plugin: {refused}"
    );
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
    workspace.slot_indicator("bb").expect("bb slots");
    workspace.recompute().expect("evaluations build");
    let refused = workspace.set_pane_target("bb", PaneTarget::SharedOscillator);
    let error = refused.expect_err("a band cannot merge into the shared oscillator");
    assert!(
        matches!(&error, EguiError::InvalidParam { what, .. } if what == "bb"),
        "the refusal names the plugin: {error:?}"
    );
    assert!(
        format!("{error:?}").contains("normalizable"),
        "the refusal carries the reason: {error:?}"
    );
}
