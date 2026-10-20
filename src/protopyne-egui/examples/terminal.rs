//! Composition root: the trading terminal over `&dyn TerminalGateway`.
//!
//! This example is the only place that names the trading concretion: it
//! boots a loopback [`ServerGateway`](protopyne_server::ServerGateway) from
//! CSV history (or holds a [`MockGateway`](protopyne_view::MockGateway) with
//! `--mock`) and passes it into the terminal as `&dyn TerminalGateway`.
//! Render modules under `src/` never name the server — the ledger pins that
//! direction, and this file is the justification for the optional
//! `server-gateway` dependency.
//!
//! The window is a text dashboard over the gateway views (health and
//! command-plane status, portfolio, proposals, chart-bar range, one
//! overlay) plus the replay scrub; chart painting stays in the chart-shell
//! (`demo`) example, which this file does not duplicate. No decision logic lives here: every
//! number on screen is a gateway read, and the only write (`arm`) is the
//! form's submit.
//!
//! Usage: `cargo run -p protopyne-egui --features server-gateway,demo
//! --example terminal -- [--mock] [--base <label>] [DIR|HISTORY.csv|HISTORY.parquet]`,
//! or pass `--check` to pull every view through the trait object without
//! opening a window. `--base` declares the history's bar spacing for the
//! gateway fold (default `15M` for files, `1M` for history-root directories);
//! it is never inferred from the filename. `--mock --check` output is pinned
//! in `tests/data/terminal_mock_check.txt`: the `protopyne-demo` make target
//! reproduces it byte-for-byte.

use std::path::PathBuf;

use common_core::config::load_json_or_default;
use common_core::io::write_atomic;
use fluent_wvr::Describable;
use protopyne_egui::views::{load_bundle, HealthModel, PortfolioModel, ViewsBundle};
use protopyne_egui::{
    live_update_of_frame, paint_workspace, prefs_path, ChartWorkspace, ReplayScrub, TerminalPrefs,
};
use protopyne_server::{serve, ServerConfig, ServerHandle};
use protopyne_view::{ChartBars, GatewayError, MockGateway, OverlaySeries, TerminalGateway};

/// Overlay shown on the dashboard: same kernel and length the parity test
/// pins, so the example and the suite read the same view.
const OVERLAY_KIND: &str = "ema";
const OVERLAY_PERIOD: i64 = 10;

fn history_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical")
}

/// Default boot history and the bar spacing its file holds: the one-minute
/// SPY trim, so the default `5M` timeframe is served natively (folded up from
/// one-minute) and every other key — including `1M` — remains reachable.
fn default_history() -> (PathBuf, &'static str) {
    (history_dir().join("SPY_1M.csv"), "1M")
}

/// Chart reads are gateway DTOs with no display view-model: the dashboard
/// renders their own range and overlay counts, so they ride alongside the
/// bundle rather than earning a model of their own. The terminal pulls them
/// through the trait object, never by reaching past it.
struct ChartReads {
    bars: ChartBars,
    overlay: OverlaySeries,
}

fn pull_charts(gateway: &dyn TerminalGateway) -> Result<ChartReads, GatewayError> {
    Ok(ChartReads {
        bars: gateway.chart_bars()?,
        overlay: gateway.chart_overlays(OVERLAY_KIND, OVERLAY_PERIOD)?,
    })
}

/// One-line health rendering shared by the headless check and the window:
/// the same gateway read renders the same line in both, so the two cannot
/// drift into disagreeing dashboards.
fn health_summary_line(health: &HealthModel) -> String {
    format!(
        "status={} version={} trading={} entries={} exits={}",
        health.status,
        health.version,
        health.trading_state,
        health.allows_entries,
        health.allows_exits,
    )
}

/// One-line fill accounting shared by the headless check and the window,
/// same single-source rule as [`health_summary_line`].
fn portfolio_summary_line(portfolio: &PortfolioModel) -> String {
    format!(
        "filled={} fees={} open={}",
        portfolio.filled_orders, portfolio.total_fees, portfolio.open_contracts,
    )
}

