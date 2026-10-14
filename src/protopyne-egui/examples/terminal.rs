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
//! overlay) plus the replay scrub; chart painting stays in the `demo` shell,
//! which this file does not duplicate. No decision logic lives here: every
//! number on screen is a gateway read, and the only write (`arm`) is the
//! form's submit.
//!
//! Usage: `cargo run -p protopyne-egui --features server-gateway,demo
//! --example terminal -- [--mock] [HISTORY.csv]`, or pass `--check` to pull
//! every view through the trait object without opening a window.

use std::path::PathBuf;

use fluent_wvr::Describable;
use protopyne_egui::views::{HealthModel, PortfolioModel, ViewsBundle, load_bundle};
use protopyne_egui::{GrantFormState, ReplayScrub, ScrubOutcome, paint_grant_form, paint_replay_scrub};
use protopyne_server::{ServerConfig, ServerHandle, serve};
use protopyne_view::{
    ChartBars, GatewayError, MockGateway, OverlaySeries, TerminalGateway,
};

/// Overlay shown on the dashboard: same kernel and length the parity test
/// pins, so the example and the suite read the same view.
const OVERLAY_KIND: &str = "ema";
const OVERLAY_PERIOD: i64 = 10;

fn history_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical")
}

fn default_m15() -> PathBuf {
    history_dir().join("SPY_15M.csv")
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
            replay
                .window()
                .map_or(0, |window| window.steps.iter().filter(|step| step.checkpoint).count()),
        ),
    ]
}

async fn boot(path: PathBuf) -> Result<ServerHandle, String> {
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let bars = protopyne_server::parse_bars_csv(&text)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    if bars.is_empty() {
        return Err(format!("no bars in {}", path.display()));
    }
    let config = ServerConfig::new().bind("127.0.0.1:0".to_owned()).build();
    serve(config, bars).await.map_err(|e| format!("server refused to boot: {e}"))
}

struct TerminalApp {
    gateway: Box<dyn TerminalGateway>,
    bundle: Result<ViewsBundle, String>,
    charts: Result<ChartReads, String>,
    /// Journal rows pre-rendered in provenance order at refresh time: the
    /// frame reads lines, it never scans the journal per row per frame.
    journal_lines: Vec<String>,
    /// The replay scrub over the standard topology: the gateway supplies the
    /// window, this app owns where the cursor stands.
    replay: Option<ReplayScrub>,
    replay_error: Option<String>,
    /// The arming form over the patch descriptor: drafts, preview, and
    /// submit all run through the gateway, never around it.
    grant_form: GrantFormState,
}

