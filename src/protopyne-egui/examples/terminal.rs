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
use protopyne_egui::views::{load_bundle, HealthModel, PortfolioModel, ViewsBundle};
use protopyne_egui::{
    live_update_of_frame, paint_workspace, prefs_path, ChartWorkspace, ReplayScrub, TerminalPrefs,
};
use protopyne_server::{serve, HistoryCache, HistoryRoot, ServerConfig, ServerHandle};
use protopyne_view::{ChartBars, GatewayError, MockGateway, OverlaySeries, TerminalGateway};

#[path = "shared/seed.rs"]
mod seed;

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
    // Starting window only: the server boots on the trailing two months by
    // time (the same span as the chart's default range), never the whole
    // store. Deeper history stays on disk and serves on demand through the
    // gateway's tape/root reads, so load-more and wider presets keep working
    // while startup pays for the window in view. When less exists, it all
    // stays — the cutoff predates the first bar instead of fabricating one.
    let bars = trailing_two_months(bars);
    // Directories hold one-minute months by contract; files keep the legacy
    // fifteen-minute default unless `--base` says otherwise.
    let base = base.unwrap_or_else(|| {
        if path.is_dir() {
            "1M".to_owned()
        } else {
            "15M".to_owned()
        }
    });
    let config_builder = ServerConfig::new()
        .bind("127.0.0.1:0".to_owned())
        .history_timeframe(base)
        .history_symbol(history_symbol_of(&path));
    // Directories that open as a history root ride along as the server's
    // root: ticker picks outside the boot market (GLD beside SPY) load
    // their own months through it instead of refusing. A directory that
    // will not open stays unset — boot serves exactly as before, with the
    // gateway falling back to the recorded market or refusing by symbol.
    let config = if path.is_dir() && protopyne_server::HistoryRoot::open(&path).is_ok() {
        config_builder
            .history_path(path.to_string_lossy().into_owned())
            .build()
    } else {
        config_builder.build()
    };
    serve(config, bars)
        .await
        .map_err(|e| format!("server refused to boot: {e}"))
}

/// Trailing two months of `bars` by timestamp (the chart's default range
/// span, not a hardcoded count, so thin and dense tapes alike keep their
/// most recent two calendar months). Ascending input stays ascending; a
/// history shorter than the span keeps every bar.
fn trailing_two_months(
    bars: Vec<protopyne_types::Bar>,
) -> Vec<protopyne_types::Bar> {
    let Some(last) = bars.last().map(|bar| bar.ts.timestamp()) else {
        return bars;
    };
    let cutoff = last - 2 * protopyne_chart_model::MONTH_SECONDS;
    let skip = bars
        .iter()
        .position(|bar| bar.ts.timestamp() >= cutoff)
        .unwrap_or(bars.len());
    bars.into_iter().skip(skip).collect()
}

/// Ticker the boot bars hold, mirroring `load_history_path`'s own pick so
/// the gateway's fallback gate serves the right market: directories name it
/// through the root's tickers (preferred default first), files carry it as
/// the stem's leading letters (`SPY_15M.csv` → `SPY`). Anything
/// unrecognizable falls back to the default ticker rather than an empty
/// gate (which would refuse every symbol-scoped read).
fn history_symbol_of(path: &std::path::Path) -> String {
    if path.is_dir() {
        if let Ok(root) = protopyne_server::HistoryRoot::open(path) {
            if let Ok(tickers) = root.tickers() {
                if let Some(symbol) = protopyne_view::pick_default_symbol(
                    protopyne_server::DEFAULT_TICKER,
                    tickers.iter().map(String::as_str),
                ) {
                    return symbol;
                }
            }
        }
        return protopyne_server::DEFAULT_TICKER.to_owned();
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| {
            stem.chars()
                .take_while(char::is_ascii_alphabetic)
                .collect::<String>()
                .to_ascii_uppercase()
        })
        .filter(|symbol| !symbol.is_empty())
        .unwrap_or_else(|| protopyne_server::DEFAULT_TICKER.to_owned())
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
    /// Month cache for serverless ticker switches: in `--mock` mode there
    /// is no tape to pull through, so a picked ticker's months load from
    /// the history directory here (exactly like the chart shell) instead
    /// of wearing the mock's canned bars. Absent outside mock-plus-dir
    /// boots, where the gateway path serves every read.
    cache: Option<HistoryCache>,
    /// Ticker whose months the workspace holds via the cache above; `None`
    /// forces a first-frame sync so mock boots open on real months, not
    /// canned rows.
    loaded_symbol: Option<String>,
    /// Load notice for the status line (holes in served months, or the
    /// reason a picked ticker refused).
    notice: Option<seed::Notice>,
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
        mock: bool,
    ) -> Self {
        let path = prefs_path(history);
        let prefs = load_json_or_default::<TerminalPrefs>(&path);
        // Serverless ticker switches need months from disk: in `--mock`
        // mode over a history directory, open the month cache so a picked
        // ticker loads its own market (exactly like the chart shell)
        // instead of wearing the mock's canned bars. Every other mode
        // serves through the gateway and leaves this absent.
        let cache = if mock {
            history
                .is_dir()
                .then(|| HistoryRoot::open(history).ok())
                .flatten()
                .map(HistoryCache::new)
        } else {
            None
        };
        let workspace = ChartWorkspace::with_gateway(&*gateway)
            .map(|mut workspace| {
                load_pine_catalog(&mut workspace, history);
                // The backtest form needs the served schema before it can
                // draft a run; a gateway that does not serve it (the mock)
                // leaves the run params over an empty document, and the
                // server names any missing field on submit.
                if let Ok(schema) = gateway.backtest_schema() {
                    workspace.set_backtest_schema(&schema.schema);
                }
                // Restore the persisted selection and per-ticker layouts over
                // the boot history; the active ticker's layout (timeframe,
                // range, chart type, loadout) applies on the switch below.
                let symbol = prefs.active_symbol.clone().or_else(|| prefs.symbol.clone());
                workspace.apply_prefs(&prefs).ok();
                if let Some(symbol) = symbol.filter(|s| !s.trim().is_empty()) {
                    let _ = workspace.switch_symbol(&symbol, &*gateway);
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
            cache,
            loaded_symbol: None,
            notice: None,
            prefs_path: path,
            saved: prefs,
        }
    }

    /// Serverless ticker switch ahead of the paint: when the modal picked a
    /// symbol the seeded frames do not hold, load its months through the
    /// cache and reseed, so the frame draws the picked ticker. No cache
    /// (loopback mode) means the gateway path already served the pick.
    fn maybe_switch_symbol(&mut self) {
        let (Ok(workspace), Some(cache)) = (&mut self.workspace, self.cache.as_mut()) else {
            return;
        };
        seed::sync_symbol(
            workspace,
            cache,
            &mut self.loaded_symbol,
            &mut self.notice,
        );
    }
}

impl eframe::App for TerminalApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Serverless ticker switch first: a modal pick the seeded frames do
        // not hold loads here, so the paint below draws the picked ticker.
        self.maybe_switch_symbol();
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
                // Gated persistence: nothing writes `protopyne.json` unless
                // the operator pressed the menu-bar Save Layout button, which
                // snapshots the active ticker's chart and indicator settings
                // over that ticker's previous entry.
                if workspace.take_save_request() {
                    let prefs = workspace.prefs();
                    if prefs != self.saved {
                        if let Ok(bytes) = serde_json::to_vec_pretty(&prefs) {
                            let _ = write_atomic(&self.prefs_path, &bytes);
                        }
                        self.saved = prefs;
                    }
                }
            }
            Err(reason) => {
                ui.colored_label(egui::Color32::RED, format!("workspace refused: {reason}"));
            }
        }
        if let Some(notice) = &self.notice {
            let ink = if notice.is_error() {
                egui::Color32::RED
            } else {
                egui::Color32::YELLOW
            };
            ui.colored_label(ink, notice.text());
        }
    }
}

