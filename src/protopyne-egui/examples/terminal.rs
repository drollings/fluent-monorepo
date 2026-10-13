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
//! overlay); chart painting stays in the `demo` shell, which this file does
//! not duplicate. No decision logic lives here: every number on screen is a
//! gateway read, and the single write (`arm`) is a later milestone's form.
//!
//! Usage: `cargo run -p protopyne-egui --features server-gateway,demo
//! --example terminal -- [--mock] [HISTORY.csv]`, or pass `--check` to pull
//! every view through the trait object without opening a window.

use std::path::PathBuf;

use protopyne_egui::views::{ViewCache, ViewsBundle};
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

/// Chart reads have no view-model yet (chart views arrive in a later
/// milestone); the terminal pulls them alongside the bundle, never by
/// reaching past the trait object.
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

fn check_lines(bundle: &ViewsBundle, charts: &ChartReads) -> Vec<String> {
    let health = &bundle.health;
    let portfolio = &bundle.portfolio;
    let bars = &charts.bars;
    let last_close = bars.rows.last().map_or(f64::NAN, |bar| bar.close);
    let overlay_last = charts.overlay.values.iter().rev().find_map(|slot| *slot);
    vec![
        format!(
            "status={} version={} trading={} entries={} exits={}",
            health.status,
            health.version,
            health.trading_state,
            health.allows_entries,
            health.allows_exits,
        ),
        format!(
            "proposals={} positions={} filled={} fees={} open={} journal={}",
            bundle.proposals.len(),
            portfolio.positions.len(),
            portfolio.filled_orders,
            portfolio.total_fees,
            portfolio.open_contracts,
            bundle.journal.len(),
        ),
        format!(
            "bars={} dropped={} last_close={last_close:.2} overlay={}/{}(last={overlay_last:.2?})",
            bars.rows.len(),
            bars.dropped,
            OVERLAY_KIND,
            OVERLAY_PERIOD,
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
    cache: ViewCache,
    bundle: Result<ViewsBundle, String>,
    charts: Result<ChartReads, String>,
}

impl TerminalApp {
    fn new(gateway: Box<dyn TerminalGateway>) -> Self {
        let mut app = Self {
            gateway,
            cache: ViewCache::new(),
            bundle: Err("no pull yet".to_owned()),
            charts: Err("no pull yet".to_owned()),
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.bundle = self.cache.load(&*self.gateway).map_err(|e| e.to_string());
        self.charts = pull_charts(&*self.gateway).map_err(|e| e.to_string());
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
                ui.label(format!(
                    "status={} version={} trading={} entries={} exits={}",
                    health.status,
                    health.version,
                    health.trading_state,
                    health.allows_entries,
                    health.allows_exits,
                ));
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
                    ui.label(format!(
                        "filled={} fees={} open={}",
                        bundle.portfolio.filled_orders,
                        bundle.portfolio.total_fees,
                        bundle.portfolio.open_contracts,
                    ));
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
            ui.collapsing(format!("journal ({})", bundle.journal.len()), |ui| {
                // Provenance order, not arrival order: the timeline is the
                // journal's own causal chains.
                for seq in &bundle.provenance_order {
                    if let Some(row) =
                        bundle.journal.iter().find(|row| row.seq == *seq)
                    {
                        ui.label(format!(
                            "#{} {} {} {}",
                            row.seq, row.client_id, row.kind, row.at_ms
                        ));
                    }
                }
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
        let cache = ViewCache::new();
        match (cache.load(&*gateway), pull_charts(&*gateway)) {
            (Ok(bundle), Ok(charts)) => {
                for line in check_lines(&bundle, &charts) {
                    println!("protopyne-terminal check: {line}");
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("protopyne-terminal: gateway refused: {e}");
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
