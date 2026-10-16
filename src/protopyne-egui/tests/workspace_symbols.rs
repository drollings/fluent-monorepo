//! Gateway-less symbol switches: the demo shell's on-demand path.
//!
//! `switch_symbol_seeded` records the picked ticker, replaces every
//! timeframe slot, and binds the active timeframe's rows — so a symbol pick
//! never paints another ticker's rows, and an unloadable ticker paints its
//! own label over empty rows rather than stale ones.

use super::*;
use protopyne_chart_model::ChartRow;

fn grid_rows(start: i64, step: i64, count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: start + i as i64 * step,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 1_000 + i as u64,
            }
        })
        .collect()
}

fn all_frames(step: i64, count: usize) -> Vec<(TimeframeKey, Vec<ChartRow>)> {
    TimeframeKey::all()
        .into_iter()
        .enumerate()
        .map(|(slot, key)| {
            (
                key,
                grid_rows(1_700_000_000 + slot as i64 * 10_000, step, count),
            )
        })
        .collect()
}

#[test]
fn seeded_switch_records_the_ticker_and_its_rows() {
    let mut workspace = ChartWorkspace::new().expect("workspace boots");
    assert_eq!(
        workspace.active_symbol(),
        "GLD",
        "the default ticker labels first"
    );
    workspace
        .switch_symbol_seeded("SPY", all_frames(60, 24))
        .expect("SPY seeds");
    assert_eq!(workspace.active_symbol(), "SPY");
    assert_eq!(workspace.rows().len(), 24, "the active key's rows bind");
    assert_eq!(workspace.loaded_timeframes().len(), 6, "every key seeds");
}

#[test]
fn seeded_switch_replaces_rows_on_a_second_pick() {
    let mut workspace = ChartWorkspace::new().expect("workspace boots");
    workspace
        .switch_symbol_seeded("SPY", all_frames(60, 24))
        .expect("SPY seeds");
    workspace
        .switch_symbol_seeded("QQQ", all_frames(300, 9))
        .expect("QQQ seeds");
    assert_eq!(workspace.active_symbol(), "QQQ");
    // The active timeframe is still M15, whose slot now holds QQQ's rows.
    assert_eq!(workspace.rows().len(), 9, "no SPY row survives the switch");
}

#[test]
fn seeded_switch_to_empty_paints_the_label_over_no_rows() {
    let mut workspace = ChartWorkspace::new().expect("workspace boots");
    workspace
        .switch_symbol_seeded("SPY", all_frames(60, 24))
        .expect("SPY seeds");
    let empty: Vec<(TimeframeKey, Vec<ChartRow>)> = TimeframeKey::all()
        .into_iter()
        .map(|key| (key, Vec::new()))
        .collect();
    workspace
        .switch_symbol_seeded("NOPE", empty)
        .expect("empty seeds");
    assert_eq!(workspace.active_symbol(), "NOPE");
    assert!(
        workspace.rows().is_empty(),
        "nothing paints under the fresh label"
    );
}

#[test]
fn seeded_switch_without_the_active_key_clears_stale_rows() {
    let mut workspace = ChartWorkspace::new().expect("workspace boots");
    workspace
        .switch_symbol_seeded("SPY", all_frames(60, 24))
        .expect("SPY seeds");
    // No M15 slot and no resample base: the select fails, and the rows must
    // clear rather than linger under the fresh label.
    let err = workspace
        .switch_symbol_seeded(
            "QQQ",
            vec![(TimeframeKey::M1, grid_rows(1_700_000_000, 60, 5))],
        )
        .expect_err("the active key is missing");
    assert!(err.to_string().contains("no loaded history"), "{err}");
    assert_eq!(workspace.active_symbol(), "QQQ");
    assert!(
        workspace.rows().is_empty(),
        "stale SPY rows never survive a failed switch"
    );
}
