//! Gateway-less symbol switches: the demo shell's on-demand path.
//!
//! `switch_symbol_seeded` records the picked ticker, replaces every
//! timeframe slot, and binds the active timeframe's rows — so a symbol pick
//! never paints another ticker's rows, and an unloadable ticker paints its
//! own label over empty rows rather than stale ones.

use super::*;
use protopyne_chart_model::ChartRow;
use protopyne_view::{
    ArmResponse, ChartBars, GatewayError, HealthView, JournalRowView, OverlaySeries, PortfolioView,
    ProposalView, ReplayView, SymbolInfoView, TerminalGateway, WorstCase,
};
use std::collections::BTreeMap;

fn unavailable() -> GatewayError {
    GatewayError::Unavailable("catalog gateway serves bars only".to_owned())
}

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

/// A gateway whose symbol catalog is whatever a history root discovered; it
/// serves bars for any symbol so the workspace can boot over it.
struct RootGateway {
    rows: Vec<SymbolInfoView>,
}

impl RootGateway {
    fn new(rows: Vec<SymbolInfoView>) -> Self {
        Self { rows }
    }
}

fn scripted_bars(count: usize) -> ChartBars {
    let rows = (0..count)
        .map(|i| ChartRow {
            time: 1_700_000_000 + i as i64 * 900,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 10 + i as u64,
        })
        .collect();
    ChartBars { rows, dropped: 0 }
}

impl TerminalGateway for RootGateway {
    fn symbols(&self) -> Result<Vec<SymbolInfoView>, GatewayError> {
        Ok(self.rows.clone())
    }

    fn chart_bars_for(&self, _symbol: &str, _timeframe: &str) -> Result<ChartBars, GatewayError> {
        Ok(scripted_bars(120))
    }

    fn health(&self) -> Result<HealthView, GatewayError> {
        Err(unavailable())
    }

    fn proposals(&self) -> Result<Vec<ProposalView>, GatewayError> {
        Err(unavailable())
    }

    fn portfolio(&self) -> Result<PortfolioView, GatewayError> {
        Err(unavailable())
    }

    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError> {
        Err(unavailable())
    }

    fn chart_bars(&self) -> Result<ChartBars, GatewayError> {
        Err(unavailable())
    }

    fn chart_overlays(&self, _kind: &str, _period: i64) -> Result<OverlaySeries, GatewayError> {
        Err(unavailable())
    }

    fn replay(&self) -> Result<ReplayView, GatewayError> {
        Err(unavailable())
    }

    fn preview_arm(&self, _patch: serde_json::Value) -> Result<WorstCase, GatewayError> {
        Err(unavailable())
    }

    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError> {
        Err(unavailable())
    }

    fn arm(&self, _grant: serde_json::Value) -> Result<ArmResponse, GatewayError> {
        Err(unavailable())
    }
}

#[test]
fn symbol_catalog_builder_refuses_an_empty_row_list() {
    let Err(err) = build_symbol_catalog(&[]) else {
        panic!("an empty row list refuses");
    };
    assert!(        err.to_string().contains("empty"),
        "the refusal names the emptiness: {err}"
    );
}

#[test]
fn symbol_catalog_builder_renders_discovered_rows_sorted_with_unknown_class() {
    let rows = vec![
        SymbolInfoView {
            symbol: "ZZZ".to_owned(),
            name: "ZZZ".to_owned(),
            asset_class: "Unknown".to_owned(),
        },
        SymbolInfoView {
            symbol: "AAA".to_owned(),
            name: "AAA".to_owned(),
            asset_class: "Unknown".to_owned(),
        },
        SymbolInfoView {
            symbol: "SPY".to_owned(),
            name: "SPDR S&P 500 ETF Trust".to_owned(),
            asset_class: "Stocks".to_owned(),
        },
    ];
    let mut catalog = build_symbol_catalog(&rows).expect("the catalog builds");
    let ids: Vec<&str> = catalog
        .filtered()
        .into_iter()
        .map(fluent_egui::CatalogEntry::id)
        .collect();
    assert_eq!(
        ids, ["AAA", "SPY", "ZZZ"],
        "the modal renders rows sorted by ticker"
    );
    catalog.set_category(Some(AssetClass::Unknown));
    let ids: Vec<&str> = catalog
        .filtered()
        .into_iter()
        .map(fluent_egui::CatalogEntry::id)
        .collect();
    assert_eq!(ids, ["AAA", "ZZZ"], "Unknown is a real tab, not a guess");
}

#[test]
fn symbol_modal_follows_the_gateway_catalog_and_never_leaks_the_fallback() {
    // A root holding AAA and ZZZ: the modal shows exactly those tickers.
    let dir = tempfile::tempdir().expect("tempdir");
    for ticker in ["ZZZ", "AAA"] {
        std::fs::create_dir_all(dir.path().join(ticker).join("2026/01"))
            .expect("ticker tree creates");
    }
    let root = protopyne_server::HistoryRoot::open(dir.path()).expect("root opens");
    let gateway = RootGateway::new(root.symbol_rows().expect("rows read"));
    let modal = symbol_modal_for(Some(&gateway)).expect("the modal builds");
    let ids: Vec<&str> = modal
        .filtered()
        .into_iter()
        .map(fluent_egui::CatalogEntry::id)
        .collect();
    assert_eq!(
        ids, ["AAA", "ZZZ"],
        "exactly the discovered tickers, no SPY/GLD unless present"
    );

    // A dot-only root discovers nothing: the modal refuses rather than
    // leaking the shipped fallback in behind an empty discovery.
    let dots = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dots.path().join(".cache")).expect("dot dir creates");
    let root = protopyne_server::HistoryRoot::open(dots.path()).expect("root opens");
    let gateway = RootGateway::new(root.symbol_rows().expect("empty rows"));
    let Err(err) = symbol_modal_for(Some(&gateway)) else {
        panic!("an empty discovery refuses");
    };
    assert!(
        err.to_string().contains("empty"),
        "the refusal names the emptiness: {err}"
    );

    // A gateway-less workspace (the demo shell) serves the shipped offline
    // list — the one documented fallback.
    let modal = symbol_modal_for(None).expect("the offline modal builds");
    assert_eq!(modal.len(), 5, "the shipped offline catalog");
}

#[test]
fn workspace_boot_prefers_the_default_symbol_else_the_first_sorted() {
    let rows = vec![
        SymbolInfoView {
            symbol: "ZZZ".to_owned(),
            name: "ZZZ".to_owned(),
            asset_class: "Unknown".to_owned(),
        },
        SymbolInfoView {
            symbol: "AAA".to_owned(),
            name: "AAA".to_owned(),
            asset_class: "Unknown".to_owned(),
        },
    ];
    let gateway = RootGateway::new(rows);
    let workspace = ChartWorkspace::with_gateway(&gateway).expect("the workspace boots");
    assert_eq!(
        workspace.active_symbol(),
        "AAA",
        "an absent preference falls back to the first ticker in sorted order"
    );
}
