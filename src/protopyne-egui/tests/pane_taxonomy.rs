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
fn mapped_kernels_resolve_to_the_shared_pane_without_a_custom_pane() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.slot_indicator("stoch").expect("stoch slots");
    workspace.slot_indicator("aroon").expect("aroon slots");
    workspace.recompute().expect("evaluations build");
    assert!(
        workspace.custom_pane_ids().is_empty(),
        "normalized oscillators share the sub pane: {:?}",
        workspace.custom_pane_ids()
    );
    assert!(
        workspace.sub_panes.is_empty(),
        "no custom viewport is bound"
    );
    for slot in ["rsi", "stoch", "aroon"] {
        let evaluation = workspace.evaluations.get(slot).expect("slot evaluates");
        assert!(
            evaluation.series.iter().all(|series| series.pane.is_none()),
            "{slot} series carry no pane identity"
        );
        let sliced = workspace.evaluation_for_pane(slot, evaluation, "sub");
        assert_eq!(
            sliced.series.len(),
            evaluation.series.len(),
            "{slot} resolves to the shared pane in full"
        );
    }
}

#[test]
fn unmapped_kernels_keep_their_own_panes() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("macd").expect("macd slots");
    workspace.slot_indicator("mom").expect("mom slots");
    workspace.recompute().expect("evaluations build");
    for pane in ["kernel:macd", "kernel:mom"] {
        assert!(
            workspace.custom_pane_ids().contains(&pane.to_owned()),
            "unmapped kernels keep {pane}: {:?}",
            workspace.custom_pane_ids()
        );
    }
}