fn check_lines(bundle: &ViewsBundle, charts: &ChartReads, replay: &ReplayScrub) -> Vec<String> {
    let portfolio = &bundle.portfolio;
    let bars = &charts.bars;
    let last_close = bars.rows.last().map_or(f64::NAN, |bar| bar.close);
    let overlay_last = charts.overlay.values.iter().rev().find_map(|slot| *slot);
    vec![
        health_summary_line(&bundle.health),
        format!(
            "proposals={} positions={} {} journal={}",
            bundle.proposals.len(),
            portfolio.positions.len(),
            portfolio_summary_line(portfolio),
            bundle.journal.len(),
        ),
        format!(
            "bars={} dropped={} last_close={last_close:.2} overlay={}/{}(last={overlay_last:.2?})",
            bars.rows.len(),
            bars.dropped,
            OVERLAY_KIND,
            OVERLAY_PERIOD,
        ),
        format!(
            "replay bars={} cursor={} anchors={}",
            replay.state().total_bars,
            replay.state().cursor,
            replay.window().map_or(0, |window| window
                .steps
                .iter()
                .filter(|step| step.checkpoint)
                .count()),
        ),
    ]
}

async fn boot(path: PathBuf, base: Option<String>) -> Result<ServerHandle, String> {
    let bars = protopyne_server::load_history_path(&path, None)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    if bars.is_empty() {
        return Err(format!("no bars in {}", path.display()));
    }
    // Directories hold one-minute months by contract; files keep the legacy
    // fifteen-minute default unless `--base` says otherwise.
    let base = base.unwrap_or_else(|| {
        if path.is_dir() {
            "1M".to_owned()
        } else {
            "15M".to_owned()
        }
    });
    let config = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_timeframe(base)
        .build();
    serve(config, bars)
        .await
        .map_err(|e| format!("server refused to boot: {e}"))
}

struct TerminalApp {
    gateway: Box<dyn TerminalGateway>,
    /// The rehearsal feed, drained once per frame into the workspace: the
    /// pump ticks behind the window, so the drain never blocks and a lagged
    /// channel just repaints. Absent on `--mock`, which has no server.
    feed: Option<tokio::sync::broadcast::Receiver<String>>,
    /// The shared chart workspace, booted over the same gateway: Material top
    /// bar, floating left-boundary dock drawer (gateway terminal, health,
    /// portfolio, proposals, chart, journals, arm-grant, replay), canvas with
    /// the upper-left hotlist, and the symbol/indicator modals.
    workspace: Result<ChartWorkspace, String>,
    /// Where the favorites/loadout/settings document lives.
    prefs_path: PathBuf,
    /// Last persisted snapshot, so the frame writes only on a real change.
    saved: TerminalPrefs,
}

impl TerminalApp {
    fn new(
        gateway: Box<dyn TerminalGateway>,
        feed: Option<tokio::sync::broadcast::Receiver<String>>,
        history: &std::path::Path,
    ) -> Self {
        let path = prefs_path(history);
        let prefs = load_json_or_default::<TerminalPrefs>(&path);
        let workspace = ChartWorkspace::with_gateway(&*gateway)
            .map(|mut workspace| {
                // The composition root owns the descriptor; the workspace owns
                // the form. Until set, the dock's arm-grant subpane shows
                // read-only tier texts.
                workspace.set_grant_schema(&protopyne_strategy::GrantPatch::default().describe());
                // Restore the persisted selection over the boot history; a
                // persisted timeframe switches the live read.
                let symbol = prefs.symbol.clone();
                workspace.apply_prefs(&prefs).ok();
                if let Some(symbol) = symbol.filter(|s| !s.trim().is_empty()) {
                    let _ = workspace.switch_symbol(&symbol, &*gateway);
                }
                if let Some(label) = prefs.timeframe.as_deref() {
                    if let Some(key) = protopyne_egui::TimeframeKey::all()
                        .into_iter()
                        .find(|key| key.label() == label)
                    {
                        let _ = workspace.switch_timeframe(key, &*gateway);
                    }
                }
                // Dock data (journal/portfolio) and the replay window stream on
                // demand when the dock opens; boot pays for the chart alone.
                workspace
            })
            .map_err(|e| e.to_string());
        Self {
            gateway,
            feed,
            workspace,
            prefs_path: path,
            saved: prefs,
        }
    }
}