/// Load `<data-root>/indicators/*.pine` plus libraries into the workspace
/// catalog. A missing tree or an unreadable file degrades to no Pine rows
/// (named on stderr) rather than refusing boot: Pine scripts are an
/// opt-in testing surface, never a boot dependency.
fn load_pine_catalog(workspace: &mut ChartWorkspace, history: &std::path::Path) {
    use protopyne_egui::pine_data_root;

    let Some(root) = pine_data_root(history) else {
        return;
    };
    match protopyne_egui::load_pine_sources(&root) {
        Ok((indicators, libraries)) => {
            // One boot line names what the catalog serves: a script added
            // to `indicators/` later needs a restart (sources load once at
            // boot), and this line is where that staleness shows up.
            let mut stems: Vec<&str> = indicators
                .iter()
                .map(|file| {
                    file.relpath
                        .rsplit('/')
                        .next()
                        .unwrap_or(&file.relpath)
                })
                .collect();
            stems.sort_unstable();
            eprintln!(
                "protopyne-terminal: pine catalog serves {} scripts: {}",
                stems.len(),
                stems.join(", ")
            );
            workspace.set_pine_sources(indicators, libraries);
        }
        Err(error) => eprintln!("protopyne-terminal: pine indicators unavailable: {error}"),
    }
}

fn main() {
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
    // The UI thread must stay outside any tokio runtime: the gateway bridges
    // synchronous paint calls onto runtimes (the shared bridge and scoped
    // blocking workers), and bridging from inside a runtime worker panics
    // ("Cannot start a runtime from within a runtime"). Boot the server on
    // an explicit runtime that outlives the window, and run eframe on the
    // runtime-free main thread. A `#[tokio::main]` here would park the paint
    // thread on a runtime worker and crash the first replay step or broker
    // read.
    let runtime: Option<tokio::runtime::Runtime> = if mock {
        None
    } else {
        Some(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|error| {
                    eprintln!("protopyne-terminal: runtime refused: {error}");
                    std::process::exit(1);
                }),
        )
    };
    let handle: Option<ServerHandle> = match &runtime {
        Some(runtime) => match runtime.block_on(boot(history_path, base)) {
            Ok(handle) => Some(handle),
            Err(e) => {
                eprintln!("protopyne-terminal: {e}");
                std::process::exit(1);
            }
        },
        None => None,
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
        let app = TerminalApp::new(gateway, feed, &prefs_source, mock);
        let run = eframe::run_native(
            "protopyne gateway terminal",
            eframe::NativeOptions::default(),
            Box::new(|cc| {
                protopyne_egui::install_noto_sans(
                    &cc.egui_ctx,
                    include_bytes!("../fonts/NotoSans-Regular.ttf"),
                    include_bytes!("../fonts/NotoSansMono-Regular.ttf"),
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Box::new(app))
            }),
        );
        if let Err(e) = run {
            eprintln!("protopyne-terminal: window refused: {e}");
            std::process::exit(1);
        }
    }
    if let (Some(handle), Some(runtime)) = (handle, &runtime) {
        runtime.block_on(handle.shutdown());
    }
}
