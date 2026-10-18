use super::*;
use protopyne_chart_model::heikin_rows_of;

fn trend_rows(count: i64) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + (i as f64);
            ChartRow {
                time: 1_700_000_000 + i * 900,
                open: close - 1.0,
                high: close + 1.0,
                low: close - 2.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

#[test]
fn heikin_cache_covers_full_history_once() {
    let rows = trend_rows(40);
    let workspace = ChartWorkspace::with_rows(rows.clone()).expect("workspace boots");
    assert_eq!(
        workspace.heikin,
        heikin_rows_of(&rows),
        "the cache mirrors the full history"
    );
    assert_eq!(
        workspace.heikin.len(),
        rows.len(),
        "no bar is windowed out of the cache"
    );
}

#[test]
fn heikin_cache_invalidates_on_rows_shape_change() {
    let mut workspace = ChartWorkspace::with_rows(trend_rows(40)).expect("workspace boots");
    let grown = trend_rows(41);
    workspace.set_rows(grown.clone()).expect("rows reseed");
    assert_eq!(
        workspace.heikin,
        heikin_rows_of(&grown),
        "a new shape rebuilds the cache"
    );
    workspace.set_rows(Vec::new()).expect("empty rows reseed");
    assert!(
        workspace.heikin.is_empty(),
        "empty history clears the cache"
    );
}