impl eframe::App for TerminalApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // One live drain per frame: the rehearsal frames the server already
        // pumps become closed-bar updates through the workspace's own live
        // path, so the window chart advances exactly like a fed terminal.
        if let (Ok(workspace), Some(rx)) = (&mut self.workspace, &mut self.feed) {
            while let Ok(frame) = rx.try_recv() {
                match live_update_of_frame(&frame) {
                    Ok(update) => {
                        if workspace.apply_live_update(update).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        // The dock owns every gateway view now: terminal, health, portfolio,
        // proposals, chart, journals, arm-grant, replay. No second panel tree
        // lives here — the workspace is the single terminal truth.
        match &mut self.workspace {
            Ok(workspace) => {
                if let Err(error) = paint_workspace(ui, workspace, Some(&*self.gateway)) {
                    ui.colored_label(egui::Color32::RED, format!("workspace refused: {error}"));
                }
                let prefs = workspace.prefs();
                if prefs != self.saved {
                    if let Ok(bytes) = serde_json::to_vec_pretty(&prefs) {
                        let _ = write_atomic(&self.prefs_path, &bytes);
                    }
                    self.saved = prefs;
                }
            }
            Err(reason) => {
                ui.colored_label(egui::Color32::RED, format!("workspace refused: {reason}"));
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let mut path: Option<PathBuf> = None;
    let mut check = false;
    let mut mock = false;
    let mut base: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--mock" => mock = true,
            "--base" => {
                base = Some(args.next().unwrap_or_else(|| {
                    eprintln!("protopyne-terminal: --base needs a label");
                    std::process::exit(1);
                }));
            }
            _ => path = Some(PathBuf::from(arg)),
        }
    }
    let (history_path, default_base) = if let Some(path) = path {
        (path, None)
    } else {
        let (path, base) = default_history();
        (path, Some(base.to_owned()))
    };
    let base = base.or(default_base);
    let prefs_source = history_path.clone();
    let handle: Option<ServerHandle> = if mock {
        None
    } else {
        match boot(history_path, base).await {
            Ok(handle) => Some(handle),
            Err(e) => {
                eprintln!("protopyne-terminal: {e}");
                std::process::exit(1);
            }
        }
    };
    let gateway: Box<dyn TerminalGateway> = match &handle {
        Some(handle) => Box::new(handle.gateway()),
        None => Box::new(MockGateway::new()),
    };
    if check {
        // The scrub reads its window from the same trait object, so `--check`
        // proves the replay seam without opening a window.
        let scrub: Result<ReplayScrub, String> = ReplayScrub::standard()
            .map_err(|e| e.to_string())
            .and_then(|mut scrub| {
                scrub
                    .ingest(&*gateway)
                    .map(|()| scrub)
                    .map_err(|e| e.to_string())
            });
        match ChartWorkspace::with_gateway(&*gateway) {
            Ok(workspace) => println!(
                "protopyne-terminal check: workspace symbol={} timeframe={} bars={} slots=[{}] cached={}",
                workspace.active_symbol(),
                workspace.timeframe().label(),
                workspace.rows().len(),
                workspace.slot_ids().join(","),
                workspace.cached_series(),
            ),
            Err(e) => {
                eprintln!("protopyne-terminal: workspace refused: {e}");
                std::process::exit(1);
            }
        }
        match (load_bundle(&*gateway), pull_charts(&*gateway), scrub) {
            (Ok(bundle), Ok(charts), Ok(replay)) => {
                for line in check_lines(&bundle, &charts, &replay) {
                    println!("protopyne-terminal check: {line}");
                }
            }
            (Err(e), _, _) | (_, Err(e), _) => {
                eprintln!("protopyne-terminal: gateway refused: {e}");
                std::process::exit(1);
            }
            (_, _, Err(e)) => {
                eprintln!("protopyne-terminal: replay refused: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let feed = handle.as_ref().map(ServerHandle::feed_receiver);
        let app = TerminalApp::new(gateway, feed, &prefs_source);
        let run = eframe::run_native(
            "protopyne gateway terminal",
            eframe::NativeOptions::default(),
            Box::new(|_cc| Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Box::new(app))),
        );
        if let Err(e) = run {
            eprintln!("protopyne-terminal: window refused: {e}");
            std::process::exit(1);
        }
    }
    if let Some(handle) = handle {
        handle.shutdown().await;
    }
}