impl TerminalApp {
    fn new(gateway: Box<dyn TerminalGateway>) -> Self {
        let (replay, replay_error) = match ReplayScrub::standard() {
            Ok(scrub) => (Some(scrub), None),
            // A topology that cannot be built is a refused scrub, reported in
            // the panel that would have hosted it rather than a dead frame.
            Err(e) => (None, Some(e.to_string())),
        };
        let mut app = Self {
            gateway,
            bundle: Err("no pull yet".to_owned()),
            charts: Err("no pull yet".to_owned()),
            journal_lines: Vec::new(),
            replay,
            replay_error,
            grant_form: GrantFormState::from_schema(
                &protopyne_strategy::GrantPatch::default().describe(),
            ),
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        // Uncached by design: every refresh reads fresh gateway state, so a
        // halted venue can never render as live from a stale bundle.
        self.bundle = load_bundle(&*self.gateway).map_err(|e| e.to_string());
        // One seq-to-row index per refresh, then one line per ordered row:
        // the per-frame journal walk is a line read, not a find per row.
        self.journal_lines = self.bundle.as_ref().map_or_else(
            |_| Vec::new(),
            |bundle| {
                let by_seq: std::collections::HashMap<i64, &protopyne_view::JournalRowView> =
                    bundle.journal.iter().map(|row| (row.seq, row)).collect();
                bundle
                    .provenance_order
                    .iter()
                    .filter_map(|seq| by_seq.get(seq))
                    .map(|row| format!("#{} {} {} {}", row.seq, row.client_id, row.kind, row.at_ms))
                    .collect()
            },
        );
        self.charts = pull_charts(&*self.gateway).map_err(|e| e.to_string());
        // A repeated read widens the window; it never moves the operator's
        // cursor, so a refresh cannot yank a scrub back to the head.
        if let Some(scrub) = self.replay.as_mut() {
            self.replay_error = scrub.ingest(&*self.gateway).err().map(|e| e.to_string());
        }
    }
}

impl eframe::App for TerminalApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("gateway-header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("gateway terminal");
                if ui.button("refresh").clicked() {
                    self.refresh();
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let bundle = match &self.bundle {
                Ok(bundle) => bundle,
                Err(reason) => {
                    ui.colored_label(egui::Color32::RED, format!("gateway refused: {reason}"));
                    return;
                }
            };
            ui.collapsing("health", |ui| {
                let health = &bundle.health;
                ui.label(health_summary_line(health));
                if health.halted() {
                    ui.colored_label(egui::Color32::RED, "HALTED: no entry affordance");
                }
                ui.label(format!(
                    "entry affordance: {} exit affordance: {}",
                    health.entry_affordance(),
                    health.exit_affordance(),
                ));
                match &health.last_command {
                    Some(command) => {
                        ui.label(format!(
                            "last command: {} ({})",
                            command.kind, command.detail
                        ));
                    }
                    None => {
                        ui.label("last command: none");
                    }
                }
            });
            ui.collapsing(
                format!("portfolio ({} positions)", bundle.portfolio.positions.len()),
                |ui| {
                    if bundle.portfolio.is_empty() {
                        ui.label("empty: no positions, no fills");
                    }
                    for position in &bundle.portfolio.positions {
                        ui.label(format!(
                            "{} qty={} avg={}",
                            position.underlying, position.quantity, position.avg_price,
                        ));
                    }
                    ui.label(portfolio_summary_line(&bundle.portfolio));
                },
            );
            ui.collapsing(format!("proposals ({})", bundle.proposals.len()), |ui| {
                for proposal in &bundle.proposals {
                    ui.label(format!(
                        "{} {} {} {} {} limit={} conviction={:.3} state={}",
                        proposal.id,
                        proposal.strategy,
                        proposal.underlying,
                        proposal.direction,
                        proposal.structure,
                        proposal.limit_price,
                        proposal.conviction,
                        proposal.state,
                    ));
                }
            });
            match &self.charts {
                Ok(charts) => {
                    ui.collapsing(
                        format!("chart ({} bars, {} dropped)", charts.bars.rows.len(), charts.bars.dropped),
                        |ui| {
                            if let (Some(first), Some(last)) =
                                (charts.bars.rows.first(), charts.bars.rows.last())
                            {
                                ui.label(format!(
                                    "first={} last={} last_close={:.2}",
                                    first.time, last.time, last.close,
                                ));
                            }
                            let overlay = &charts.overlay;
                            let defined = overlay.values.iter().flatten().count();
                            ui.label(format!(
                                "overlay {}/{}: {defined}/{} defined",
                                overlay.kind,
                                overlay.period,
                                overlay.values.len(),
                            ));
                        },
                    );
                }
                Err(reason) => {
                    ui.colored_label(egui::Color32::RED, format!("chart reads refused: {reason}"));
                }
            }
            egui::Panel::right("replay-scrub").show(ui, |ui| {
                if let Some(reason) = self.replay_error.clone() {
                    ui.colored_label(egui::Color32::RED, format!("replay refused: {reason}"));
                }
                if let Some(scrub) = self.replay.as_mut() {
                    if let Some(ScrubOutcome::Rewound { panes, .. }) =
                        paint_replay_scrub(ui, scrub)
                    {
                    // The panes a move made due are this terminal's own
                    // recompute work list. The dashboard lists them: pane
                    // series belong to the chart shell, which owns the
                    // kernels this file deliberately does not duplicate.
                        ui.separator();
                        ui.label(format!("{} panes to recompute:", panes.len()));
                        for pane in &panes {
                            ui.label(format!("  {pane}"));
                        }
                    }
                }
            });
            ui.collapsing(format!("journal ({})", bundle.journal.len()), |ui| {
                // Provenance order, not arrival order: the rows were ordered
                // and rendered at refresh time, so the frame only reads.
                for line in &self.journal_lines {
                    ui.label(line);
                }
            });
            egui::Panel::left("grant-arm").show(ui, |ui| {
                paint_grant_form(ui, &mut self.grant_form, &*self.gateway);
            });
        });
    }
}

#[tokio::main]
async fn main() {
    let mut path: Option<PathBuf> = None;
    let mut check = false;
    let mut mock = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--check" => check = true,
            "--mock" => mock = true,
            _ => path = Some(PathBuf::from(arg)),
        }
    }
    let handle: Option<ServerHandle> = if mock {
        None
    } else {
        match boot(path.unwrap_or_else(default_m15)).await {
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
                scrub.ingest(&*gateway).map(|()| scrub).map_err(|e| e.to_string())
            });
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
        let app = TerminalApp::new(gateway);
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
