//! The workspace seeds and paints headlessly without a gateway, and
//! redraws chart panes and replay state over a root-backed gateway.
//!
//! The store-backed switch E2E lives in the integration-tier
//! `workspace_store` target; the gateway-less control here stays in the
//! fast gate.

use std::path::PathBuf;

use super::*;
use chrono::DateTime;
use protopyne_chart_model::ChartRow;
use protopyne_server::{serve_with_tape, ServerConfig};
use protopyne_types::Bar;

const BASE_MS: i64 = 1_767_974_400_000;

/// A flat 15-minute bar series on the fixed stamp grid: contiguous, whole,
/// and two-decimal, so the tape stores every row. `count` bars at `price`,
/// so boot history and tape can hold visibly different series.
fn flat_bars(count: i64, price: &str) -> Vec<Bar> {
    (0..count)
        .map(|step| {
            let price = price.parse().expect("decimal");
            Bar::new()
                .ts(DateTime::from_timestamp_millis(BASE_MS + step * 900_000).expect("in range"))
                .o(price)
                .h(price)
                .l(price)
                .c(price)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// The boot history the server is handed: twelve flat bars at 100.
fn boot_bars() -> Vec<Bar> {
    flat_bars(12, "100")
}

fn sample_rows() -> Vec<ChartRow> {
    (0..8i64)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: 1_700_000_000 + i * 900,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 1_000 + i as u64,
            }
        })
        .collect()
}

#[test]
fn a_gateway_less_workspace_seeds_and_paints_unchanged() {
    let rows = sample_rows();
    let mut workspace = ChartWorkspace::with_rows(rows.clone()).expect("the workspace seeds");
    assert_eq!(
        workspace.rows().len(),
        rows.len(),
        "the seeded rows are the workspace's"
    );
    egui::__run_test_ui(|ui| {
        paint_workspace(ui, &mut workspace, None).expect("the workspace paints without a gateway");
    });
}

/// End-to-end over the shipped data root without a tape: the boot history
/// is SPY, the root carries GLD months, and picking GLD must redraw the
/// chart, the oscillator panes, and the replay state — never strand the
/// SPY tape under a GLD label.
#[tokio::test]
async fn root_backed_switch_redraws_chart_panes_and_replay() {
    use protopyne_kernels::ManifestFile;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical");
    if !root.join("GLD").is_dir() {
        eprintln!("shipped root has no GLD tree; skipping");
        return;
    }
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_path(root.to_string_lossy().into_owned())
        .history_symbol("SPY".to_owned())
        .build();
    let handle = serve_with_tape(config, boot_bars(), None, None)
        .await
        .expect("server boots over the shipped root");
    let gateway = handle.gateway();

    let mut workspace = ChartWorkspace::new().expect("the workspace boots");
    workspace
        .switch_timeframe(TimeframeKey::M15, &gateway)
        .expect("the boot market selects");
    workspace.slot_indicator("rsi").expect("rsi slots");
    let indicators = vec![ManifestFile {
        relpath: "indicators/_Scope.pine".to_owned(),
        text: "indicator(\"Scope\", overlay=false)\nplot(close, \"C\")\n".to_owned(),
    }];
    workspace.set_pine_sources(indicators, Vec::new());
    workspace.slot_indicator("pine:_Scope").expect("pine slots");
    // A picked replay window is live before the switch: the switch must
    // unset it, never carry SPY's boundaries onto GLD.
    let rows = workspace.rows().to_vec();
    workspace.replay_range = Some((rows[0].time, rows[1].time));
    workspace
        .switch_symbol("GLD", &gateway)
        .expect("a root-backed switch pulls GLD");
    assert_eq!(
        workspace.active_symbol(),
        "GLD",
        "the ticker button follows"
    );
    assert!(
        !workspace.rows().is_empty(),
        "GLD months load rows of their own"
    );
    assert!(
        workspace
            .rows()
            .iter()
            .all(|row| (row.close - 100.0).abs() > 1e-6),
        "no SPY boot bar survives under the GLD label"
    );
    for id in ["rsi", "pine:_Scope"] {
        let evaluation = workspace
            .evaluations
            .get(id)
            .unwrap_or_else(|| panic!("{id} evaluated after the switch"));
        assert!(
            !evaluation.series.is_empty(),
            "{id} paints after the switch"
        );
        assert_eq!(
            evaluation.series[0].values.len(),
            workspace.rows().len(),
            "{id} tracks the GLD rows"
        );
    }
    assert_eq!(workspace.replay_range, None, "the picked window unsets");
    assert!(
        workspace.replay_error.is_some(),
        "the unset names itself on the replay section"
    );
    egui::__run_test_ui(|ui| {
        paint_workspace(ui, &mut workspace, Some(&gateway)).expect("the workspace paints");
    });
    handle.shutdown().await;
}
