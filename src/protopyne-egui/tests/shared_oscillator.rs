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

fn shared_range(workspace: &ChartWorkspace) -> (f64, f64) {
    let sub = workspace.sub.expect("shared viewport is bound");
    let range = sub.scale().range();
    (range.min, range.max)
}

fn merge_to_shared(workspace: &mut ChartWorkspace, slot: &str) {
    workspace
        .set_pane_target(slot, PaneTarget::SharedOscillator)
        .expect("mapped oscillator merges");
}

fn set_threshold(workspace: &mut ChartWorkspace, slot: &str, threshold: &str) {
    workspace
        .stack
        .set_slot_field(slot, "osc_threshold", threshold)
        .expect("threshold edits");
}

#[test]
fn shared_pane_auto_ranges_symmetrically_over_normalized_content() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    merge_to_shared(&mut workspace, "rsi");
    workspace.recompute().expect("evaluations build");
    let (lo, hi) = shared_range(&workspace);
    assert!(
        (lo + hi).abs() < 1e-9,
        "the shared range stays symmetric, got ({lo}, {hi})"
    );
    assert!(
        lo <= -100.0 && hi >= 100.0,
        "the shared range contains plus/minus threshold, got ({lo}, {hi})"
    );
    assert!(
        hi > 100.0 && lo < -100.0 && hi.is_finite() && lo.is_finite(),
        "padding extends past the threshold with finite bounds, got ({lo}, {hi})"
    );
    assert!(
        (lo, hi) != (-110.0, 110.0),
        "normalized content leaves the fixed fallback behind"
    );
}

#[test]
fn shared_pane_range_covers_the_union_of_slot_thresholds() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.slot_indicator("cci").expect("cci slots");
    set_threshold(&mut workspace, "rsi", "100");
    set_threshold(&mut workspace, "cci", "150");
    merge_to_shared(&mut workspace, "rsi");
    merge_to_shared(&mut workspace, "cci");
    workspace.recompute().expect("evaluations build");
    let (lo, hi) = shared_range(&workspace);
    assert!(
        lo <= -150.0 && hi >= 150.0,
        "the shared range contains both thresholds, got ({lo}, {hi})"
    );
    assert!(
        (lo + hi).abs() < 1e-9,
        "the union range stays symmetric, got ({lo}, {hi})"
    );
}

#[test]
fn shared_pane_without_normalized_content_falls_back_to_fixed_band() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(60)).expect("workspace boots");
    workspace.recompute().expect("evaluations build");
    assert_eq!(
        shared_range(&workspace),
        (-110.0, 110.0),
        "no normalized content keeps the honest fallback exactly"
    );
}
