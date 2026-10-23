use super::*;
use crate::oscillators::{normalize_series, OscillatorKind, WhichLeg};

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

fn evaluated_display(
    workspace: &ChartWorkspace,
    slot: &str,
) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
    let evaluation = workspace.evaluations.get(slot).expect("slot evaluates");
    let series = evaluation.series.first().expect("one series");
    (series.values.clone(), series.display_values.clone())
}

#[test]
fn mapped_oscillators_paint_normalized_display() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    for (slot, kind) in [("rsi", OscillatorKind::Rsi), ("cci", OscillatorKind::Cci)] {
        workspace.slot_indicator(slot).expect("kernel slots");
        workspace.recompute().expect("evaluations build");
        let (raw, display) = evaluated_display(&workspace, slot);
        let expected = normalize_series(&raw, kind).expect("kind evaluates");
        assert_eq!(
            display, expected,
            "{slot} display equals the catalog squash of its raw leg"
        );
        assert_ne!(raw, display, "{slot} display differs from raw on a ramp");
        workspace.remove_indicator(slot).expect("slot removes");
        workspace.recompute().expect("evaluations build");
    }
}

#[test]
fn mapped_series_publish_raw_under_unchanged_keys() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");
    let evaluation = workspace.evaluations.get("rsi").expect("rsi evaluates");
    let series = evaluation.series.first().expect("one series");
    let node = series.node.clone();
    let key = series.key;
    let catalog_values = workspace
        .series_catalog
        .resolve(&node)
        .expect("node publishes")
        .to_vec();
    assert_eq!(
        catalog_values, series.values,
        "the catalog carries raw values under the unchanged node"
    );
    assert_eq!(
        workspace.series_catalog.series_key(&node),
        Some(key),
        "the catalog key matches the stored series key"
    );
    // A consumer reading series:<node> sees raw, never display.
    let consumer = workspace
        .series_catalog
        .resolve(&node)
        .expect("consumer resolves");
    assert_eq!(
        consumer.to_vec(),
        series.values,
        "a series:<node> consumer resolves raw"
    );
}

#[test]
fn warmup_holes_stay_holes_through_the_mapping() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(30)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");
    let evaluation = workspace.evaluations.get("rsi").expect("rsi evaluates");
    let series = evaluation.series.first().expect("one series");
    assert!(
        series.values.first().is_some_and(Option::is_none),
        "raw warms up with a hole"
    );
    for (raw, display) in series.values.iter().zip(series.display_values.iter()) {
        if raw.is_none() {
            assert!(
                display.is_none(),
                "warmup holes stay holes through the mapping"
            );
        }
    }
    assert_eq!(
        series.values.len(),
        series.display_values.len(),
        "display preserves length"
    );
    let _ = WhichLeg::Slow;
}
